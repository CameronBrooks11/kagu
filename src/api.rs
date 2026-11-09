//! The client API, version 1: each method's params and result. These types
//! are the contract; their schemas are committed (spec: Contracts).

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::event::{RunView, TaskSubmitted};

pub const VERSION: u32 = 1;

/// Params of `run.start`, `run.cancel` and `log.read`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RunRef {
    pub run: String,
}

/// Params of methods that take none.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NoParams {}

/// Result of methods that return nothing but success.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Done {}

/// Result of `task.submit`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Submitted {
    pub run: String,
}

/// Params of `log.subscribe`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Subscribe {
    /// Stream events after this seq instead of after the snapshot's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<i64>,
}

/// Result of `log.subscribe`: the non-terminal runs and the seq they reflect,
/// read together. Events then follow as `log.event` notifications.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub runs: BTreeMap<String, RunView>,
    pub seq: i64,
}

/// Result of `log.read`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Events {
    pub events: Vec<crate::event::Event>,
}

/// Result of `host.status`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Status {
    pub api: u32,
    pub active: Vec<String>,
}

/// Result of `profile.list`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Profiles {
    pub profiles: Vec<String>,
}

/// Every method: name, params, result.
pub fn methods() -> Vec<(&'static str, schemars::Schema, schemars::Schema)> {
    use schemars::schema_for;
    vec![
        (
            "task.submit",
            schema_for!(TaskSubmitted),
            schema_for!(Submitted),
        ),
        ("run.start", schema_for!(RunRef), schema_for!(Done)),
        ("run.cancel", schema_for!(RunRef), schema_for!(Done)),
        (
            "log.subscribe",
            schema_for!(Subscribe),
            schema_for!(Snapshot),
        ),
        ("log.read", schema_for!(RunRef), schema_for!(Events)),
        ("host.status", schema_for!(NoParams), schema_for!(Status)),
        ("profile.list", schema_for!(NoParams), schema_for!(Profiles)),
    ]
}
