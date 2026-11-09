//! Where kagu keeps things, per the XDG base directories.

use std::path::PathBuf;

fn xdg(var: &str, fallback: &str) -> Result<PathBuf, String> {
    match std::env::var_os(var).filter(|v| !v.is_empty()) {
        Some(dir) => Ok(PathBuf::from(dir)),
        None => {
            let home = std::env::var_os("HOME").ok_or("HOME is unset")?;
            Ok(PathBuf::from(home).join(fallback))
        }
    }
}

/// `$XDG_CONFIG_HOME/kagu`.
pub fn config() -> Result<PathBuf, String> {
    Ok(xdg("XDG_CONFIG_HOME", ".config")?.join("kagu"))
}

/// `$XDG_STATE_HOME/kagu`.
pub fn state() -> Result<PathBuf, String> {
    Ok(xdg("XDG_STATE_HOME", ".local/state")?.join("kagu"))
}

/// `$XDG_RUNTIME_DIR/kagu/kagu.sock`. There is no fallback: the runtime
/// directory is the one private, per-boot place for a socket.
pub fn socket() -> Result<PathBuf, String> {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|v| !v.is_empty())
        .ok_or("XDG_RUNTIME_DIR is unset; pass --socket")?;
    Ok(PathBuf::from(dir).join("kagu").join("kagu.sock"))
}
