//! Where kagu keeps things, per the XDG base directories.

use std::path::PathBuf;

/// An environment lookup: the process's, or one a caller supplies.
pub type Env<'a> = &'a dyn Fn(&str) -> Option<String>;

fn process_env(var: &str) -> Option<String> {
    std::env::var(var).ok()
}

fn xdg_with(env: Env, var: &str, fallback: &str) -> Result<PathBuf, String> {
    match env(var).filter(|v| !v.is_empty()) {
        Some(dir) => Ok(PathBuf::from(dir)),
        None => {
            let home = env("HOME").ok_or("HOME is unset")?;
            Ok(PathBuf::from(home).join(fallback))
        }
    }
}

fn xdg(var: &str, fallback: &str) -> Result<PathBuf, String> {
    xdg_with(&process_env, var, fallback)
}

/// `$XDG_CONFIG_HOME/kagu`.
pub fn config() -> Result<PathBuf, String> {
    Ok(xdg("XDG_CONFIG_HOME", ".config")?.join("kagu"))
}

/// `$XDG_STATE_HOME/kagu`.
pub fn state() -> Result<PathBuf, String> {
    Ok(xdg("XDG_STATE_HOME", ".local/state")?.join("kagu"))
}

/// `$XDG_STATE_HOME/kagu/homes/<profile>`: a profile's harness home.
pub fn home(env: Env, profile: &str) -> Result<PathBuf, String> {
    Ok(xdg_with(env, "XDG_STATE_HOME", ".local/state")?
        .join("kagu")
        .join("homes")
        .join(profile))
}

/// `$XDG_RUNTIME_DIR/kagu/kagu.sock`. There is no fallback: the runtime
/// directory is the one private, per-boot place for a socket.
pub fn socket() -> Result<PathBuf, String> {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|v| !v.is_empty())
        .ok_or("XDG_RUNTIME_DIR is unset; pass --socket")?;
    Ok(PathBuf::from(dir).join("kagu").join("kagu.sock"))
}
