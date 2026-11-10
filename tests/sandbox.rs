//! Confinement through a real host: bwrap + Landlock around the stub agent.
//! These need user namespaces and a unix socket, which an agent sandbox may
//! refuse; they run in CI and through `just test-integration`.

mod support;

use std::os::linux::net::SocketAddrExt;
use std::os::unix::net::{SocketAddr, UnixListener};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use kagu::event::{Event, State, fold};
use serde_json::{Value, json};
use support::Host;

fn scratch() -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "kagu-sandbox-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Run the stub's `probe` mode confined, with `request` as its prompt.
fn probe(dir: &Path, network: &str, request: &Value, env: &[(&str, &str)]) -> Vec<Event> {
    probe_with(dir, network, "", request, env)
}

fn probe_with(
    dir: &Path,
    network: &str,
    grant_extra: &str,
    request: &Value,
    env: &[(&str, &str)],
) -> Vec<Event> {
    let stub = env!("CARGO_BIN_EXE_kagu-stub-agent");
    let workspace = dir.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let profile = dir.join("profile.toml");
    std::fs::write(
        &profile,
        format!(
            "version = 1\n[grant]\nsubstrate = \"bwrap\"\nnetwork = {network:?}\n{grant_extra}\n[worker]\ncommand = [{stub:?}, \"probe\"]\n"
        ),
    )
    .unwrap();
    let host = Host::start(dir, env);
    let out = host
        .cli(&["run", "--json", "--prompt", &request.to_string()])
        .arg("--profile")
        .arg(&profile)
        .arg("--workspace")
        .arg(&workspace)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn report(events: &[Event]) -> Value {
    let update = events
        .iter()
        .find(|e| e.kind == "worker.update")
        .unwrap_or_else(|| panic!("no report: {events:#?}"));
    serde_json::from_str(update.data["content"]["text"].as_str().unwrap()).unwrap()
}

fn admitted(events: &[Event]) -> &Value {
    &events
        .iter()
        .find(|e| e.kind == "run.admitted")
        .unwrap()
        .data
}

/// Whether any process on this host has `marker` in its command line.
fn alive(marker: &str) -> bool {
    std::fs::read_dir("/proc").unwrap().flatten().any(|p| {
        std::fs::read(p.path().join("cmdline"))
            .map(|c| String::from_utf8_lossy(&c).contains(marker))
            .unwrap_or(false)
    })
}

#[test]
#[ignore = "needs user namespaces and a unix socket; run with `just test-integration`"]
fn a_confined_worker_reaches_only_its_grant() {
    let dir = scratch();
    let outside = dir.join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("secret"), "s").unwrap();
    let name = format!("kagu-test-{}", std::process::id());
    let _listener =
        UnixListener::bind_addr(&SocketAddr::from_abstract_name(&name).unwrap()).unwrap();
    let marker = format!("kagu-marker-{}", std::process::id());
    let request = json!({
        "outside": outside.join("secret"),
        "outside_new": outside.join("new"),
        "abstract": name,
        "marker": marker,
    });
    let events = probe(&dir, "host", &request, &[("KAGU_TEST_LEAK", "1")]);
    assert_eq!(
        fold(&events).unwrap().state,
        Some(State::Completed),
        "{events:#?}"
    );
    let a = admitted(&events);
    assert_eq!(a["substrate"], "bwrap");
    assert_eq!(a["levels"]["filesystem"], "enforced");
    assert_eq!(a["levels"]["process_tree"], "enforced");
    assert_eq!(a["levels"]["network"], "unrestricted");
    let r = report(&events);
    assert_eq!(r["read_outside"], false, "{r}");
    assert_eq!(r["write_outside"], false, "{r}");
    assert_eq!(r["write_workspace"], true, "{r}");
    assert_eq!(r["env_leaked"], false, "{r}");
    // Landlock alone forbids this: no mount hides /proc/self.
    assert_eq!(r["write_proc_comm"], false, "{r}");
    // These tests need a kernel with Landlock ABI 6 (Linux 6.12), where
    // credentials are enforced by scoping abstract unix sockets.
    assert_eq!(a["levels"]["credentials"], "enforced");
    assert_eq!(r["abstract_socket"], false, "{r}");
    // Positive control for the network-off test: here the network works.
    assert_eq!(r["network"], true, "{r}");
    assert!(!outside.join("new").exists());
    assert!(dir.join("workspace/written-by-stub").exists());
    // The run ended, so its whole tree is gone.
    assert!(!alive(&marker), "a descendant outlived its run");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
#[ignore = "needs user namespaces and a unix socket; run with `just test-integration`"]
fn network_off_is_enforced() {
    let dir = scratch();
    let request = json!({"abstract": "unused", "marker": "kagu-unused"});
    let events = probe(&dir, "off", &request, &[]);
    let a = admitted(&events);
    assert_eq!(a["levels"]["network"], "enforced");
    assert_eq!(a["levels"]["credentials"], "enforced");
    assert_eq!(report(&events)["network"], false);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
#[ignore = "needs user namespaces and a unix socket; run with `just test-integration`"]
fn a_sandbox_that_cannot_start_is_rejected() {
    let dir = scratch();
    // bwrap refuses to bind a path that does not exist.
    let extra = "ro = [\"/nonexistent/kagu-test\"]";
    let request = json!({"abstract": "unused", "marker": "kagu-unused"});
    let events = probe_with(&dir, "host", extra, &request, &[]);
    let view = fold(&events).unwrap();
    assert_eq!(view.state, Some(State::Rejected), "{events:#?}");
    assert!(
        !events.iter().any(|e| e.data["to"] == "working"),
        "reached working"
    );
    let last = events.iter().rev().find(|e| e.kind == "run.state").unwrap();
    let detail = last.data["detail"].as_str().unwrap();
    assert!(detail.contains("the sandbox did not start"), "{detail}");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
#[ignore = "needs user namespaces and a unix socket; run with `just test-integration`"]
fn permissions_are_allowed_inside_an_enforced_sandbox() {
    let dir = scratch();
    let stub = env!("CARGO_BIN_EXE_kagu-stub-agent");
    let workspace = dir.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let profile = dir.join("profile.toml");
    std::fs::write(
        &profile,
        format!(
            "version = 1\n[grant]\nsubstrate = \"bwrap\"\n[worker]\ncommand = [{stub:?}, \"ask\"]\n"
        ),
    )
    .unwrap();
    let host = Host::start(&dir, &[]);
    let out = host
        .cli(&["run", "--json", "--prompt", "go"])
        .arg("--profile")
        .arg(&profile)
        .arg("--workspace")
        .arg(&workspace)
        .output()
        .unwrap();
    let events: Vec<Event> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(
        fold(&events).unwrap().state,
        Some(State::Completed),
        "{events:#?}"
    );
    let decided = events
        .iter()
        .find(|e| e.kind == "permission.decided")
        .unwrap();
    assert_eq!(decided.data["option_id"], "yes");
    drop(host);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
#[ignore = "needs user namespaces and a unix socket; run with `just test-integration`"]
fn the_home_is_writable_and_its_pinned_files_are_not() {
    let dir = scratch();
    let stub = env!("CARGO_BIN_EXE_kagu-stub-agent");
    let workspace = dir.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let state = dir.join("state");
    let profile = dir.join("homed.toml");
    std::fs::write(
        &profile,
        format!(
            "version = 1\n[grant]\nsubstrate = \"bwrap\"\n[worker]\ncommand = [{stub:?}, \"auth\"]\nhome_env = [\"STUB_HOME\"]\n[worker.home_files]\n\"settings.json\" = \"{{}}\"\n"
        ),
    )
    .unwrap();
    // Sign in confined: the login writes its credential into the home.
    let auth = std::process::Command::new(env!("CARGO_BIN_EXE_kagu"))
        .arg("auth")
        .arg(&profile)
        .env("XDG_STATE_HOME", &state)
        .output()
        .unwrap();
    assert!(
        auth.status.success(),
        "{}",
        String::from_utf8_lossy(&auth.stderr)
    );

    // The login tried to plant the pinned file: bound, the host's copy is untouched.
    let home = state
        .join("kagu/homes")
        .join(kagu::profile::home_key(&profile).unwrap());
    assert_ne!(
        std::fs::read_to_string(home.join("settings.json")).unwrap(),
        "planted"
    );
    let host = Host::start(&dir, &[("XDG_STATE_HOME", state.to_str().unwrap())]);
    let out = host
        .cli(&["run", "--json", "--prompt", "go"])
        .arg("--profile")
        .arg(&profile)
        .arg("--workspace")
        .arg(&workspace)
        .output()
        .unwrap();
    let events: Vec<Event> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(
        fold(&events).unwrap().state,
        Some(State::Completed),
        "{events:#?}"
    );
    let r = report(&events);
    assert_eq!(r["settings"], "{}", "{r}");
    assert_eq!(r["transcript"], true, "the home is not writable: {r}");
    assert_eq!(r["planted"], false, "a pinned file was writable: {r}");
    drop(host);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
#[ignore = "needs user namespaces and a unix socket; run with `just test-integration`"]
fn a_hidden_path_is_empty_inside_the_sandbox() {
    let dir = scratch();
    let shared = dir.join("shared");
    std::fs::create_dir_all(shared.join("hidden")).unwrap();
    std::fs::write(shared.join("hidden/secret"), "s").unwrap();
    std::fs::write(shared.join("visible"), "v").unwrap();
    let extra = format!(
        "ro = [{:?}]\nhide = [{:?}]",
        shared.to_str().unwrap(),
        shared.join("hidden").to_str().unwrap()
    );
    let read = |path: &Path, sub: &str| {
        let run_dir = dir.join(sub);
        std::fs::create_dir_all(&run_dir).unwrap();
        let request = json!({"outside": path, "abstract": "unused", "marker": "kagu-unused"});
        let events = probe_with(&run_dir, "host", &extra, &request, &[]);
        assert_eq!(
            fold(&events).unwrap().state,
            Some(State::Completed),
            "{events:#?}"
        );
        report(&events)["read_outside"].as_bool().unwrap()
    };
    assert!(
        !read(&shared.join("hidden/secret"), "a"),
        "a hidden file was readable"
    );
    // Positive control: the rest of the ro grant is readable.
    assert!(
        read(&shared.join("visible"), "b"),
        "the grant around it was not readable"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}
