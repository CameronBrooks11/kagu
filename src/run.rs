//! One run, driven by its actor: the only writer of the run's events
//! (Invariant 2). Each state change is appended before it takes effect
//! (Invariant 1).

use std::collections::BTreeMap;
use std::io::Read;
use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::acp::{Conn, Msg};
use crate::event::*;
use crate::profile;
use crate::store::Store;

/// ACP error code for "authentication required".
const AUTH_REQUIRED: i64 = -32000;
/// How long a worker may take to exit after its stdin closes.
const EXIT_GRACE: Duration = Duration::from_secs(2);

pub struct Options {
    pub profile: PathBuf,
    pub workspace: PathBuf,
    pub input: String,
}

enum CallError {
    Rpc { code: i64, message: String },
    Exited,
}

impl CallError {
    fn outcome(&self) -> (Reason, Option<String>) {
        match self {
            Self::Rpc { code, .. } if *code == AUTH_REQUIRED => (Reason::AuthRequired, None),
            Self::Rpc { code, message } => (Reason::Error, Some(format!("{code}: {message}"))),
            Self::Exited => (Reason::Exited, None),
        }
    }
}

/// The state a `session/prompt` stop reason leads to (spec: Run lifecycle).
fn stop(reason: &str) -> (State, Option<Reason>) {
    match reason {
        "end_turn" => (State::Checking, None),
        // kagu has not asked to cancel, so the worker cancelled on its own.
        "cancelled" => (State::Failed, Some(Reason::UnrequestedCancel)),
        "max_tokens" | "max_turn_requests" => (State::Failed, Some(Reason::Limit)),
        "refusal" => (State::Failed, Some(Reason::Refusal)),
        _ => (State::Failed, Some(Reason::UnknownStopReason)),
    }
}

struct Actor<'a> {
    id: String,
    store: &'a Store,
    emit: &'a mut dyn FnMut(&Event),
    /// Injected credential values and the references that replace them.
    secrets: Vec<(String, String)>,
    view: RunView,
}

impl Actor<'_> {
    fn log<P: Payload>(&mut self, source: Source, payload: &P) -> Result<(), String> {
        let event = self
            .store
            .append(&self.id, source, payload)
            .map_err(|e| format!("event log: {e}"))?;
        (self.emit)(&event);
        Ok(())
    }

    fn to(
        &mut self,
        to: State,
        reason: Option<Reason>,
        detail: Option<String>,
    ) -> Result<(), String> {
        self.log(Source::Host, &RunState { to, reason, detail })?;
        self.view.state = Some(to);
        self.view.reason = reason;
        Ok(())
    }

    fn redact(&self, value: Value) -> Value {
        match value {
            Value::String(s) => Value::String(self.redact_str(s)),
            Value::Array(a) => Value::Array(a.into_iter().map(|v| self.redact(v)).collect()),
            Value::Object(o) => {
                Value::Object(o.into_iter().map(|(k, v)| (k, self.redact(v))).collect())
            }
            other => other,
        }
    }

    fn redact_str(&self, mut s: String) -> String {
        for (value, reference) in &self.secrets {
            if s.contains(value.as_str()) {
                s = s.replace(value.as_str(), reference);
            }
        }
        s
    }

    fn raw(&mut self, stream: &str, text: String) -> Result<(), String> {
        let text = self.redact_str(text);
        self.log(
            Source::Worker,
            &WorkerRaw {
                stream: stream.into(),
                text,
            },
        )
    }

    /// Send a request and handle everything the worker sends until its response.
    fn call(
        &mut self,
        conn: &mut Conn,
        method: &str,
        params: Value,
    ) -> Result<Result<Value, CallError>, String> {
        let Ok(id) = conn.request(method, params) else {
            return Ok(Err(CallError::Exited));
        };
        loop {
            match conn.recv() {
                Msg::Closed => return Ok(Err(CallError::Exited)),
                Msg::Raw { stream, text } => self.raw(stream, text)?,
                Msg::Rpc(msg) => {
                    let is_ours = msg.get("method").is_none() && msg.get("id") == Some(&json!(id));
                    if is_ours {
                        if let Some(err) = msg.get("error") {
                            return Ok(Err(CallError::Rpc {
                                code: err.get("code").and_then(Value::as_i64).unwrap_or(0),
                                message: err
                                    .get("message")
                                    .and_then(Value::as_str)
                                    .unwrap_or("")
                                    .into(),
                            }));
                        }
                        return Ok(Ok(msg.get("result").cloned().unwrap_or(Value::Null)));
                    }
                    self.incoming(conn, msg)?;
                }
            }
        }
    }

    /// A notification or request from the worker that is not our response.
    fn incoming(&mut self, conn: &mut Conn, msg: Value) -> Result<(), String> {
        let method = msg
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if method == "session/update"
            && msg.get("id").is_none()
            && let Some(update) = msg.get("params").and_then(|p| p.get("update"))
        {
            let update = self.redact(update.clone());
            return self.log(Source::Worker, &WorkerUpdate(update));
        }
        self.raw("stdout", msg.to_string())?;
        if let Some(id) = msg.get("id").cloned().filter(|_| !method.is_empty()) {
            // No policy and no clients yet: refuse permission, decline the rest.
            let sent = if method == "session/request_permission" {
                let reject = msg["params"]["options"]
                    .as_array()
                    .and_then(|opts| opts.iter().find(|o| o["kind"] == "reject_once"))
                    .and_then(|o| o.get("optionId").cloned());
                let outcome = match reject {
                    Some(option_id) => json!({"outcome": "selected", "optionId": option_id}),
                    None => json!({"outcome": "cancelled"}),
                };
                conn.respond(id, json!({ "outcome": outcome }))
            } else {
                conn.respond_error(id, -32601, "method not found")
            };
            // A closed stdin surfaces as Closed on the next receive.
            drop(sent);
        }
        Ok(())
    }

    fn fail(&mut self, err: CallError) -> Result<(), String> {
        let (reason, detail) = err.outcome();
        self.to(State::Failed, Some(reason), detail)
    }

    /// Admission through the end of the prompt. Returns the worker if one started.
    fn drive(&mut self, opts: &Options, workspace: &str) -> Result<Option<(Child, Conn)>, String> {
        self.log(
            Source::Client,
            &TaskSubmitted {
                profile: opts.profile.display().to_string(),
                workspace: workspace.to_string(),
                input: opts.input.clone(),
                thread: None,
            },
        )?;
        self.to(State::Submitted, None, None)?;

        let loaded = match profile::load(&opts.profile) {
            Ok(l) => l,
            Err(e) => {
                self.to(State::Rejected, Some(Reason::Error), Some(e))?;
                return Ok(None);
            }
        };
        let worker = &loaded.profile.worker;
        let mut env = Vec::new();
        for (name, reference) in &worker.env {
            let var = profile::reference_var(reference).expect("validated at load");
            match std::env::var(var) {
                Ok(value) => {
                    if !value.is_empty() {
                        self.secrets.push((value.clone(), reference.clone()));
                    }
                    env.push((name.clone(), value));
                }
                Err(_) => {
                    let detail = format!("credential reference {reference} is unset");
                    self.to(State::Rejected, Some(Reason::Error), Some(detail))?;
                    return Ok(None);
                }
            }
        }
        // No substrate yet: nothing is confined (spec: Grant).
        let levels = BTreeMap::from([
            ("credentials".to_string(), Level::Unsupported),
            ("filesystem".to_string(), Level::Unsupported),
            ("network".to_string(), Level::Unrestricted),
            ("process_tree".to_string(), Level::Unsupported),
        ]);
        self.log(
            Source::Host,
            &RunAdmitted {
                profile_digest: loaded.digest.clone(),
                command: worker.command.clone(),
                env: worker.env.clone(),
                substrate: "none".into(),
                levels,
            },
        )?;

        let spawned = Command::new(&worker.command[0])
            .args(&worker.command[1..])
            .current_dir(workspace)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .envs(env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();
        let mut child = match spawned {
            Ok(c) => c,
            Err(e) => {
                let detail = format!("spawn {}: {e}", worker.command[0]);
                self.to(State::Rejected, Some(Reason::Error), Some(detail))?;
                return Ok(None);
            }
        };
        let mut conn = Conn::attach(&mut child);
        self.log(Source::Host, &WorkerSpawned { pid: child.id() })?;
        self.to(State::Working, None, None)?;

        let init = json!({"protocolVersion": 1, "clientCapabilities": {}});
        let result = match self.call(&mut conn, "initialize", init)? {
            Ok(r) => r,
            Err(e) => {
                self.fail(e)?;
                return Ok(Some((child, conn)));
            }
        };
        let version = result
            .get("protocolVersion")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        self.log(
            Source::Worker,
            &WorkerInitialized {
                protocol_version: version,
                agent_info: result.get("agentInfo").cloned(),
            },
        )?;
        if version != 1 {
            let detail = format!("worker speaks ACP protocol version {version}, not 1");
            self.to(State::Failed, Some(Reason::Error), Some(detail))?;
            return Ok(Some((child, conn)));
        }

        let new = json!({"cwd": workspace, "mcpServers": []});
        let session = match self.call(&mut conn, "session/new", new)? {
            Ok(r) => r
                .get("sessionId")
                .and_then(Value::as_str)
                .map(str::to_string),
            Err(e) => {
                self.fail(e)?;
                return Ok(Some((child, conn)));
            }
        };
        let Some(session_id) = session else {
            self.to(
                State::Failed,
                Some(Reason::Error),
                Some("session/new returned no sessionId".into()),
            )?;
            return Ok(Some((child, conn)));
        };
        self.log(
            Source::Worker,
            &WorkerSession {
                session_id: session_id.clone(),
            },
        )?;

        let prompt =
            json!({"sessionId": session_id, "prompt": [{"type": "text", "text": opts.input}]});
        match self.call(&mut conn, "session/prompt", prompt)? {
            Ok(r) => {
                let reason = r.get("stopReason").and_then(Value::as_str).unwrap_or("");
                let (state, why) = stop(reason);
                let detail = (why == Some(Reason::UnknownStopReason))
                    .then(|| format!("stopReason {reason:?}"));
                self.to(state, why, detail)?;
            }
            Err(e) => self.fail(e)?,
        }
        if self.view.state == Some(State::Checking) {
            // No checks yet (#9): the result is not verified.
            self.log(
                Source::Host,
                &RunVerdict {
                    verdict: Verdict::NotRun,
                },
            )?;
            self.view.verdict = Verdict::NotRun;
            self.to(State::Completed, None, None)?;
        }
        Ok(Some((child, conn)))
    }

    /// End the conversation, give the worker a grace period, then kill it.
    fn stop_worker(&mut self, mut child: Child, mut conn: Conn) -> Result<(), String> {
        conn.close();
        let deadline = Instant::now() + EXIT_GRACE;
        let status = loop {
            match child.try_wait() {
                Ok(Some(s)) => break s,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20))
                }
                _ => {
                    let _ = child.kill();
                    break child.wait().map_err(|e| format!("wait: {e}"))?;
                }
            }
        };
        // Whatever the worker still had buffered. A descendant holding stdout
        // open must not hold the run open, so this is bounded too.
        let deadline = Instant::now() + EXIT_GRACE;
        while let Some(msg) = conn.recv_until(deadline) {
            match msg {
                Msg::Closed => break,
                Msg::Raw { stream, text } => self.raw(stream, text)?,
                Msg::Rpc(msg) => self.raw("stdout", msg.to_string())?,
            }
        }
        self.log(
            Source::Host,
            &WorkerExited {
                code: status.code(),
                signal: status.signal(),
            },
        )
    }
}

fn new_run_id() -> String {
    let mut bytes = [0u8; 8];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .expect("read /dev/urandom");
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!("r_{hex}")
}

/// Run one task to a terminal state, emitting each event as it is appended.
pub fn run(
    store: &Store,
    opts: &Options,
    emit: &mut dyn FnMut(&Event),
) -> Result<(String, RunView), String> {
    let workspace = std::fs::canonicalize(&opts.workspace)
        .map_err(|e| format!("workspace {}: {e}", opts.workspace.display()))?;
    if !workspace.is_dir() {
        return Err(format!(
            "workspace {} is not a directory",
            workspace.display()
        ));
    }
    let workspace = workspace
        .to_str()
        .ok_or("workspace path is not UTF-8")?
        .to_string();
    let mut actor = Actor {
        id: new_run_id(),
        store,
        emit,
        secrets: Vec::new(),
        view: RunView {
            state: None,
            reason: None,
            verdict: Verdict::NotRun,
        },
    };
    if let Some((child, conn)) = actor.drive(opts, &workspace)? {
        actor.stop_worker(child, conn)?;
    }
    Ok((actor.id, actor.view))
}
