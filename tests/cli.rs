//! The CLI against a real host over its unix socket. These need to bind a
//! socket, which an agent sandbox may refuse; they run in CI and through
//! `just test-integration`.

mod support;

use std::io::{BufRead, BufReader};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU32, Ordering};

use kagu::event::{Event, State, fold};
use kagu::store::Store;
use support::Host;

fn scratch() -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "kagu-cli-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn stub_profile(dir: &Path, mode: &str) -> PathBuf {
    let stub = env!("CARGO_BIN_EXE_kagu-stub-agent");
    let path = dir.join("profile.toml");
    std::fs::write(
        &path,
        format!("version = 1\n[worker]\ncommand = [{stub:?}, {mode:?}]\n"),
    )
    .unwrap();
    path
}

fn run_cmd(host: &Host, dir: &Path, mode: &str) -> std::process::Command {
    let mut c = host.cli(&["run", "--json", "--prompt", "go"]);
    c.arg("--profile")
        .arg(stub_profile(dir, mode))
        .arg("--workspace")
        .arg(dir);
    c
}

fn parse(stdout: &[u8]) -> Vec<Event> {
    String::from_utf8_lossy(stdout)
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn stored(host: &Host, run: &str) -> Vec<Event> {
    Store::open(&host.db).unwrap().events(run).unwrap()
}

#[test]
#[ignore = "binds a unix socket; run with `just test-integration`"]
fn run_prints_what_is_stored_and_exits_by_state() {
    let dir = scratch();
    let host = Host::start(&dir, &[]);
    for (mode, code, state) in [
        ("end_turn", 0, State::Completed),
        ("refusal", 1, State::Failed),
    ] {
        let out = run_cmd(&host, &dir, mode).output().unwrap();
        assert_eq!(out.status.code(), Some(code), "{mode}");
        let printed = parse(&out.stdout);
        let run = &printed[0].run;
        assert_eq!(printed, stored(&host, run), "{mode}");
        assert_eq!(fold(&printed).unwrap().state, Some(state));
    }
    drop(host);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
#[ignore = "binds a unix socket; run with `just test-integration`"]
fn watch_replays_history_and_resumes_from_a_cursor() {
    let dir = scratch();
    let host = Host::start(&dir, &[]);
    let out = run_cmd(&host, &dir, "end_turn").output().unwrap();
    let events = parse(&out.stdout);
    let run = events[0].run.clone();

    let all = host.cli(&["watch", &run, "--json"]).output().unwrap();
    assert_eq!(all.status.code(), Some(0));
    assert_eq!(parse(&all.stdout), events);

    let cursor = events[3].seq;
    let rest = host
        .cli(&["watch", &run, "--json", "--after", &cursor.to_string()])
        .output()
        .unwrap();
    let want: Vec<Event> = events.into_iter().filter(|e| e.seq > cursor).collect();
    assert_eq!(parse(&rest.stdout), want);
    drop(host);
    std::fs::remove_dir_all(&dir).unwrap();
}

/// Start `kagu run` on a worker that waits; return it once the worker works.
fn start_hanging(host: &Host, dir: &Path, mode: &str) -> (std::process::Child, String) {
    let mut child = run_cmd(host, dir, mode)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let mut run = String::new();
    for line in lines.by_ref() {
        let e: Event = serde_json::from_str(&line.unwrap()).unwrap();
        run = e.run.clone();
        if e.kind == "worker.update" {
            break;
        }
    }
    // Keep draining so the CLI never blocks on a full pipe.
    std::thread::spawn(move || lines.for_each(drop));
    (child, run)
}

#[test]
#[ignore = "binds a unix socket; run with `just test-integration`"]
fn cancel_from_a_second_cli() {
    let dir = scratch();
    let host = Host::start(&dir, &[]);
    let (mut child, run) = start_hanging(&host, &dir, "hang");
    let out = host.cli(&["cancel", &run]).output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(child.wait().unwrap().code(), Some(1));
    assert_eq!(
        fold(&stored(&host, &run)).unwrap().state,
        Some(State::Canceled)
    );
    drop(host);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
#[ignore = "binds a unix socket; run with `just test-integration`"]
fn a_host_crash_leaves_the_run_interrupted() {
    let dir = scratch();
    let mut host = Host::start(&dir, &[]);
    let (mut child, run) = start_hanging(&host, &dir, "hang");
    host.crash_and_restart();
    let _ = child.wait();
    let out = host.cli(&["log", &run, "--json"]).output().unwrap();
    let events = parse(&out.stdout);
    assert_eq!(fold(&events).unwrap().state, Some(State::Interrupted));
    assert!(
        !events.iter().any(|e| e.kind == "worker.exited"),
        "an exit was invented"
    );
    drop(host);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
#[ignore = "binds a unix socket; run with `just test-integration`"]
fn the_socket_is_private_and_single() {
    let dir = scratch();
    let host = Host::start(&dir, &[]);
    let mode = std::fs::metadata(&host.socket)
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
    let dir_mode = std::fs::metadata(host.socket.parent().unwrap())
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(dir_mode & 0o777, 0o700);
    let second = std::process::Command::new(env!("CARGO_BIN_EXE_kagu"))
        .args(["serve", "--db"])
        .arg(dir.join("other.db"))
        .arg("--socket")
        .arg(&host.socket)
        .output()
        .unwrap();
    assert_eq!(second.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&second.stderr).contains("already serving"));
    let status = host.cli(&["status"]).output().unwrap();
    let status: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(status["api"], 1);
    drop(host);
    std::fs::remove_dir_all(&dir).unwrap();
}
