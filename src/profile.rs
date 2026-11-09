//! Profiles: TOML, version 1, unknown keys refused (spec: Contracts).

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    /// Profile schema version; must be 1.
    pub version: u32,
    pub worker: Worker,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Worker {
    /// The worker's argv; it must speak ACP v1 on stdio.
    pub command: Vec<String>,
    /// Environment variable name to credential reference (`env:NAME`).
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

/// A profile and the SHA-256 of the file it came from.
pub struct Loaded {
    pub profile: Profile,
    pub digest: String,
}

pub fn load(path: &Path) -> Result<Loaded, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let text = std::str::from_utf8(&bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    let profile: Profile = toml::from_str(text).map_err(|e| format!("{}: {e}", path.display()))?;
    if profile.version != 1 {
        return Err(format!("unsupported profile version {}", profile.version));
    }
    if profile.worker.command.is_empty() {
        return Err("worker.command is empty".into());
    }
    for (name, reference) in &profile.worker.env {
        reference_var(reference).ok_or_else(|| {
            format!("worker.env.{name}: {reference:?} is not a reference (expected env:NAME)")
        })?;
    }
    let digest = Sha256::digest(&bytes)
        .iter()
        .fold(String::new(), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        });
    Ok(Loaded { profile, digest })
}

/// The variable an `env:NAME` reference names.
pub fn reference_var(reference: &str) -> Option<&str> {
    reference.strip_prefix("env:").filter(|v| !v.is_empty())
}

/// A profile as a task names it: a path (containing `/`), or a name under
/// `$XDG_CONFIG_HOME/kagu/profiles/` (default `~/.config`).
pub fn resolve(profile: &str) -> Result<PathBuf, String> {
    if profile.contains('/') {
        return Ok(PathBuf::from(profile));
    }
    if profile.is_empty() || profile.starts_with('.') {
        return Err(format!("{profile:?} is not a profile name"));
    }
    Ok(crate::paths::config()?
        .join("profiles")
        .join(format!("{profile}.toml")))
}
