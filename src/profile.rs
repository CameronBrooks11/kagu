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

/// Refuse a confined grant that reaches kagu's own state: the event log,
/// every profile's home and credentials, and the pinned files. kagu itself
/// grants a run its own home and pins. Each path is compared both as written
/// and with links and `..` resolved, against the state directory both ways;
/// a path that does not exist cannot be mounted, so it reaches nothing.
pub fn keep_out_of_state(
    env: crate::paths::Env,
    grant: &Grant,
    workspace: Option<&str>,
) -> Result<(), String> {
    if grant.substrate != Substrate::Bwrap {
        return Ok(());
    }
    let raw = crate::paths::state_with(env)?;
    let states = [std::fs::canonicalize(&raw).ok(), Some(raw.clone())];
    for p in workspace
        .into_iter()
        .chain(grant.rw.iter().map(String::as_str))
        .chain(grant.ro.iter().map(String::as_str))
    {
        let written = Path::new(p);
        let spellings = [
            std::fs::canonicalize(written).ok(),
            Some(written.to_path_buf()),
        ];
        for path in spellings.iter().flatten() {
            for state in states.iter().flatten() {
                if path.starts_with(state) || state.starts_with(path) {
                    return Err(format!("grant: {p} reaches kagu's state {}", raw.display()));
                }
            }
        }
    }
    Ok(())
}

/// A profile's harness home, ready for a run.
pub struct Home {
    pub path: String,
    /// Read-only binds of pinned files, source onto destination, under bwrap.
    pub pins: Vec<(String, String)>,
}

/// Create the profile's harness home (0700) and place its `home_files`.
///
/// Under bwrap each pinned file is bound read-only from a copy in kagu's
/// state, named by its digest and rewritten every time, and the host never removes or
/// renames a regular file in the home: that would detach the bind of a run
/// already using it. Without a sandbox there are no binds, and each file
/// is replaced atomically before the run.
pub fn prepare_home(
    env: crate::paths::Env,
    path: &Path,
    worker: &Worker,
    grant: &Grant,
) -> Result<Home, String> {
    use std::os::unix::fs::DirBuilderExt;
    let home = crate::paths::home(env, &home_key(path)?)?;
    let text = home.to_str().ok_or("home path is not UTF-8")?.to_string();
    if let Some(ro) = grant.ro.iter().find(|ro| home.starts_with(ro)) {
        return Err(format!("grant: ro path {ro:?} contains the harness home"));
    }
    let private = |dir: &Path| {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .map_err(|e| format!("{}: {e}", dir.display()))
    };
    private(&home)?;
    let bind = grant.substrate == Substrate::Bwrap;
    let mut pins = Vec::new();
    for (name, content) in &worker.home_files {
        let file = home.join(name);
        let err = |e: std::io::Error| format!("{}: {e}", file.display());
        match std::fs::symlink_metadata(&file) {
            Ok(m) if m.is_dir() => return Err(format!("{}: is a directory", file.display())),
            // A link is never a mount point: replace it, never follow it.
            Ok(m) if m.file_type().is_symlink() => match std::fs::remove_file(&file) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(err(e)),
                _ => {}
            },
            _ => {}
        }
        if bind {
            // The mount point: created if missing, otherwise left alone.
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&file)
            {
                Err(e) if e.kind() != std::io::ErrorKind::AlreadyExists => return Err(err(e)),
                _ => {}
            }
            let store = crate::paths::pins(env)?;
            private(&store)?;
            let source = store.join(hex(&Sha256::digest(content.as_bytes())));
            // Rewritten every time: what is there is not trusted. A run
            // already bound to it keeps the inode it was given.
            replace(&source, content)?;
            let source = source.to_str().ok_or("pin path is not UTF-8")?.to_string();
            pins.push((source, format!("{text}/{name}")));
        } else {
            replace(&file, content)?;
        }
    }
    Ok(Home { path: text, pins })
}

/// Write `content` to `file` by renaming a fresh file over it: concurrent
/// writers each succeed, and a link at `file` is replaced, not followed.
fn replace(file: &Path, content: &str) -> Result<(), String> {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let name = file.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    let tmp = file.with_file_name(format!(".{name}.kagu-{}-{n}", std::process::id()));
    std::fs::write(&tmp, content)
        .and_then(|()| std::fs::rename(&tmp, file))
        .map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            format!("{}: {e}", file.display())
        })
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
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
