//! Runs against the stub agent, driven in-process: every row of the
//! stop-reason table, every failure before the prompt (spec: Run lifecycle),
//! redaction, cancel and recovery. The socket and CLI are tested in cli.rs.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use kagu::event::{Event, Reason, State, TaskSubmitted, Verdict, fold};
use kagu::log::Log;
use kagu::run::{self, Cancel};

struct Outcome {
    stored: Vec<Event>,
}

fn scratch() -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "kagu-test-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn profile(dir: &Path, body: &str) -> PathBuf {
    let path = dir.join("profile.toml");
    std::fs::write(&path, body).unwrap();
    path
}

fn stub_profile(dir: &Path, mode: &str, extra: &str) -> PathBuf {
    let stub = env!("CARGO_BIN_EXE_kagu-stub-agent");
    profile(
        dir,
        &format!(
            "version = 1\n[grant]\nsubstrate = \"none\"\n[worker]\ncommand = [{stub:?}, {mode:?}]\n{extra}"
        ),
    )
}

fn task(dir: &Path, profile: &Path) -> TaskSubmitted {
    TaskSubmitted {
        profile: profile.to_str().unwrap().into(),
        workspace: dir.to_str().unwrap().into(),
        input: "do the thing".into(),
        thread: None,
    }
}

/// Submit and run one task to the end, with `env` as the host's environment.
fn kagu(dir: &Path, profile: &Path, env: &[(&str, &str)]) -> Outcome {
    let log = Log::open(&dir.join("kagu.db")).unwrap();
    let id = run::submit(&log, task(dir, profile)).unwrap();
    let lookup = |k: &str| {
        env.iter()
            .find(|(n, _)| *n == k)
            .map(|(_, v)| v.to_string())
    };
    run::start(&log, &id, &Cancel::default(), &lookup).unwrap();
    let stored = log.events(&id).unwrap();
    drop(log);
    std::fs::remove_dir_all(dir).unwrap();
    Outcome { stored }
}

fn run_mode(mode: &str) -> Outcome {
    let dir = scratch();
    let p = stub_profile(&dir, mode, "");
    kagu(&dir, &p, &[])
}

fn expect(o: &Outcome, state: State, reason: Option<Reason>) {
    let view = fold(&o.stored).unwrap();
    assert_eq!(view.state, Some(state), "events: {:#?}", o.stored);
    assert_eq!(view.reason, reason);
    let seqs: Vec<i64> = o.stored.iter().map(|e| e.seq).collect();
    assert!(
        seqs.windows(2).all(|w| w[0] < w[1]),
        "seq not increasing: {seqs:?}"
    );
}

fn kinds(o: &Outcome) -> Vec<&str> {
    o.stored.iter().map(|e| e.kind.as_str()).collect()
}

#[test]
fn end_turn_completes_unverified() {
    let o = run_mode("end_turn");
    expect(&o, State::Completed, None);
    assert_eq!(fold(&o.stored).unwrap().verdict, Verdict::NotRun);
    let k = kinds(&o);
    for want in [
        "task.submitted",
        "run.admitted",
        "worker.spawned",
        "worker.initialized",
        "worker.session",
        "worker.update",
        "run.verdict",
        "worker.exited",
    ] {
        assert!(k.contains(&want), "missing {want} in {k:?}");
    }
    let update = o.stored.iter().find(|e| e.kind == "worker.update").unwrap();
    assert_eq!(update.data["sessionUpdate"], "agent_message_chunk");
    let raw = o.stored.iter().find(|e| e.kind == "worker.raw").unwrap();
    assert_eq!(raw.data["stream"], "stderr");
}

#[test]
fn stop_reasons() {
    for (mode, reason) in [
        ("max_tokens", Reason::Limit),
        ("max_turn_requests", Reason::Limit),
        ("refusal", Reason::Refusal),
        ("cancelled", Reason::UnrequestedCancel),
        ("bogus", Reason::UnknownStopReason),
    ] {
        let o = run_mode(mode);
        expect(&o, State::Failed, Some(reason));
        assert!(!kinds(&o).contains(&"run.verdict"), "{mode}");
    }
}

#[test]
fn failures_before_and_during_the_prompt() {
    for (mode, reason) in [
        ("init_auth", Reason::AuthRequired),
        ("new_auth", Reason::AuthRequired),
        ("prompt_auth", Reason::AuthRequired),
        ("init_error", Reason::Error),
        ("prompt_error", Reason::Error),
        ("version_2", Reason::Error),
        ("new_error", Reason::Error),
        ("no_session", Reason::Error),
        ("exit_at_start", Reason::Exited),
        ("exit_in_new", Reason::Exited),
        ("exit_in_prompt", Reason::Exited),
    ] {
        let o = run_mode(mode);
        expect(&o, State::Failed, Some(reason));
        assert_eq!(kinds(&o).last(), Some(&"worker.exited"), "{mode}");
    }
}

#[test]
fn admission_rejections() {
    let dir = scratch();
    let p = profile(
        &dir,
        "version = 1\n[worker]\ncommand = [\"/nonexistent/agent\"]\n",
    );
    expect(&kagu(&dir, &p, &[]), State::Rejected, Some(Reason::Error));

    let dir = scratch();
    let p = stub_profile(
        &dir,
        "end_turn",
        "[worker.env]\nKEY = \"env:KAGU_TEST_UNSET\"\n",
    );
    let o = kagu(&dir, &p, &[]);
    expect(&o, State::Rejected, Some(Reason::Error));
    assert!(!kinds(&o).contains(&"run.admitted"));

    let dir = scratch();
    let p = profile(
        &dir,
        "version = 1\nsurprise = true\n[worker]\ncommand = [\"x\"]\n",
    );
    expect(&kagu(&dir, &p, &[]), State::Rejected, Some(Reason::Error));
}

fn leak(mode: &str, secret: &str) -> Outcome {
    let dir = scratch();
    let env = "[worker.env]\nSTUB_SECRET = \"env:KAGU_TEST_SECRET\"\n";
    let p = stub_profile(&dir, mode, env);
    kagu(&dir, &p, &[("KAGU_TEST_SECRET", secret)])
}

/// Every string in every event, keys included.
fn strings(v: &serde_json::Value, out: &mut Vec<String>) {
    match v {
        serde_json::Value::String(s) => out.push(s.clone()),
        serde_json::Value::Array(a) => a.iter().for_each(|v| strings(v, out)),
        serde_json::Value::Object(o) => o.iter().for_each(|(k, v)| {
            out.push(k.clone());
            strings(v, out);
        }),
        _ => {}
    }
}

fn assert_absent(o: &Outcome, secret: &str) {
    let escaped = serde_json::to_string(secret).unwrap();
    let escaped = escaped.trim_matches('"');
    let mut all = Vec::new();
    for e in &o.stored {
        strings(&e.data, &mut all);
    }
    for s in &all {
        assert!(
            !s.contains(secret) && !s.contains(escaped),
            "secret in log: {s}"
        );
    }
}

#[test]
fn credentials_never_reach_the_log() {
    // A quote and a backslash: JSON escaping must not hide the value.
    let secret = r#"s3c"ret\value-123"#;
    // agentInfo, sessionId, an update, a raw notification, stderr, plain
    // stdout, an error message.
    let o = leak("leak", secret);
    expect(&o, State::Failed, Some(Reason::Error));
    assert_absent(&o, secret);
    let admitted = o.stored.iter().find(|e| e.kind == "run.admitted").unwrap();
    assert_eq!(admitted.data["env"]["STUB_SECRET"], "env:KAGU_TEST_SECRET");
    let session = o
        .stored
        .iter()
        .find(|e| e.kind == "worker.session")
        .unwrap();
    assert_eq!(session.data["session_id"], "env:KAGU_TEST_SECRET");

    // JSON on stderr, non-object JSON on stdout, an object key, a stop reason.
    let o = leak("leak_stop", secret);
    expect(&o, State::Failed, Some(Reason::UnknownStopReason));
    assert_absent(&o, secret);
}

#[test]
fn a_numeric_secret_is_redacted_in_json_lines() {
    let o = leak("leak_number", "987654321");
    expect(&o, State::Completed, None);
    assert_absent(&o, "987654321");
}

#[test]
fn a_short_secret_does_not_rewrite_kagus_own_fields() {
    // "e" occurs in "exited", "failed", "run.state" and more.
    let o = leak("leak_stop", "e");
    expect(&o, State::Failed, Some(Reason::UnknownStopReason));
    let kinds: Vec<&str> = o.stored.iter().map(|e| e.kind.as_str()).collect();
    assert!(kinds.contains(&"worker.exited") && kinds.contains(&"run.admitted"));
}

#[test]
fn exit_is_seen_while_a_descendant_holds_stdout() {
    let started = std::time::Instant::now();
    let o = run_mode("orphan_exit");
    expect(&o, State::Failed, Some(Reason::Exited));
    // The descendant sleeps 10 s; the run must not wait for it.
    assert!(
        started.elapsed() < std::time::Duration::from_secs(8),
        "{:?}",
        started.elapsed()
    );
}

#[test]
fn invalid_utf8_does_not_end_the_stream() {
    let o = run_mode("bad_utf8");
    expect(&o, State::Completed, None);
    let raw = o
        .stored
        .iter()
        .find(|e| e.data["stream"] == "stderr")
        .unwrap();
    assert!(raw.data["text"].as_str().unwrap().contains('\u{FFFD}'));
}

#[test]
fn permission_is_refused_and_recorded_before_the_answer() {
    let o = run_mode("ask");
    expect(&o, State::Completed, None);
    let decided = o
        .stored
        .iter()
        .position(|e| e.kind == "permission.decided")
        .unwrap();
    assert_eq!(o.stored[decided].data["option_id"], "no");
    // The stub reports the answer it received in its next update.
    let answer = o.stored[decided..]
        .iter()
        .find(|e| e.kind == "worker.update")
        .unwrap();
    let got: serde_json::Value =
        serde_json::from_str(answer.data["content"]["text"].as_str().unwrap()).unwrap();
    assert_eq!(got["result"]["outcome"]["optionId"], "no");
}

#[test]
fn unknown_requests_get_method_not_found() {
    let o = run_mode("ask_unknown");
    expect(&o, State::Completed, None);
    let answer = o
        .stored
        .iter()
        .rfind(|e| e.kind == "worker.update")
        .unwrap();
    let got: serde_json::Value =
        serde_json::from_str(answer.data["content"]["text"].as_str().unwrap()).unwrap();
    assert_eq!(got["error"]["code"], -32601);
}

#[test]
fn non_json_stdout_is_kept_raw() {
    let o = run_mode("noise");
    expect(&o, State::Completed, None);
    assert!(
        o.stored
            .iter()
            .any(|e| e.data["stream"] == "stdout" && e.data["text"] == "not json")
    );
}

#[test]
fn admission_records_levels_and_digest() {
    let o = run_mode("end_turn");
    let a = &o
        .stored
        .iter()
        .find(|e| e.kind == "run.admitted")
        .unwrap()
        .data;
    assert_eq!(a["substrate"], "none");
    assert_eq!(a["levels"]["network"], "unrestricted");
    assert_eq!(a["levels"]["filesystem"], "unsupported");
    assert_eq!(a["profile_digest"].as_str().unwrap().len(), 64);
}

/// Start a run on its own thread; return once its worker has a session.
fn start_hanging(
    dir: &Path,
    mode: &str,
) -> (
    std::sync::Arc<Log>,
    String,
    std::sync::Arc<Cancel>,
    std::thread::JoinHandle<()>,
) {
    let p = stub_profile(dir, mode, "");
    let log = std::sync::Arc::new(Log::open(&dir.join("kagu.db")).unwrap());
    let id = run::submit(&log, task(dir, &p)).unwrap();
    let cancel = std::sync::Arc::new(Cancel::default());
    let handle = {
        let (log, id, cancel) = (log.clone(), id.clone(), cancel.clone());
        std::thread::spawn(move || run::start(&log, &id, &cancel, &|_| None).unwrap())
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !log
        .events(&id)
        .unwrap()
        .iter()
        .any(|e| e.kind == "worker.update")
    {
        assert!(std::time::Instant::now() < deadline, "worker never started");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    (log, id, cancel, handle)
}

#[test]
fn cancel_asks_the_worker_to_stop() {
    let dir = scratch();
    let (log, id, cancel, handle) = start_hanging(&dir, "hang");
    cancel.request();
    handle.join().unwrap();
    let o = Outcome {
        stored: log.events(&id).unwrap(),
    };
    expect(&o, State::Canceled, None);
    let exited = o.stored.iter().find(|e| e.kind == "worker.exited").unwrap();
    assert_eq!(exited.data["code"], 0, "the worker stopped itself");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn cancel_kills_a_worker_that_ignores_it() {
    let dir = scratch();
    let (log, id, cancel, handle) = start_hanging(&dir, "hang_hard");
    let asked = std::time::Instant::now();
    cancel.request();
    handle.join().unwrap();
    // The grace period, then the kill.
    assert!(asked.elapsed() < run::GRACE * 3, "{:?}", asked.elapsed());
    let o = Outcome {
        stored: log.events(&id).unwrap(),
    };
    expect(&o, State::Canceled, None);
    let exited = o.stored.iter().find(|e| e.kind == "worker.exited").unwrap();
    assert_eq!(exited.data["signal"], 9);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn cancel_before_start_never_spawns() {
    let dir = scratch();
    let p = stub_profile(&dir, "end_turn", "");
    let log = Log::open(&dir.join("kagu.db")).unwrap();
    let id = run::submit(&log, task(&dir, &p)).unwrap();
    let cancel = Cancel::default();
    cancel.request();
    run::start(&log, &id, &cancel, &|_| None).unwrap();
    let o = Outcome {
        stored: log.events(&id).unwrap(),
    };
    expect(&o, State::Canceled, None);
    assert!(!kinds(&o).contains(&"worker.spawned"));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn recovery_interrupts_unfinished_runs_only() {
    let dir = scratch();
    let p = stub_profile(&dir, "end_turn", "");
    let db = dir.join("kagu.db");
    let (done, pending) = {
        let log = Log::open(&db).unwrap();
        let done = run::submit(&log, task(&dir, &p)).unwrap();
        run::start(&log, &done, &Cancel::default(), &|_| None).unwrap();
        (done, run::submit(&log, task(&dir, &p)).unwrap())
    };
    // A host starting on this log.
    let log = Log::open(&db).unwrap();
    kagu::host::recover(&log).unwrap();
    let view = |id: &str| fold(&log.events(id).unwrap()).unwrap().state;
    assert_eq!(view(&done), Some(State::Completed));
    assert_eq!(view(&pending), Some(State::Interrupted));
    let last = log.events(&pending).unwrap().pop().unwrap();
    assert_eq!(last.source, kagu::event::Source::Host);
    drop(log);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_second_host_on_a_live_log_is_refused_before_it_writes() {
    let dir = scratch();
    let p = stub_profile(&dir, "end_turn", "");
    let db = dir.join("kagu.db");
    // The first host: holds the lock, with a run it has not finished.
    let _lock = kagu::host::lock_db(&db).unwrap();
    let log = Log::open(&db).unwrap();
    let id = run::submit(&log, task(&dir, &p)).unwrap();
    let before = log.events(&id).unwrap();
    let second = std::process::Command::new(env!("CARGO_BIN_EXE_kagu"))
        .arg("serve")
        .arg("--db")
        .arg(&db)
        .arg("--socket")
        .arg(dir.join("second.sock"))
        .output()
        .unwrap();
    assert_eq!(second.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&second.stderr);
    assert!(stderr.contains("another kagu host"), "{stderr}");
    assert_eq!(log.events(&id).unwrap(), before, "the second host wrote");
    drop(log);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn an_existing_socket_directory_is_never_changed() {
    use std::os::unix::fs::PermissionsExt;
    let dir = scratch();
    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    let serve = |sock: &Path| {
        std::process::Command::new(env!("CARGO_BIN_EXE_kagu"))
            .arg("serve")
            .arg("--db")
            .arg(dir.join("kagu.db"))
            .arg("--socket")
            .arg(sock)
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap()
    };
    // Writable by others: refused, and left as it was.
    let open = dir.join("open");
    std::fs::create_dir(&open).unwrap();
    std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o777)).unwrap();
    let out = serve(&open.join("kagu.sock")).wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("not private to this user"));
    assert_eq!(mode(&open), 0o777);
    // Private but not 0700: used as it is, never tightened. Where the bind
    // works the host keeps serving, so stop it once it has bound or exited.
    let shared = dir.join("shared");
    std::fs::create_dir(&shared).unwrap();
    std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o755)).unwrap();
    let sock = shared.join("kagu.sock");
    let mut host = serve(&sock);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !sock.exists() && host.try_wait().unwrap().is_none() {
        assert!(
            std::time::Instant::now() < deadline,
            "host neither bound nor exited"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let _ = host.kill();
    let _ = host.wait();
    assert_eq!(mode(&shared), 0o755);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_read_only_path_over_the_workspace_is_rejected() {
    let dir = scratch();
    let stub = env!("CARGO_BIN_EXE_kagu-stub-agent");
    let parent = dir.parent().unwrap().to_str().unwrap();
    let p = profile(
        &dir,
        &format!(
            "version = 1\n[grant]\nsubstrate = \"none\"\nro = [{parent:?}]\n[worker]\ncommand = [{stub:?}, \"end_turn\"]\n"
        ),
    );
    let o = kagu(&dir, &p, &[]);
    expect(&o, State::Rejected, Some(Reason::Error));
    let last = o
        .stored
        .iter()
        .rev()
        .find(|e| e.kind == "run.state")
        .unwrap();
    let detail = last.data["detail"].as_str().unwrap();
    assert!(detail.contains("contains the workspace"), "{detail}");
}

/// A stub profile with a harness home, named so its home is predictable.
fn home_profile(dir: &Path, name: &str, mode: &str, worker_extra: &str) -> PathBuf {
    let stub = env!("CARGO_BIN_EXE_kagu-stub-agent");
    let path = dir.join(format!("{name}.toml"));
    std::fs::write(
        &path,
        format!(
            "version = 1\n[grant]\nsubstrate = \"none\"\n[worker]\ncommand = [{stub:?}, {mode:?}]\nhome_env = [\"STUB_HOME\"]\n{worker_extra}"
        ),
    )
    .unwrap();
    path
}

/// Run in-process with `state` as the host's XDG_STATE_HOME.
fn run_with_state(dir: &Path, profile: &Path, state: &Path) -> Vec<Event> {
    let log = Log::open(&dir.join("kagu.db")).unwrap();
    let id = run::submit(&log, task(dir, profile)).unwrap();
    let state = state.to_str().unwrap().to_string();
    let lookup = move |k: &str| (k == "XDG_STATE_HOME").then(|| state.clone());
    run::start(&log, &id, &Cancel::default(), &lookup).unwrap();
    log.events(&id).unwrap()
}

/// `kagu auth` against `profile` with `state` as XDG_STATE_HOME.
fn auth_cli(profile: &Path, state: &Path) {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_kagu"))
        .args(["auth"])
        .arg(profile)
        .env("XDG_STATE_HOME", state)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The stub's report of what it found in its home.
fn home_report(events: &[Event]) -> serde_json::Value {
    let update = events.iter().find(|e| e.kind == "worker.update").unwrap();
    serde_json::from_str(update.data["content"]["text"].as_str().unwrap()).unwrap()
}

const PIN: &str = "[worker.home_files]\n\"settings.json\" = \"{}\"\n";

#[test]
fn auth_signs_the_worker_in_to_its_own_home() {
    use std::os::unix::fs::PermissionsExt;
    let dir = scratch();
    let p = home_profile(&dir, "authstub", "auth", PIN);
    // Before sign-in the worker asks for authentication.
    let before = Outcome {
        stored: run_with_state(&dir, &p, &dir.join("unsigned")),
    };
    expect(&before, State::Failed, Some(Reason::AuthRequired));
    // Sign-in creates the home itself.
    let state = dir.join("state");
    auth_cli(&p, &state);
    let home = state
        .join("kagu/homes")
        .join(kagu::profile::home_key(&p).unwrap());
    assert_eq!(
        std::fs::read_to_string(home.join("credentials")).unwrap(),
        "t0k"
    );
    let mode = std::fs::metadata(&home).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o700);
    // After it, the same profile runs in that home.
    let after = Outcome {
        stored: run_with_state(&dir, &p, &state),
    };
    expect(&after, State::Completed, None);
    let admitted = after
        .stored
        .iter()
        .find(|e| e.kind == "run.admitted")
        .unwrap();
    assert_eq!(admitted.data["home"], home.to_str().unwrap());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn home_files_are_rewritten_before_every_run() {
    let dir = scratch();
    let state = dir.join("state");
    let p = home_profile(&dir, "pinned", "auth", PIN);
    auth_cli(&p, &state);
    // Unconfined, the first run can plant over the file...
    let first = run_with_state(&dir, &p, &state);
    assert_eq!(home_report(&first)["settings"], "{}");
    assert_eq!(home_report(&first)["planted"], true);
    // ...but the next run starts from what the profile says.
    let second = run_with_state(&dir, &p, &state);
    assert_eq!(home_report(&second)["settings"], "{}");
    // A link left in its place is replaced, not followed.
    let home = state
        .join("kagu/homes")
        .join(kagu::profile::home_key(&p).unwrap());
    let target = dir.join("target");
    std::fs::write(&target, "keep").unwrap();
    std::fs::remove_file(home.join("settings.json")).unwrap();
    std::os::unix::fs::symlink(&target, home.join("settings.json")).unwrap();
    let third = run_with_state(&dir, &p, &state);
    assert_eq!(home_report(&third)["settings"], "{}");
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "keep");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn profiles_with_one_file_name_get_different_homes() {
    let dir = scratch();
    std::fs::create_dir_all(dir.join("a")).unwrap();
    std::fs::create_dir_all(dir.join("b")).unwrap();
    let a = home_profile(&dir.join("a"), "same", "end_turn", "");
    let b = home_profile(&dir.join("b"), "same", "end_turn", "");
    let state = dir.join("state");
    let home = |p: &Path| {
        let events = run_with_state(&dir, p, &state);
        let admitted = events.iter().find(|e| e.kind == "run.admitted").unwrap();
        admitted.data["home"].as_str().unwrap().to_string()
    };
    assert_ne!(home(&a), home(&b));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_grant_that_makes_the_home_read_only_is_rejected() {
    let dir = scratch();
    let state = dir.join("state");
    let ro = format!("[grant]\nro = [{:?}]\n", state.to_str().unwrap());
    // home_profile writes its own [grant]; this one replaces it.
    let stub = env!("CARGO_BIN_EXE_kagu-stub-agent");
    let p = dir.join("rohome.toml");
    std::fs::write(
        &p,
        format!("version = 1\n{ro}substrate = \"none\"\n[worker]\ncommand = [{stub:?}, \"end_turn\"]\nhome_env = [\"STUB_HOME\"]\n"),
    )
    .unwrap();
    let o = Outcome {
        stored: run_with_state(&dir, &p, &state),
    };
    expect(&o, State::Rejected, Some(Reason::Error));
    let last = o
        .stored
        .iter()
        .rev()
        .find(|e| e.kind == "run.state")
        .unwrap();
    assert!(
        last.data["detail"]
            .as_str()
            .unwrap()
            .contains("contains the harness home")
    );
    // kagu auth refuses it too.
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_kagu"))
        .arg("auth")
        .arg(&p)
        .env("XDG_STATE_HOME", &state)
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("contains the harness home"));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn system_prompt_and_meta_reach_the_worker() {
    let dir = scratch();
    let extra = "[worker.system_prompt]\nmode = \"append\"\ntext = \"be terse\"\n\
                 [worker.meta.claudeCode.options]\nsettingSources = []\n";
    let p = home_profile(&dir, "echo", "echo_new", extra);
    let events = run_with_state(&dir, &p, &dir.join("state"));
    let update = events.iter().find(|e| e.kind == "worker.update").unwrap();
    let params: serde_json::Value =
        serde_json::from_str(update.data["content"]["text"].as_str().unwrap()).unwrap();
    assert_eq!(params["systemPrompt"]["mode"], "append");
    assert_eq!(params["systemPrompt"]["content"][0]["text"], "be terse");
    assert_eq!(
        params["_meta"]["claudeCode"]["options"]["settingSources"],
        serde_json::json!([])
    );
    let admitted = events.iter().find(|e| e.kind == "run.admitted").unwrap();
    assert_eq!(admitted.data["system_prompt"]["text"], "be terse");
    assert_eq!(
        admitted.data["meta"]["claudeCode"]["options"]["settingSources"],
        serde_json::json!([])
    );
    for setting in ["system_prompt", "meta", "home_isolation"] {
        assert_eq!(admitted.data["levels"][setting], "advisory", "{setting}");
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn the_example_claude_profile_loads() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/claude-code.toml");
    let loaded = kagu::profile::load(&path).unwrap();
    assert_eq!(loaded.profile.worker.home_env, ["CLAUDE_CONFIG_DIR"]);
}

/// A worker with one pinned file, and a grant with `substrate`.
fn pinned(substrate: &str) -> (kagu::profile::Worker, kagu::profile::Grant) {
    let p: kagu::profile::Profile = toml::from_str(&format!(
        "version = 1\n[grant]\nsubstrate = {substrate:?}\n[worker]\ncommand = [\"x\"]\nhome_env = [\"H\"]\n[worker.home_files]\n\"settings.json\" = \"{{}}\"\n"
    ))
    .unwrap();
    (p.worker, p.grant)
}

#[test]
fn runs_of_one_profile_prepare_its_home_concurrently() {
    let dir = scratch();
    let profile = home_profile(&dir, "busy", "end_turn", "");
    let state = dir.join("state").to_str().unwrap().to_string();
    for substrate in ["none", "bwrap"] {
        let (worker, grant) = pinned(substrate);
        std::thread::scope(|s| {
            for _ in 0..4 {
                s.spawn(|| {
                    let env = |k: &str| (k == "XDG_STATE_HOME").then(|| state.clone());
                    for _ in 0..50 {
                        kagu::profile::prepare_home(&env, &profile, &worker, &grant).unwrap();
                    }
                });
            }
        });
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn under_bwrap_a_pinned_file_in_the_home_is_never_replaced() {
    use std::os::unix::fs::MetadataExt;
    let dir = scratch();
    let profile = home_profile(&dir, "bound", "end_turn", "");
    let state = dir.join("state").to_str().unwrap().to_string();
    let env = |k: &str| (k == "XDG_STATE_HOME").then(|| state.clone());
    let (worker, grant) = pinned("bwrap");
    let home = kagu::profile::prepare_home(&env, &profile, &worker, &grant).unwrap();
    let dest = Path::new(&home.path).join("settings.json");
    // Another run's bind may sit on this file: replacing it would detach it.
    let inode = std::fs::metadata(&dest).unwrap().ino();
    std::fs::write(&dest, "planted").unwrap();
    let again = kagu::profile::prepare_home(&env, &profile, &worker, &grant).unwrap();
    assert_eq!(std::fs::metadata(&dest).unwrap().ino(), inode);
    // What the run sees is the bind's source, which holds the pinned text.
    let (source, target) = &again.pins[0];
    assert_eq!(target, dest.to_str().unwrap());
    assert_eq!(std::fs::read_to_string(source).unwrap(), "{}");
    // A link in its place is replaced by a file, and its target untouched.
    let outside = dir.join("outside");
    std::fs::write(&outside, "keep").unwrap();
    std::fs::remove_file(&dest).unwrap();
    std::os::unix::fs::symlink(&outside, &dest).unwrap();
    kagu::profile::prepare_home(&env, &profile, &worker, &grant).unwrap();
    assert!(
        !std::fs::symlink_metadata(&dest)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(std::fs::read_to_string(&outside).unwrap(), "keep");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_tampered_pin_source_is_rewritten() {
    let dir = scratch();
    let profile = home_profile(&dir, "tampered", "end_turn", "");
    let state = dir.join("state").to_str().unwrap().to_string();
    let env = |k: &str| (k == "XDG_STATE_HOME").then(|| state.clone());
    let (worker, grant) = pinned("bwrap");
    let first = kagu::profile::prepare_home(&env, &profile, &worker, &grant).unwrap();
    let source = first.pins[0].0.clone();
    std::fs::write(&source, "planted").unwrap();
    kagu::profile::prepare_home(&env, &profile, &worker, &grant).unwrap();
    assert_eq!(std::fs::read_to_string(&source).unwrap(), "{}");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_confined_grant_that_reaches_kagu_state_is_rejected() {
    let dir = scratch();
    let state = dir.join("state");
    let stub = env!("CARGO_BIN_EXE_kagu-stub-agent");
    // Rejected before any sandbox is built, so this runs without bwrap.
    for extra in [
        format!("rw = [{:?}]", state.to_str().unwrap()),
        format!("ro = [{:?}]", state.join("kagu/homes").to_str().unwrap()),
        format!("rw = [{:?}]", dir.to_str().unwrap()),
    ] {
        let p = profile(
            &dir,
            &format!(
                "version = 1\n[grant]\nsubstrate = \"bwrap\"\n{extra}\n[worker]\ncommand = [{stub:?}, \"end_turn\"]\n"
            ),
        );
        let o = Outcome {
            stored: run_with_state(&dir, &p, &state),
        };
        expect(&o, State::Rejected, Some(Reason::Error));
        let last = o
            .stored
            .iter()
            .rev()
            .find(|e| e.kind == "run.state")
            .unwrap();
        let detail = last.data["detail"].as_str().unwrap();
        assert!(detail.contains("reaches kagu's state"), "{extra}: {detail}");
    }
    // kagu auth refuses it too.
    let p = profile(
        &dir,
        &format!(
            "version = 1\n[grant]\nsubstrate = \"bwrap\"\nrw = [{:?}]\n[worker]\ncommand = [{stub:?}, \"auth\"]\nhome_env = [\"STUB_HOME\"]\n",
            state.to_str().unwrap()
        ),
    );
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_kagu"))
        .arg("auth")
        .arg(&p)
        .env("XDG_STATE_HOME", &state)
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("reaches kagu's state"));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_grant_spelled_through_links_or_dots_still_cannot_reach_kagu_state() {
    let dir = scratch();
    let real = dir.join("real");
    std::fs::create_dir_all(real.join("kagu")).unwrap();
    std::fs::create_dir_all(dir.join("other")).unwrap();
    std::os::unix::fs::symlink(&real, dir.join("link")).unwrap();
    std::os::unix::fs::symlink(real.join("kagu"), dir.join("tostate")).unwrap();
    let grant = |path: &Path| kagu::profile::Grant {
        substrate: kagu::profile::Substrate::Bwrap,
        rw: vec![path.to_str().unwrap().to_string()],
        ..Default::default()
    };
    let d = dir.to_str().unwrap().to_string();
    for (state, path) in [
        // XDG_STATE_HOME spelled through a link, the grant spelled the same.
        (format!("{d}/link"), dir.join("link/kagu")),
        // XDG_STATE_HOME through a link, the grant its real path.
        (format!("{d}/link"), dir.join("real/kagu")),
        (format!("{d}/real"), dir.join("other/../real/kagu")),
        (format!("{d}/real"), dir.join("tostate")),
    ] {
        let env = |k: &str| (k == "XDG_STATE_HOME").then(|| state.clone());
        let err = kagu::profile::keep_out_of_state(&env, &grant(&path), None).unwrap_err();
        assert!(err.contains("reaches kagu's state"), "{path:?}: {err}");
    }
    // A sibling is fine.
    let env = |k: &str| (k == "XDG_STATE_HOME").then(|| format!("{d}/real"));
    kagu::profile::keep_out_of_state(&env, &grant(&dir.join("other")), None).unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
}
