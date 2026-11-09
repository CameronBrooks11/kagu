//! One run, driven by its actor: the only writer of the run's events
//! (Invariant 2). Each state change is appended before it takes effect
//! (Invariant 1).

use std::collections::BTreeMap;
use std::io::Read;
use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::acp::{Conn, Msg};
use crate::event::*;
use crate::profile;
use crate::store::Store;

/// ACP error code for "authentication required".
const AUTH_REQUIRED: i64 = -32000;
/// How long a worker may take to exit once its stdin closes, and how long
/// output may trail an exit.
const GRACE: Duration = Duration::from_secs(2);

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
    worker: Option<Conn>,
    exit: Option<ExitStatus>,
}

impl Actor<'_> {
    fn log<P: Payload>(&mut self, source: Source, payload: &P) -> Result<(), String> {
        let data = serde_json::to_value(payload).expect("payload serializes");
        let event = self
            .store
            .append_value(&self.id, source, P::TYPE, P::V, data)
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

    fn redact_str(&self, mut s: String) -> String {
        for (secret, reference) in &self.secrets {
            if s.contains(secret.as_str()) {
                s = s.replace(secret.as_str(), reference);
            }
        }
        s
    }

    /// Replace injected credential values in worker data, keys included.
    fn redact(&self, value: Value) -> Value {
        match value {
            Value::String(s) => Value::String(self.redact_str(s)),
            Value::Array(a) => Value::Array(a.into_iter().map(|v| self.redact(v)).collect()),
            Value::Object(o) => Value::Object(
                o.into_iter()
                    .map(|(k, v)| (self.redact_str(k), self.redact(v)))
                    .collect(),
            ),
            other => other,
        }
    }

    /// Redact a raw line as it arrives (Invariant 13). Text that parses as
    /// JSON is redacted as JSON, before escaping can hide a value. Protocol
    /// messages pass unchanged: kagu must read the real message, and redacts
    /// each part of it that it stores.
    fn clean(&self, msg: Msg) -> Msg {
        if self.secrets.is_empty() {
            return msg;
        }
        match msg {
            Msg::Raw { stream, text } => {
                let text = match serde_json::from_str::<Value>(&text) {
                    Ok(v) => {
                        let red = self.redact(v.clone());
                        if red == v { text } else { red.to_string() }
                    }
                    Err(_) => self.redact_str(text),
                };
                Msg::Raw { stream, text }
            }
            other => other,
        }
    }

    fn raw(&mut self, stream: &str, text: String) -> Result<(), String> {
        self.log(
            Source::Worker,
            &WorkerRaw {
                stream: stream.into(),
                text,
            },
        )
    }

    fn raw_json(&mut self, msg: Value) -> Result<(), String> {
        let text = self.redact(msg).to_string();
        self.raw("stdout", text)
    }

    /// Send a request and handle everything the worker sends until its response.
    fn call(&mut self, method: &str, params: Value) -> Result<Result<Value, CallError>, String> {
        let conn = self.worker.as_mut().expect("worker started");
        let Ok(id) = conn.request(method, params) else {
            return Ok(Err(CallError::Exited));
        };
        // Once the worker has exited, a response may still trail it briefly.
        let mut deadline: Option<Instant> = self.exit.map(|_| Instant::now() + GRACE);
        loop {
            let conn = self.worker.as_ref().expect("worker started");
            let msg = match deadline {
                Some(d) => match conn.recv_until(d) {
                    Some(m) => m,
                    None => return Ok(Err(CallError::Exited)),
                },
                None => conn.recv(),
            };
            match self.clean(msg) {
                Msg::Closed => return Ok(Err(CallError::Exited)),
                Msg::Exited(status) => {
                    self.exit = Some(status);
                    deadline = Some(Instant::now() + GRACE);
                }
                Msg::Raw { stream, text } => self.raw(stream, text)?,
                Msg::Rpc(msg) => {
                    let ours = msg.get("method").is_none() && msg.get("id") == Some(&json!(id));
                    if !ours {
                        self.incoming(msg)?;
                        continue;
                    }
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
            }
        }
    }

    /// A notification or request from the worker that is not our response.
    fn incoming(&mut self, msg: Value) -> Result<(), String> {
        let method = msg
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let id = msg.get("id").cloned();
        if method == "session/update"
            && id.is_none()
            && let Some(update) = msg.get("params").and_then(|p| p.get("update"))
        {
            let update = self.redact(update.clone());
            return self.log(Source::Worker, &WorkerUpdate(update));
        }
        let options = msg["params"]["options"].clone();
        self.raw_json(msg)?;
        let Some(id) = id.filter(|_| !method.is_empty()) else {
            return Ok(());
        };
        // A write to a closed stdin surfaces as Closed or Exited on the next receive.
        if method == "session/request_permission" {
            // No policy and no clients yet: refuse.
            let option_id = options
                .as_array()
                .and_then(|opts| opts.iter().find(|o| o["kind"] == "reject_once"))
                .and_then(|o| o.get("optionId"))
                .and_then(Value::as_str)
                .map(str::to_string);
            self.log(
                Source::Host,
                &PermissionDecided {
                    option_id: option_id.clone(),
                },
            )?;
            let outcome = match option_id {
                Some(o) => json!({"outcome": "selected", "optionId": o}),
                None => json!({"outcome": "cancelled"}),
            };
            let conn = self.worker.as_mut().expect("worker started");
            let _ = conn.respond(id, json!({ "outcome": outcome }));
        } else {
            let conn = self.worker.as_mut().expect("worker started");
            let _ = conn.respond_error(id, -32601, "method not found");
        }
        Ok(())
    }

    fn fail(&mut self, err: CallError) -> Result<(), String> {
        let (reason, detail) = err.outcome();
        let detail = detail.map(|d| self.redact_str(d));
        self.to(State::Failed, Some(reason), detail)
    }

    fn reject(&mut self, detail: String) -> Result<(), String> {
        self.to(State::Rejected, Some(Reason::Error), Some(detail))
    }

    /// Admission through the end of the prompt.
    fn drive(&mut self, opts: &Options, workspace: &str) -> Result<(), String> {
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
            Err(e) => return self.reject(e),
        };
        let worker = &loaded.profile.worker;
        let mut env = Vec::new();
        for (name, reference) in &worker.env {
            let var = profile::reference_var(reference).expect("validated at load");
            let Ok(value) = std::env::var(var) else {
                return self.reject(format!("credential reference {reference} is unset"));
            };
            if !value.is_empty() {
                self.secrets.push((value.clone(), reference.clone()));
            }
            env.push((name.clone(), value));
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
                profile: opts.profile.display().to_string(),
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
        let child = match spawned {
            Ok(c) => c,
            Err(e) => return self.reject(format!("spawn {}: {e}", worker.command[0])),
        };
        let conn = Conn::attach(child);
        let pid = conn.pid();
        self.worker = Some(conn);
        self.log(Source::Host, &WorkerSpawned { pid })?;
        self.to(State::Working, None, None)?;

        let init = json!({"protocolVersion": 1, "clientCapabilities": {}});
        let result = match self.call("initialize", init)? {
            Ok(r) => r,
            Err(e) => return self.fail(e),
        };
        let version = result
            .get("protocolVersion")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        self.log(
            Source::Worker,
            &WorkerInitialized {
                protocol_version: version,
                agent_info: result.get("agentInfo").cloned().map(|v| self.redact(v)),
            },
        )?;
        if version != 1 {
            let detail = format!("worker speaks ACP protocol version {version}, not 1");
            return self.to(State::Failed, Some(Reason::Error), Some(detail));
        }

        let new = json!({"cwd": workspace, "mcpServers": []});
        let session = match self.call("session/new", new)? {
            Ok(r) => r
                .get("sessionId")
                .and_then(Value::as_str)
                .map(str::to_string),
            Err(e) => return self.fail(e),
        };
        let Some(session_id) = session else {
            let detail = "session/new returned no sessionId".to_string();
            return self.to(State::Failed, Some(Reason::Error), Some(detail));
        };
        self.log(
            Source::Worker,
            &WorkerSession {
                session_id: self.redact_str(session_id.clone()),
            },
        )?;

        let prompt =
            json!({"sessionId": session_id, "prompt": [{"type": "text", "text": opts.input}]});
        match self.call("session/prompt", prompt)? {
            Ok(r) => {
                let reason = r.get("stopReason").and_then(Value::as_str).unwrap_or("");
                let (state, why) = stop(reason);
                let detail = (why == Some(Reason::UnknownStopReason))
                    .then(|| format!("stopReason {:?}", self.redact_str(reason.to_string())));
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
            self.to(State::Completed, None, None)?;
        }
        Ok(())
    }

    /// End the conversation, give the worker a grace period, kill it if it
    /// stays, and record the exit.
    fn stop_worker(&mut self) -> Result<(), String> {
        let Some(mut conn) = self.worker.take() else {
            return Ok(());
        };
        conn.close();
        let mut killed = false;
        let mut deadline = Instant::now() + GRACE;
        while self.exit.is_none() {
            match conn.recv_until(deadline).map(|m| self.clean(m)) {
                Some(Msg::Exited(s)) => self.exit = Some(s),
                Some(Msg::Raw { stream, text }) => self.raw(stream, text)?,
                Some(Msg::Rpc(msg)) => self.raw_json(msg)?,
                Some(Msg::Closed) => {}
                None if !killed => {
                    conn.kill();
                    killed = true;
                    deadline = Instant::now() + GRACE;
                }
                None => return Err(format!("worker {} did not exit after kill", conn.pid())),
            }
        }
        // Output that trails the exit, bounded: a descendant may hold stdout open.
        let deadline = Instant::now() + GRACE;
        while let Some(msg) = conn.recv_until(deadline).map(|m| self.clean(m)) {
            match msg {
                Msg::Closed => break,
                Msg::Raw { stream, text } => self.raw(stream, text)?,
                Msg::Rpc(msg) => self.raw_json(msg)?,
                Msg::Exited(_) => {}
            }
        }
        let status = self.exit.expect("exit recorded");
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
        worker: None,
        exit: None,
    };
    let driven = actor.drive(opts, &workspace);
    // The worker is stopped whatever happened, so none outlives its run.
    let stopped = actor.stop_worker();
    driven?;
    stopped?;
    Ok((actor.id, actor.view))
}
