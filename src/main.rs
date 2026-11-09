use std::path::PathBuf;
use std::process::ExitCode;

use kagu::event::State;
use kagu::run::{Options, run};
use kagu::store::Store;

const USAGE: &str =
    "usage: kagu run --profile <file> --workspace <dir> --prompt <text> [--db <file>]";

fn main() -> ExitCode {
    match cli(std::env::args().skip(1).collect()) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("kagu: {e}");
            ExitCode::from(2)
        }
    }
}

fn cli(args: Vec<String>) -> Result<ExitCode, String> {
    let mut args = args.into_iter();
    if args.next().as_deref() != Some("run") {
        return Err(USAGE.into());
    }
    let (mut profile, mut workspace, mut prompt, mut db) = (None, None, None, None);
    while let Some(flag) = args.next() {
        let value = args.next().ok_or(USAGE)?;
        match flag.as_str() {
            "--profile" => profile = Some(PathBuf::from(value)),
            "--workspace" => workspace = Some(PathBuf::from(value)),
            "--prompt" => prompt = Some(value),
            "--db" => db = Some(PathBuf::from(value)),
            _ => return Err(USAGE.into()),
        }
    }
    let opts = Options {
        profile: profile.ok_or(USAGE)?,
        workspace: workspace.ok_or(USAGE)?,
        input: prompt.ok_or(USAGE)?,
    };
    let db = match db {
        Some(p) => p,
        None => default_db()?,
    };
    let store = Store::open(&db).map_err(|e| format!("{}: {e}", db.display()))?;
    let (_, view) = run(&store, &opts, &mut |event| {
        println!(
            "{}",
            serde_json::to_string(event).expect("event serializes")
        );
    })?;
    // Exit 0 only for a run that executed to the end; the verdict is in the log.
    Ok(if view.state == Some(State::Completed) {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

/// `$XDG_STATE_HOME/kagu/kagu.db`, defaulting to `~/.local/state`.
fn default_db() -> Result<PathBuf, String> {
    let base = match std::env::var_os("XDG_STATE_HOME").filter(|v| !v.is_empty()) {
        Some(dir) => PathBuf::from(dir),
        None => {
            PathBuf::from(std::env::var_os("HOME").ok_or("HOME is unset")?).join(".local/state")
        }
    };
    let dir = base.join("kagu");
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    Ok(dir.join("kagu.db"))
}
