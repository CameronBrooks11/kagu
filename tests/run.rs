//! `kagu run` against the stub agent: every row of the stop-reason table and
//! every failure before the prompt (spec: Run lifecycle).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

use kagu::event::{Event, Reason, State, Verdict, fold};
use kagu::store::Store;

struct Outcome {
    exit: i32,
    printed: Vec<Event>,
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
        &format!("version = 1\n[worker]\ncommand = [{stub:?}, {mode:?}]\n{extra}"),
    )
}

fn kagu(dir: &Path, profile: &Path, env: &[(&str, &str)]) -> Outcome {
    let db = dir.join("kagu.db");
    let out = Command::new(env!("CARGO_BIN_EXE_kagu"))
        .args(["run", "--profile"])
        .arg(profile)
        .args(["--workspace"])
        .arg(dir)
        .args(["--prompt", "do the thing", "--db"])
        .arg(&db)
        .envs(env.iter().copied())
        .output()
        .unwrap();
    let printed: Vec<Event> = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let run = printed.first().map(|e| e.run.clone()).unwrap_or_default();
    let stored = Store::open(&db).unwrap().events(&run).unwrap();
    std::fs::remove_dir_all(dir).unwrap();
    Outcome {
        exit: out.status.code().unwrap(),
        printed,
        stored,
    }
}

fn run_mode(mode: &str) -> Outcome {
    let dir = scratch();
    let p = stub_profile(&dir, mode, "");
    kagu(&dir, &p, &[])
}

fn expect(o: &Outcome, state: State, reason: Option<Reason>) {
    let view = fold(&o.stored).unwrap();
    assert_eq!(view.state, Some(state), "events: {:#?}", o.printed);
    assert_eq!(view.reason, reason);
    // What was printed is what was stored, and the fold of either agrees.
    assert_eq!(o.printed, o.stored);
    let seqs: Vec<i64> = o.stored.iter().map(|e| e.seq).collect();
    assert!(
        seqs.windows(2).all(|w| w[0] < w[1]),
        "seq not increasing: {seqs:?}"
    );
    assert_eq!(o.exit, if state == State::Completed { 0 } else { 1 });
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
        ("exit_at_start", Reason::Exited),
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

#[test]
fn credentials_are_references_and_redacted() {
    let dir = scratch();
    let p = stub_profile(
        &dir,
        "echo_secret",
        "[worker.env]\nSTUB_SECRET = \"env:KAGU_TEST_SECRET\"\n",
    );
    let secret = "s3cret-value-123";
    let o = kagu(&dir, &p, &[("KAGU_TEST_SECRET", secret)]);
    expect(&o, State::Completed, None);
    let all = serde_json::to_string(&o.stored).unwrap();
    assert!(!all.contains(secret), "secret value reached the log");
    let admitted = o.stored.iter().find(|e| e.kind == "run.admitted").unwrap();
    assert_eq!(admitted.data["env"]["STUB_SECRET"], "env:KAGU_TEST_SECRET");
    let update = o.stored.iter().find(|e| e.kind == "worker.update").unwrap();
    assert_eq!(update.data["content"]["text"], "env:KAGU_TEST_SECRET");
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
