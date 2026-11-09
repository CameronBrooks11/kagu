//! Generated JSON Schemas, committed under `schemas/` (spec: Contracts).

use schemars::{JsonSchema, schema_for};
use serde_json::Value;

use crate::event::*;
use crate::profile::Profile;

fn event<T: Payload>() -> (String, Value) {
    (
        format!("events/{}.v{}.json", T::TYPE, T::V),
        schema_for!(T).to_value(),
    )
}

fn named<T: JsonSchema>(path: &str) -> (String, Value) {
    (path.to_string(), schema_for!(T).to_value())
}

/// Every committed schema, as (path under `schemas/`, schema).
pub fn all() -> Vec<(String, Value)> {
    let api = crate::api::methods()
        .into_iter()
        .map(|(method, params, result)| {
            (
                format!("api/v{}/{method}.json", crate::api::VERSION),
                serde_json::json!({"params": params, "result": result}),
            )
        });
    let mut all = vec![
        named::<Event>("envelope.v1.json"),
        named::<Profile>("profile.v1.json"),
        event::<TaskSubmitted>(),
        event::<RunAdmitted>(),
        event::<RunState>(),
        event::<RunVerdict>(),
        event::<PermissionDecided>(),
        event::<WorkerSpawned>(),
        event::<WorkerInitialized>(),
        event::<WorkerSession>(),
        event::<WorkerUpdate>(),
        event::<WorkerRaw>(),
        event::<WorkerExited>(),
    ];
    all.extend(api);
    all
}
