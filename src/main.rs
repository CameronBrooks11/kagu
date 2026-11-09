use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use serde_json::{Value, json};

use kagu::client::Client;
use kagu::event::{Event, State};

const USAGE: &str = "usage:
  kagu serve [--db <file>] [--socket <path>]
  kagu run --profile <name|file> --workspace <dir> --prompt <text> [--thread <id>] [--json]
  kagu watch [<run>] [--after <seq>] [--json]
  kagu log <run> [--json]
  kagu continue <run> --prompt <text> [--json]
  kagu cancel <run>
  kagu status
  kagu auth <profile> [--method <id>]
Every client command takes --socket <path> (default $XDG_RUNTIME_DIR/kagu/kagu.sock).";

fn main() -> ExitCode {
    match cli(std::env::args().skip(1).collect()) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("kagu: {e}");
            ExitCode::from(2)
        }
    }
}

struct Args {
    positional: Vec<String>,
    flags: BTreeMap<String, String>,
    json: bool,
}

impl Args {
    fn parse(args: Vec<String>) -> Result<Self, String> {
        let mut out = Args {
            positional: Vec::new(),
            flags: BTreeMap::new(),
            json: false,
        };
        let mut args = args.into_iter();
        while let Some(arg) = args.next() {
            match arg.strip_prefix("--") {
                Some("json") => out.json = true,
                Some(flag) => {
                    let value = args.next().ok_or(USAGE)?;
                    out.flags.insert(flag.to_string(), value);
                }
                None => out.positional.push(arg),
            }
        }
        Ok(out)
    }

    fn flag(&self, name: &str) -> Option<&str> {
        self.flags.get(name).map(String::as_str)
    }

    fn required(&self, name: &str) -> Result<&str, String> {
        self.flag(name)
            .ok_or_else(|| format!("--{name} is required\n{USAGE}"))
    }

    fn socket(&self) -> Result<PathBuf, String> {
        match self.flag("socket") {
            Some(p) => Ok(PathBuf::from(p)),
            None => kagu::paths::socket(),
        }
    }

    fn connect(&self) -> Result<Client, String> {
        Client::connect(&self.socket()?)
    }

    /// Reject flags the command does not take.
    fn only(&self, allowed: &[&str]) -> Result<(), String> {
        match self.flags.keys().find(|k| !allowed.contains(&k.as_str())) {
            Some(k) => Err(format!("unknown flag --{k}\n{USAGE}")),
            None => Ok(()),
        }
    }
}

fn cli(args: Vec<String>) -> Result<ExitCode, String> {
    let Some((command, rest)) = args.split_first() else {
        return Err(USAGE.into());
    };
    if command == "sandbox-init" {
        // Inside the sandbox: on success this process becomes the worker.
        if let Err(e) = kagu::substrate::sandbox_init(rest) {
            eprintln!("kagu sandbox-init: {e}");
            return Ok(ExitCode::from(126));
        }
        return Ok(ExitCode::SUCCESS);
    }
    let args = Args::parse(rest.to_vec())?;
    match command.as_str() {
        "serve" => {
            args.only(&["db", "socket"])?;
            let db = match args.flag("db") {
                Some(p) => PathBuf::from(p),
                None => {
                    let dir = kagu::paths::state()?;
                    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
                    dir.join("kagu.db")
                }
            };
            kagu::host::serve(&db, &args.socket()?)?;
            Ok(ExitCode::SUCCESS)
        }
        "run" => {
            args.only(&["profile", "workspace", "prompt", "thread", "socket"])?;
            run(&args)
        }
        "continue" => {
            args.only(&["prompt", "socket"])?;
            let prev = args.positional.first().ok_or(USAGE)?;
            let input = args.required("prompt")?;
            let (_, events) = args.connect()?.subscribe(None)?;
            let mut control = args.connect()?;
            let run = control.call("task.continue", json!({ "run": prev, "input": input }))?["run"]
                .as_str()
                .ok_or("host returned no run id")?
                .to_string();
            eprintln!("kagu: run {run} continues {prev}");
            control.call("run.start", json!({ "run": run }))?;
            follow(&run, events, Render::new(args.json), 0)
        }
        "watch" => {
            args.only(&["after", "socket"])?;
            watch(&args)
        }
        "log" => {
            args.only(&["socket"])?;
            let run = args.positional.first().ok_or(USAGE)?;
            let mut out = Render::new(args.json);
            let events = read(&mut args.connect()?, run)?;
            if events.is_empty() {
                return Err(format!("no run {run}"));
            }
            for e in events {
                out.event(&e);
            }
            out.end();
            Ok(ExitCode::SUCCESS)
        }
        "cancel" => {
            args.only(&["socket"])?;
            let run = args.positional.first().ok_or(USAGE)?;
            args.connect()?.call("run.cancel", json!({ "run": run }))?;
            Ok(ExitCode::SUCCESS)
        }
        "auth" => {
            args.only(&["method"])?;
            let profile = args.positional.first().ok_or(USAGE)?;
            let code = kagu::auth::auth(&profile_arg(profile)?, args.flag("method"))?;
            Ok(ExitCode::from(u8::try_from(code).unwrap_or(1)))
        }
        "status" => {
            args.only(&["socket"])?;
            let status = args.connect()?.call("host.status", json!({}))?;
            println!("{}", serde_json::to_string_pretty(&status).expect("json"));
            Ok(ExitCode::SUCCESS)
        }
        _ => Err(USAGE.into()),
    }
}

/// A profile argument as the host should see it: a name as given, a path
/// made absolute against this process's directory.
fn profile_arg(profile: &str) -> Result<String, String> {
    if !profile.contains('/') && !profile.ends_with(".toml") {
        return Ok(profile.to_string());
    }
    absolute(Path::new(profile))
}

fn absolute(path: &Path) -> Result<String, String> {
    let path = std::fs::canonicalize(path).map_err(|e| format!("{}: {e}", path.display()))?;
    path.to_str()
        .map(str::to_string)
        .ok_or_else(|| format!("{} is not UTF-8", path.display()))
}

fn run(args: &Args) -> Result<ExitCode, String> {
    let mut task = json!({
        "profile": profile_arg(args.required("profile")?)?,
        "workspace": absolute(Path::new(args.required("workspace")?))?,
        "input": args.required("prompt")?,
    });
    if let Some(thread) = args.flag("thread") {
        task["thread"] = json!(thread);
    }
    // Subscribe first, so no event of the new run can be missed.
    let (snapshot, events) = args.connect()?.subscribe(None)?;
    let _ = snapshot;
    let mut control = args.connect()?;
    let run = control.call("task.submit", task)?["run"]
        .as_str()
        .ok_or("host returned no run id")?
        .to_string();
    eprintln!("kagu: run {run} (detach with Ctrl-C; `kagu cancel {run}` to stop it)");
    control.call("run.start", json!({ "run": run }))?;
    follow(&run, events, Render::new(args.json), 0)
}

fn read(client: &mut Client, run: &str) -> Result<Vec<Event>, String> {
    let events = client.call("log.read", json!({ "run": run }))?;
    serde_json::from_value(events["events"].clone()).map_err(|e| e.to_string())
}

fn watch(args: &Args) -> Result<ExitCode, String> {
    let after = match args.flag("after") {
        Some(s) => Some(s.parse::<i64>().map_err(|e| format!("--after: {e}"))?),
        None => None,
    };
    let mut out = Render::new(args.json);
    let Some(run) = args.positional.first() else {
        let (_, events) = args.connect()?.subscribe(after)?;
        for e in events {
            out.event(&e?);
        }
        return Ok(ExitCode::SUCCESS);
    };
    // History first, then everything after it: no gap, no repeat. The whole
    // history is read even with a cursor, because whether the run has ended
    // depends on events before it; only events after the cursor are printed.
    let history = read(&mut args.connect()?, run)?;
    if history.is_empty() {
        return Err(format!("no run {run}"));
    }
    let from = history
        .last()
        .map(|e| e.seq)
        .unwrap_or(0)
        .max(after.unwrap_or(0));
    let (_, events) = args.connect()?.subscribe(Some(from))?;
    let history = history.into_iter().map(Ok);
    follow(run, history.chain(events), out, after.unwrap_or(0))
}

/// Follow a run's events until it has ended and its worker is gone,
/// printing those after `print_after`.
fn follow(
    run: &str,
    events: impl Iterator<Item = Result<Event, String>>,
    mut out: Render,
    print_after: i64,
) -> Result<ExitCode, String> {
    let (mut state, mut spawned, mut exited) = (None, false, false);
    for e in events {
        let e = e?;
        if e.run != run {
            continue;
        }
        if e.seq > print_after {
            out.event(&e);
        }
        match e.kind.as_str() {
            "run.state" => state = serde_json::from_value::<State>(e.data["to"].clone()).ok(),
            "worker.spawned" => spawned = true,
            "worker.exited" => exited = true,
            _ => {}
        }
        // No exit follows an interrupted run: the host did not observe one.
        let no_exit_coming = !spawned || state == Some(State::Interrupted);
        if state.is_some_and(State::is_terminal) && (exited || no_exit_coming) {
            out.end();
            return Ok(if state == Some(State::Completed) {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            });
        }
    }
    out.end();
    Err("host closed the connection".into())
}

/// Human-readable output: the agent's text as it streams, one line per tool
/// call and per lifecycle event. `--json` prints each event instead.
struct Render {
    json: bool,
    mid_line: bool,
}

impl Render {
    fn new(json: bool) -> Self {
        Self {
            json,
            mid_line: false,
        }
    }

    fn line(&mut self, text: &str) {
        if self.mid_line {
            println!();
            self.mid_line = false;
        }
        println!("{text}");
    }

    fn end(&mut self) {
        if self.mid_line {
            println!();
            self.mid_line = false;
        }
    }

    fn event(&mut self, e: &Event) {
        if self.json {
            println!("{}", serde_json::to_string(e).expect("event serializes"));
            return;
        }
        let d = &e.data;
        let text = |v: &Value| v.as_str().unwrap_or("").to_string();
        match e.kind.as_str() {
            "worker.update" => match d["sessionUpdate"].as_str() {
                Some("agent_message_chunk") if d["content"]["type"] == "text" => {
                    print!("{}", text(&d["content"]["text"]));
                    self.mid_line = true;
                }
                Some("tool_call") => self.line(&format!("→ {}", text(&d["title"]))),
                _ => {}
            },
            "run.state" => {
                let mut s = format!("· {} {}", e.run, text(&d["to"]));
                if !d["reason"].is_null() {
                    s += &format!(" ({})", text(&d["reason"]));
                }
                if !d["detail"].is_null() {
                    s += &format!(": {}", text(&d["detail"]));
                }
                self.line(&s);
            }
            "run.verdict" => self.line(&format!("· verdict {}", text(&d["verdict"]))),
            "permission.decided" => {
                let option = d["option_id"].as_str().unwrap_or("cancelled");
                self.line(&format!("· permission: {option}"));
            }
            "worker.exited" => self.line(&format!("· worker exited {d}")),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Render, follow};
    use kagu::event::{Event, Source};
    use serde_json::json;

    fn ev(seq: i64, run: &str, kind: &str, data: serde_json::Value) -> Result<Event, String> {
        Ok(Event {
            seq,
            run: run.into(),
            ts: String::new(),
            source: Source::Host,
            kind: kind.into(),
            v: 1,
            data,
        })
    }

    /// An interrupted run has no recorded exit, so following it must stop at
    /// the interruption, not wait for an exit that will never come.
    #[test]
    fn follow_stops_at_an_interrupted_run() {
        let events = vec![
            ev(1, "r", "run.state", json!({"to": "submitted"})),
            ev(2, "r", "worker.spawned", json!({"pid": 1})),
            ev(3, "r", "run.state", json!({"to": "working"})),
            ev(4, "r", "run.state", json!({"to": "interrupted"})),
            Err("followed past the end".into()),
        ];
        let code = follow("r", events.into_iter(), Render::new(true), 0).unwrap();
        assert_eq!(code, std::process::ExitCode::FAILURE);
    }

    /// A finished run that spawned a worker is followed until its exit.
    #[test]
    fn follow_waits_for_the_worker_exit() {
        let events = vec![
            ev(1, "r", "worker.spawned", json!({"pid": 1})),
            ev(2, "r", "run.state", json!({"to": "completed"})),
            Err("stopped before worker.exited".into()),
        ];
        assert!(follow("r", events.into_iter(), Render::new(true), 0).is_err());
    }
}
