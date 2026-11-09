//! The event envelope, the payload of each event type, and the fold that
//! derives a run's state from its events (spec: Event envelope, Invariant 1).

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Host,
    Worker,
    Client,
}

/// Run lifecycle states (spec: Run lifecycle).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Submitted,
    Working,
    InputRequired,
    Checking,
    Completed,
    Failed,
    Canceled,
    Rejected,
    Interrupted,
}

impl State {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Canceled | Self::Rejected | Self::Interrupted
        )
    }
}

/// Why a run ended `failed` or `rejected`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    AuthRequired,
    Error,
    Exited,
    Limit,
    Refusal,
    UnknownStopReason,
    UnrequestedCancel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Pass,
    Fail,
    Error,
    NotRun,
}

/// How a setting is held (spec: Enforcement levels).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    Enforced,
    Supported,
    Advisory,
    Unsupported,
    Unrestricted,
}

/// A stored event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Event {
    /// Host-wide, strictly increasing, assigned at commit; gaps allowed.
    pub seq: i64,
    pub run: String,
    /// RFC 3339 UTC.
    pub ts: String,
    pub source: Source,
    #[serde(rename = "type")]
    pub kind: String,
    pub v: u32,
    pub data: Value,
}

/// A payload type: its `type` and `v` name its schema.
pub trait Payload: Serialize + DeserializeOwned + JsonSchema {
    const TYPE: &'static str;
    const V: u32;
}

macro_rules! payload {
    ($t:ty, $name:literal, $v:literal) => {
        impl Payload for $t {
            const TYPE: &'static str = $name;
            const V: u32 = $v;
        }
    };
}

/// `task.submitted`: the intent, as the client gave it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TaskSubmitted {
    pub profile: String,
    pub workspace: String,
    pub input: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread: Option<String>,
}
payload!(TaskSubmitted, "task.submitted", 1);

/// `run.admitted`: the configuration the run is bound to (Invariant 5).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RunAdmitted {
    /// SHA-256 of the profile file, hex.
    pub profile_digest: String,
    pub command: Vec<String>,
    /// Environment variable name to credential reference; never a value.
    pub env: BTreeMap<String, String>,
    pub substrate: String,
    pub levels: BTreeMap<String, Level>,
}
payload!(RunAdmitted, "run.admitted", 1);

/// `run.state`: a lifecycle transition.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RunState {
    pub to: State,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<Reason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}
payload!(RunState, "run.state", 1);

/// `run.verdict`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RunVerdict {
    pub verdict: Verdict,
}
payload!(RunVerdict, "run.verdict", 1);

/// `worker.spawned`: the root process of the run's worker.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WorkerSpawned {
    pub pid: u32,
}
payload!(WorkerSpawned, "worker.spawned", 1);

/// `worker.initialized`: what the worker reported in ACP `initialize`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WorkerInitialized {
    pub protocol_version: u64,
    /// ACP `agentInfo`, unmodified.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_info: Option<Value>,
}
payload!(WorkerInitialized, "worker.initialized", 1);

/// `worker.session`: the harness's own session id, used to resume.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WorkerSession {
    pub session_id: String,
}
payload!(WorkerSession, "worker.session", 1);

/// `worker.update`: an ACP v1 `SessionUpdate`, unmodified.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct WorkerUpdate(pub Value);
payload!(WorkerUpdate, "worker.update", 1);

/// `worker.raw`: anything outside ACP's session updates, kept for diagnostics.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WorkerRaw {
    /// `stdout` or `stderr`.
    pub stream: String,
    pub text: String,
}
payload!(WorkerRaw, "worker.raw", 1);

/// `worker.exited`: an exit the host observed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WorkerExited {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<i32>,
}
payload!(WorkerExited, "worker.exited", 1);

/// What a run's events say about it.
#[derive(Debug, Clone, PartialEq)]
pub struct RunView {
    pub state: Option<State>,
    pub reason: Option<Reason>,
    pub verdict: Verdict,
}

/// Derive a run's state from its events, in `seq` order (Invariant 1).
pub fn fold(events: &[Event]) -> serde_json::Result<RunView> {
    let mut view = RunView {
        state: None,
        reason: None,
        verdict: Verdict::NotRun,
    };
    for e in events {
        match (e.kind.as_str(), e.v) {
            (RunState::TYPE, RunState::V) => {
                let s: RunState = serde_json::from_value(e.data.clone())?;
                view.state = Some(s.to);
                view.reason = s.reason;
            }
            (RunVerdict::TYPE, RunVerdict::V) => {
                let v: RunVerdict = serde_json::from_value(e.data.clone())?;
                view.verdict = v.verdict;
            }
            _ => {}
        }
    }
    Ok(view)
}
