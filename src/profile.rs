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
    #[serde(default)]
    pub grant: Grant,
}

/// What a run's worker may reach (spec: Grant).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Grant {
    #[serde(default)]
    pub substrate: Substrate,
    #[serde(default)]
    pub network: Network,
    /// Extra paths the worker may read, such as its own runtime.
    #[serde(default)]
    pub ro: Vec<String>,
    /// Extra paths the worker may write, besides its workspace.
    #[serde(default)]
    pub rw: Vec<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Substrate {
    /// bubblewrap namespaces with Landlock applied inside.
    #[default]
    Bwrap,
    /// No confinement: every grant field is `unsupported`.
    None,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Network {
    /// The host's network, shared: recorded as `unrestricted`.
    #[default]
    Host,
    /// An empty network namespace.
    Off,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Worker {
    /// The worker's argv; it must speak ACP v1 on stdio.
    pub command: Vec<String>,
    /// Environment variable name to credential reference (`env:NAME`).
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Variables set to this profile's harness home, such as
    /// `CLAUDE_CONFIG_DIR`. With none, the profile has no home.
    #[serde(default)]
    pub home_env: Vec<String>,
    /// ACP `session/new` `systemPrompt`.
    #[serde(default)]
    pub system_prompt: Option<SystemPrompt>,
    /// The adapter's `_meta` on `session/new`, passed through unmodified.
    #[serde(default)]
    pub meta: Option<serde_json::Map<String, serde_json::Value>>,
    /// Files kagu writes at the top of the home before every run, by name,
    /// and mounts read-only under bwrap: for configuration the harness reads
    /// from its home whatever it is told (spec: Harness home).
    #[serde(default)]
    pub home_files: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SystemPrompt {
    pub mode: PromptMode,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum PromptMode {
    Append,
    Override,
}

/// The name a profile's home is kept under: its file name without `.toml`,
/// then a digest of its canonical path, so two profile files never share a
/// home.
pub fn home_key(path: &Path) -> Result<String, String> {
    let canonical = std::fs::canonicalize(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let stem = canonical
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| format!("{}: no profile name", path.display()))?;
    let digest = Sha256::digest(canonical.as_os_str().as_encoded_bytes());
    let short = digest[..6].iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    });
    Ok(format!("{stem}-{short}"))
}

/// Create the profile's harness home (0700) and write its `home_files`.
/// Returns the home.
pub fn prepare_home(
    env: crate::paths::Env,
    path: &Path,
    worker: &Worker,
) -> Result<String, String> {
    use std::os::unix::fs::DirBuilderExt;
    let home = crate::paths::home(env, &home_key(path)?)?;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&home)
        .map_err(|e| format!("{}: {e}", home.display()))?;
    for (name, text) in &worker.home_files {
        let file = home.join(name);
        // A run can leave a link here; replace it rather than follow it.
        match std::fs::symlink_metadata(&file) {
            Ok(m) if m.is_dir() => return Err(format!("{}: is a directory", file.display())),
            Ok(_) => std::fs::remove_file(&file).map_err(|e| format!("{}: {e}", file.display()))?,
            Err(_) => {}
        }
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&file)
            .and_then(|mut f| std::io::Write::write_all(&mut f, text.as_bytes()))
            .map_err(|e| format!("{}: {e}", file.display()))?;
    }
    home.to_str()
        .map(str::to_string)
        .ok_or_else(|| "home path is not UTF-8".to_string())
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
    for path in profile.grant.ro.iter().chain(&profile.grant.rw) {
        if !Path::new(path).is_absolute() {
            return Err(format!("grant path {path:?} is not absolute"));
        }
    }
    // A read-only path is mounted over the writable ones, so one that
    // contains a writable path would make it read-only.
    for ro in &profile.grant.ro {
        if let Some(rw) = profile
            .grant
            .rw
            .iter()
            .find(|rw| Path::new(rw).starts_with(ro))
        {
            return Err(format!("grant: ro path {ro:?} contains rw path {rw:?}"));
        }
    }
    for name in profile.worker.home_files.keys() {
        if name.is_empty() || name.contains('/') || name == "." || name == ".." {
            return Err(format!("worker.home_files: {name:?} is not a file name"));
        }
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
