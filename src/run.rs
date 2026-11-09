//! One run, driven by its actor: the only writer of the run's events
//! (Invariant 2). Each state change is appended before it takes effect
//! (Invariant 1).

use std::io::Read;
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::{ExitStatus, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::SyncSender;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::acp::{Conn, Msg};
use crate::event::*;
use crate::log::Log;
use crate::profile::{self, Substrate};
use crate::substrate;

/// ACP error code for "authentication required".
const AUTH_REQUIRED: i64 = -32000;
/// How long a worker may take to exit once its stdin closes, how long output
/// may trail an exit, and how long a cancelled worker has to stop.
pub const GRACE: Duration = Duration::from_secs(2);

/// A request to cancel a run, shared between the host and the run's actor.
/// The flag decides; the waker only interrupts a wait.
#[derive(Default)]
pub struct Cancel {
    flag: AtomicBool,
    waker: Mutex<Option<SyncSender<Msg>>>,
}

impl Cancel {
    pub fn request(&self) {
        self.flag.store(true, Ordering::SeqCst);
        if let Some(w) = self.waker.lock().expect("waker lock").as_ref() {
            let _ = w.try_send(Msg::Cancel);
        }
    }

    fn requested(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }
}

enum CallError {
    Rpc { code: i64, message: String },
    Exited,
    Canceled,
}

impl CallError {
    fn outcome(&self) -> (Reason, Option<String>) {
        match self {
            Self::Rpc { code, .. } if *code == AUTH_REQUIRED => (Reason::AuthRequired, None),
            Self::Rpc { code, message } => (Reason::Error, Some(format!("{code}: {message}"))),
            Self::Exited | Self::Canceled => (Reason::Exited, None),
        }
    }
}

/// The state a `session/prompt` stop reason leads to (spec: Run lifecycle).
fn stop(reason: &str) -> (State, Option<Reason>) {
    match reason {
        "end_turn" => (State::Checking, None),
        // Reached only when kagu has not asked to cancel.
        "cancelled" => (State::Failed, Some(Reason::UnrequestedCancel)),
        "max_tokens" | "max_turn_requests" => (State::Failed, Some(Reason::Limit)),
        "refusal" => (State::Failed, Some(Reason::Refusal)),
        _ => (State::Failed, Some(Reason::UnknownStopReason)),
    }
}

/// How a sandbox's start ended.
enum Sandbox {
    Ready,
    Failed(String),
    Canceled,
}

struct Actor<'a> {
    id: String,
    log: &'a Log,
    cancel: &'a Cancel,
    /// Injected credential values and the references that replace them.
    secrets: Vec<(String, String)>,
    view: RunView,
    worker: Option<Conn>,
    exit: Option<ExitStatus>,
    session: Option<String>,
    /// Set once a cancel has been sent; the worker must stop by then.
    cancel_deadline: Option<Instant>,
    /// Whether permission requests are answered "allow": only when the
    /// filesystem level is `enforced`, so the sandbox is the boundary.
    allow: bool,
    /// Which of the worker's output streams have ended.
    stdout_closed: bool,
    stderr_closed: bool,
}

impl Actor<'_> {
    fn log<P: Payload>(&mut self, source: Source, payload: &P) -> Result<(), String> {
        let data = serde_json::to_value(payload).expect("payload serializes");
        self.log.append(&self.id, source, P::TYPE, P::V, data)?;
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
                        if red == v {
                            // Numbers are not redacted as JSON; the text still is.
                            self.redact_str(text)
                        } else {
                            red.to_string()
                        }
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

    /// Start cancelling, once: ask the worker to stop and set its deadline.
    fn begin_cancel(&mut self) {
        if self.cancel_deadline.is_some() || !self.cancel.requested() {
            return;
        }
        self.cancel_deadline = Some(Instant::now() + GRACE);
        if let (Some(conn), Some(session)) = (self.worker.as_mut(), self.session.clone()) {
            // A write to a closed stdin surfaces as Closed or Exited later.
            let _ = conn.notify("session/cancel", json!({ "sessionId": session }));
        }
    }

    /// Send a request and handle everything the worker sends until its response.
    fn call(&mut self, method: &str, params: Value) -> Result<Result<Value, CallError>, String> {
        let conn = self.worker.as_mut().expect("worker started");
        let Ok(id) = conn.request(method, params) else {
            return Ok(Err(CallError::Exited));
        };
        // Once the worker has exited, a response may still trail it briefly.
        let mut exit_deadline: Option<Instant> = self.exit.map(|_| Instant::now() + GRACE);
        loop {
            self.begin_cancel();
            let deadline = match (exit_deadline, self.cancel_deadline) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
            let conn = self.worker.as_ref().expect("worker started");
            let msg = match deadline {
                Some(d) => match conn.recv_until(d) {
                    Some(m) => m,
                    None if self.cancel_deadline.is_some_and(|c| Instant::now() >= c) => {
                        conn.kill();
                        return Ok(Err(CallError::Canceled));
                    }
                    None => return Ok(Err(CallError::Exited)),
                },
                None => conn.recv(),
            };
            match self.clean(msg) {
                Msg::Cancel => {}
                Msg::ErrClosed => self.stderr_closed = true,
                Msg::Closed => {
                    self.stdout_closed = true;
                    return Ok(Err(if self.cancel_deadline.is_some() {
                        CallError::Canceled
                    } else {
                        CallError::Exited
                    }));
                }
                Msg::Exited(status) => {
                    self.exit = Some(status);
                    exit_deadline = Some(Instant::now() + GRACE);
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
        if method == "session/request_permission" {
            // Answered at once, so none is pending when a cancel arrives.
            // Allowed inside an enforced sandbox, which is the real boundary;
            // refused otherwise.
            let kind = if self.allow {
                "allow_once"
            } else {
                "reject_once"
            };
            let option_id = options
                .as_array()
                .and_then(|opts| opts.iter().find(|o| o["kind"] == kind))
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

    /// Wait for `sandbox-init` to report the sandbox built. Until then the
    /// worker has not started, so a failure here rejects the run.
    fn await_sandbox(&mut self) -> Result<Sandbox, String> {
        let deadline = Instant::now() + GRACE * 5;
        let mut said = Vec::new();
        loop {
            if self.cancel.requested() {
                return Ok(Sandbox::Canceled);
            }
            let conn = self.worker.as_ref().expect("worker started");
            // Matched before redaction, which a short secret could alter.
            let msg = match conn.recv_until(deadline) {
                Some(Msg::Raw {
                    stream: "stderr",
                    text,
                }) if text == substrate::READY => return Ok(Sandbox::Ready),
                other => other.map(|m| self.clean(m)),
            };
            match msg {
                Some(Msg::Raw { stream, text }) => {
                    said.push(text.clone());
                    self.raw(stream, text)?;
                }
                Some(Msg::Exited(status)) => self.exit = Some(status),
                Some(Msg::ErrClosed) => self.stderr_closed = true,
                Some(Msg::Closed) => self.stdout_closed = true,
                Some(Msg::Rpc(msg)) => self.raw_json(msg)?,
                Some(Msg::Cancel) => {}
                None => {
                    let why = if said.is_empty() {
                        "no report".to_string()
                    } else {
                        said.join("; ")
                    };
                    return Ok(Sandbox::Failed(format!("the sandbox did not start: {why}")));
                }
            }
            if self.exit.is_some() && self.stderr_closed {
                let why = said.join("; ");
                return Ok(Sandbox::Failed(format!("the sandbox did not start: {why}")));
            }
        }
    }

    fn fail(&mut self, err: CallError) -> Result<(), String> {
        if self.cancel_deadline.is_some() {
            return self.to(State::Canceled, None, None);
        }
        let (reason, detail) = err.outcome();
        let detail = detail.map(|d| self.redact_str(d));
        self.to(State::Failed, Some(reason), detail)
    }

    fn reject(&mut self, detail: String) -> Result<(), String> {
        self.to(State::Rejected, Some(Reason::Error), Some(detail))
    }

    /// Admission through the end of the prompt.
    fn drive(&mut self, task: &TaskSubmitted, env_var: Env) -> Result<(), String> {
        if self.cancel.requested() {
            return self.to(State::Canceled, None, None);
        }
        let path = match profile::resolve(&task.profile) {
            Ok(p) => p,
            Err(e) => return self.reject(e),
        };
        let loaded = match profile::load(&path) {
            Ok(l) => l,
            Err(e) => return self.reject(e),
        };
        let worker = &loaded.profile.worker;
        // A continuation resumes its chain's session if a turn ever started in
        // it, as the log shows by turn content (a message, thought, tool call
        // or plan; not the notices an adapter sends on setup). A session no
        // turn started in holds nothing, and the harness may not know it, so
        // the continuation starts a new one.
        // A resume keeps the session id, so the whole chain shares it.
        let mut resume = None;
        let mut back = task.continues.clone();
        while let Some(prev) = back {
            let events = self.log.events(&prev)?;
            if events
                .iter()
                .any(|e| e.kind == WorkerUpdate::TYPE && turn_content(&e.data))
            {
                resume = events
                    .iter()
                    .rev()
                    .find(|e| e.kind == WorkerSession::TYPE)
                    .and_then(|e| e.data["session_id"].as_str().map(str::to_string));
                break;
            }
            back = events
                .iter()
                .find(|e| e.kind == TaskSubmitted::TYPE)
                .and_then(|e| e.data["continues"].as_str().map(str::to_string));
        }
        let mut env = Vec::new();
        for (name, reference) in &worker.env {
            let var = profile::reference_var(reference).expect("validated at load");
            let Some(value) = env_var(var) else {
                return self.reject(format!("credential reference {reference} is unset"));
            };
            if !value.is_empty() {
                self.secrets.push((value.clone(), reference.clone()));
            }
            env.push((name.clone(), value));
        }
        let mut grant = loaded.profile.grant.clone();
        if let Some(ro) = grant
            .ro
            .iter()
            .find(|ro| Path::new(&task.workspace).starts_with(ro))
        {
            return self.reject(format!("grant: ro path {ro:?} contains the workspace"));
        }
        // The profile's harness home: its state, granted read-write and named
        // by the variables the harness reads (spec: Harness home).
        let mut pins = Vec::new();
        let home = if worker.home_env.is_empty() {
            None
        } else {
            let prepared = match profile::prepare_home(env_var, &path, worker, &grant) {
                Ok(h) => h,
                Err(e) => return self.reject(e),
            };
            let home = prepared.path;
            for var in &worker.home_env {
                env.push((var.clone(), home.clone()));
            }
            grant.rw.push(home.clone());
            pins = prepared.pins;
            Some(home)
        };
        let grant = &grant;
        let probe = (grant.substrate == Substrate::Bwrap).then(substrate::probe);
        let mut levels = match substrate::levels(grant, probe.as_ref()) {
            Ok(l) => l,
            Err(e) => return self.reject(e),
        };
        self.allow = levels.get("filesystem") == Some(&Level::Enforced);
        // Worker settings: passed to the worker, not yet conformance-tested.
        let advisory = [
            ("system_prompt", worker.system_prompt.is_some()),
            ("meta", worker.meta.is_some()),
            ("home_isolation", home.is_some()),
        ];
        for (setting, present) in advisory {
            if present {
                levels.insert(setting.to_string(), Level::Advisory);
            }
        }
        self.log(
            Source::Host,
            &RunAdmitted {
                profile: task.profile.clone(),
                profile_digest: loaded.digest.clone(),
                command: worker.command.clone(),
                env: worker.env.clone(),
                substrate: serde_json::to_value(grant.substrate)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_string))
                    .unwrap_or_default(),
                levels,
                grant: grant.clone(),
                home,
                system_prompt: worker.system_prompt.clone(),
                meta: worker.meta.clone(),
            },
        )?;

        let mut command = match substrate::spawnable(
            probe.as_ref(),
            grant,
            &task.workspace,
            &worker.command,
            &env,
            &pins,
        ) {
            Ok(c) => c,
            Err(e) => return self.reject(e),
        };
        let spawned = command
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
        *self.cancel.waker.lock().expect("waker lock") = Some(conn.waker());
        self.worker = Some(conn);
        self.log(Source::Host, &WorkerSpawned { pid })?;
        if probe.is_some() {
            match self.await_sandbox()? {
                Sandbox::Ready => {}
                Sandbox::Failed(e) => return self.reject(e),
                Sandbox::Canceled => return self.to(State::Canceled, None, None),
            }
        }
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

        let mut params = json!({"cwd": task.workspace, "mcpServers": []});
        if let Some(p) = &worker.system_prompt {
            params["systemPrompt"] =
                json!({"mode": p.mode, "content": [{"type": "text", "text": p.text}]});
        }
        if let Some(meta) = &worker.meta {
            params["_meta"] = Value::Object(meta.clone());
        }
        let session = match &resume {
            None => match self.call("session/new", params)? {
                Ok(r) => r
                    .get("sessionId")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                Err(e) => return self.fail(e),
            },
            Some(id) => {
                if !result["agentCapabilities"]["sessionCapabilities"]["resume"].is_object() {
                    let detail = "the worker does not offer session/resume".to_string();
                    return self.to(State::Failed, Some(Reason::Error), Some(detail));
                }
                params["sessionId"] = json!(id);
                match self.call("session/resume", params)? {
                    Ok(_) => Some(id.clone()),
                    Err(e) => return self.fail(e),
                }
            }
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
        self.session = Some(session_id.clone());
        if self.cancel.requested() {
            return self.to(State::Canceled, None, None);
        }

        let prompt =
            json!({"sessionId": session_id, "prompt": [{"type": "text", "text": task.input}]});
        match self.call("session/prompt", prompt)? {
            // A cancel kagu asked for wins over the stop reason that answers it.
            Ok(_) if self.cancel_deadline.is_some() => self.to(State::Canceled, None, None)?,
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
                Some(Msg::Closed) => self.stdout_closed = true,
                Some(Msg::ErrClosed) => self.stderr_closed = true,
                Some(Msg::Cancel) => {}
                None if !killed => {
                    conn.kill();
                    killed = true;
                    deadline = Instant::now() + GRACE;
                }
                None => return Err(format!("worker {} did not exit after kill", conn.pid())),
            }
        }
        // Output that trails the exit, bounded: a descendant may hold a
        // stream open.
        let deadline = Instant::now() + GRACE;
        while !(self.stdout_closed && self.stderr_closed) {
            let Some(msg) = conn.recv_until(deadline).map(|m| self.clean(m)) else {
                break;
            };
            match msg {
                Msg::Closed => self.stdout_closed = true,
                Msg::ErrClosed => self.stderr_closed = true,
                Msg::Raw { stream, text } => self.raw(stream, text)?,
                Msg::Rpc(msg) => self.raw_json(msg)?,
                Msg::Exited(_) | Msg::Cancel => {}
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

/// Record a task and its first run, `submitted`. The workspace must be an
/// absolute path to a directory; the profile a path or a profile name.
pub fn submit(log: &Log, task: TaskSubmitted) -> Result<String, String> {
    if task.continues.is_some() {
        return Err("a continuation is submitted with task.continue".to_string());
    }
    record(log, task)
}

fn record(log: &Log, mut task: TaskSubmitted) -> Result<String, String> {
    let workspace = Path::new(&task.workspace);
    if !workspace.is_absolute() {
        return Err(format!("workspace {} is not absolute", workspace.display()));
    }
    let workspace = std::fs::canonicalize(workspace)
        .map_err(|e| format!("workspace {}: {e}", workspace.display()))?;
    if !workspace.is_dir() {
        return Err(format!(
            "workspace {} is not a directory",
            workspace.display()
        ));
    }
    task.workspace = workspace
        .to_str()
        .ok_or("workspace path is not UTF-8")?
        .to_string();
    let id = new_run_id();
    let data = serde_json::to_value(&task).expect("task serializes");
    log.append_task(&id, data)?;
    append_state(log, &id, State::Submitted)?;
    Ok(id)
}

/// Where credential references and the state directory are resolved: the
/// host's environment.
pub use crate::paths::Env;

/// Whether a `session/update` is content of a turn, which exists only once
/// the turn has started.
fn turn_content(update: &Value) -> bool {
    matches!(
        update["sessionUpdate"].as_str(),
        Some(
            "user_message_chunk"
                | "agent_message_chunk"
                | "agent_thought_chunk"
                | "tool_call"
                | "tool_call_update"
                | "plan"
        )
    )
}

/// Record a task that continues `prev` with `input`: the same profile and
/// workspace, in the same thread, resuming `prev`'s worker session.
pub fn continue_run(log: &Log, prev: &str, input: String) -> Result<String, String> {
    let events = log.events(prev)?;
    let earlier: TaskSubmitted = events
        .iter()
        .find(|e| e.kind == TaskSubmitted::TYPE)
        .ok_or_else(|| format!("no run {prev}"))
        .and_then(|e| serde_json::from_value(e.data.clone()).map_err(|e| e.to_string()))?;
    let state = fold(&events).map_err(|e| e.to_string())?.state;
    if !state.is_some_and(State::is_terminal) {
        return Err(format!("run {prev} has not ended"));
    }
    record(
        log,
        TaskSubmitted {
            profile: earlier.profile,
            workspace: earlier.workspace,
            input,
            thread: earlier.thread.or_else(|| Some(prev.to_string())),
            continues: Some(prev.to_string()),
        },
    )
}

/// Drive a submitted run to a terminal state. Runs on the run's own thread.
pub fn start(log: &Log, id: &str, cancel: &Cancel, env_var: Env) -> Result<(), String> {
    let events = log.events(id)?;
    let task: TaskSubmitted = events
        .iter()
        .find(|e| e.kind == TaskSubmitted::TYPE)
        .ok_or_else(|| format!("run {id} has no task"))
        .and_then(|e| serde_json::from_value(e.data.clone()).map_err(|e| e.to_string()))?;
    let mut actor = Actor {
        id: id.to_string(),
        log,
        cancel,
        secrets: Vec::new(),
        view: fold(&events).map_err(|e| e.to_string())?,
        worker: None,
        exit: None,
        session: None,
        cancel_deadline: None,
        allow: false,
        stdout_closed: false,
        stderr_closed: false,
    };
    let driven = actor.drive(&task, env_var);
    // The worker is stopped whatever happened, so none outlives its run.
    let stopped = actor.stop_worker();
    driven?;
    stopped
}

/// Record `canceled` for a run that never started, on behalf of its actor.
pub fn cancel_unstarted(log: &Log, id: &str) -> Result<(), String> {
    append_state(log, id, State::Canceled)
}

/// Record `interrupted` for a run the host found unfinished at start: the
/// host did not observe its end, so nothing else is recorded (Invariant 9).
pub fn interrupt(log: &Log, id: &str) -> Result<(), String> {
    append_state(log, id, State::Interrupted)
}

fn append_state(log: &Log, id: &str, to: State) -> Result<(), String> {
    let state = RunState {
        to,
        reason: None,
        detail: None,
    };
    let data = serde_json::to_value(&state).expect("state serializes");
    log.append(id, Source::Host, RunState::TYPE, RunState::V, data)?;
    Ok(())
}
