//! Confinement (spec: Grant): bubblewrap builds the namespaces, then
//! `kagu sandbox-init` applies Landlock to itself and execs the worker.
//! Landlock comes last because a Landlock-restricted process cannot mount.

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

use landlock::{
    ABI, Access, AccessFs, CompatLevel, Compatible, LandlockStatus, Ruleset, RulesetAttr,
    RulesetCreatedAttr, RulesetStatus, Scope, path_beneath_rules,
};

use crate::event::Level;
use crate::profile::{Grant, Network, Substrate};

/// The oldest bubblewrap with every flag used here (`--disable-userns`).
const MIN_BWRAP: (u32, u32) = (0, 8);
/// Landlock ABI from which abstract unix sockets and signals can be scoped.
const SCOPE_ABI: u32 = 6;

/// The line `sandbox-init` writes to stderr once the sandbox is built and
/// Landlock enforced, just before it execs the worker.
pub const READY: &str = "kagu-sandbox-ready";

/// What the host found when it tried the substrate.
#[derive(Debug, Clone)]
pub struct Probe {
    pub bwrap: PathBuf,
    pub landlock_abi: u32,
}

/// The substrate probe, run on first use. Only success is kept: a failure
/// is tried again by the next run that needs the substrate.
pub fn probe() -> Result<Probe, String> {
    static PROBE: Mutex<Option<Probe>> = Mutex::new(None);
    let mut kept = PROBE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(p) = kept.as_ref() {
        return Ok(p.clone());
    }
    let p = run_probe()?;
    *kept = Some(p.clone());
    Ok(p)
}

fn which(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
        .map(|dir| dir.join(name))
        .find(|p| p.is_file())
}

fn run_probe() -> Result<Probe, String> {
    let bwrap = which("bwrap").ok_or("bwrap is not on PATH")?;
    let meta = std::fs::metadata(&bwrap).map_err(|e| format!("{}: {e}", bwrap.display()))?;
    if meta.permissions().mode() & 0o4000 != 0 {
        return Err(format!(
            "{} is setuid; kagu needs unprivileged user namespaces",
            bwrap.display()
        ));
    }
    let out = Command::new(&bwrap)
        .arg("--version")
        .output()
        .map_err(|e| format!("{}: {e}", bwrap.display()))?;
    let version = String::from_utf8_lossy(&out.stdout);
    let mut parts = version
        .split_whitespace()
        .last()
        .unwrap_or("")
        .split('.')
        .map(|n| n.parse::<u32>().unwrap_or(0));
    let (major, minor) = (parts.next().unwrap_or(0), parts.next().unwrap_or(0));
    if (major, minor) < MIN_BWRAP {
        return Err(format!("bwrap {} is older than 0.8", version.trim()));
    }
    // One call proves both: a user namespace can be created, and Landlock
    // can be applied inside it.
    let exe = std::env::current_exe().map_err(|e| format!("kagu executable: {e}"))?;
    let out = Command::new(&bwrap)
        .args(["--unshare-all", "--die-with-parent", "--ro-bind", "/", "/"])
        .args(["--dev", "/dev", "--proc", "/proc", "--"])
        .arg(&exe)
        .args(["sandbox-init", "--probe"])
        .output()
        .map_err(|e| format!("{}: {e}", bwrap.display()))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!(
            "bwrap cannot create a sandbox here: {}",
            err.trim()
        ));
    }
    let landlock_abi = String::from_utf8_lossy(&out.stdout)
        .trim()
        .strip_prefix("landlock_abi ")
        .and_then(|n| n.parse().ok())
        .ok_or("sandbox-init --probe gave no Landlock ABI")?;
    if landlock_abi == 0 {
        return Err("Landlock is not available in this kernel".into());
    }
    Ok(Probe {
        bwrap,
        landlock_abi,
    })
}

/// The level of each grant field for this grant on this host, or why the
/// run must be rejected (Invariant 7: fail closed).
pub fn levels(
    grant: &Grant,
    probe: Option<&Result<Probe, String>>,
) -> Result<BTreeMap<String, Level>, String> {
    let level = |(k, v): (&str, Level)| (k.to_string(), v);
    match grant.substrate {
        Substrate::None => {
            if grant.network == Network::Off {
                return Err(
                    "network \"off\" needs a sandbox; substrate \"none\" cannot enforce it".into(),
                );
            }
            if !grant.hide.is_empty() {
                return Err(
                    "grant.hide needs a sandbox; substrate \"none\" cannot hide a path".into(),
                );
            }
            Ok([
                ("credentials", Level::Unsupported),
                ("filesystem", Level::Unsupported),
                ("network", Level::Unrestricted),
                ("process_tree", Level::Unsupported),
            ]
            .into_iter()
            .map(level)
            .collect())
        }
        Substrate::Bwrap => {
            let probe = probe
                .ok_or("substrate bwrap was not probed")?
                .as_ref()
                .map_err(|e| format!("substrate bwrap is unavailable: {e}"))?;
            let network = match grant.network {
                Network::Off => Level::Enforced,
                Network::Host => Level::Unrestricted,
            };
            // With the host's network, abstract unix sockets are shared;
            // only Landlock scoping closes them.
            let credentials = if grant.network == Network::Off || probe.landlock_abi >= SCOPE_ABI {
                Level::Enforced
            } else {
                Level::Unsupported
            };
            Ok([
                ("credentials", credentials),
                ("filesystem", Level::Enforced),
                ("network", network),
                ("process_tree", Level::Enforced),
            ]
            .into_iter()
            .map(level)
            .collect())
        }
    }
}

/// System paths visible read-only inside the sandbox.
const SYSTEM_RO: &[&str] = &["/usr", "/etc", "/opt"];
/// Top-level paths that are directories on some systems and symlinks into
/// /usr on others.
const SYSTEM_LINKS: &[&str] = &["/bin", "/sbin", "/lib", "/lib32", "/lib64"];

/// Where `HOME` and the XDG directories point inside the sandbox: a tmpfs
/// fresh for each run.
pub const HOME: &str = "/tmp/home";

/// The command that runs `argv` confined by `grant`: bwrap, then
/// `kagu sandbox-init`, then the worker.
pub fn command(
    probe: &Probe,
    grant: &Grant,
    workspace: &str,
    argv: &[String],
    env: &[(String, String)],
    pins: &[(String, String)],
) -> Result<Command, String> {
    let exe = std::env::current_exe().map_err(|e| format!("kagu executable: {e}"))?;
    let program = resolve(&argv[0])?;
    let mut c = Command::new(&probe.bwrap);
    c.args(["--die-with-parent", "--new-session", "--unshare-all"]);
    if grant.network == Network::Host {
        c.arg("--share-net");
    }
    // `--unshare-all` only tries the user namespace; `--disable-userns`
    // needs it made explicit.
    c.args(["--unshare-user", "--disable-userns"]);
    c.args(["--proc", "/proc", "--dev", "/dev", "--tmpfs", "/tmp"]);
    for dir in SYSTEM_RO {
        if Path::new(dir).exists() {
            c.args(["--ro-bind", dir, dir]);
        }
    }
    // DNS: where /etc/resolv.conf links out of /etc (systemd-resolved puts
    // it under /run), mount its target too.
    if let Ok(real) = std::fs::canonicalize("/etc/resolv.conf")
        && !real.starts_with("/etc")
    {
        c.arg("--ro-bind").arg(&real).arg(&real);
    }
    for link in SYSTEM_LINKS {
        match std::fs::read_link(link) {
            Ok(target) => {
                c.arg("--symlink").arg(target).arg(link);
            }
            Err(_) if Path::new(link).is_dir() => {
                c.args(["--ro-bind", link, link]);
            }
            Err(_) => {}
        }
    }
    // Read-write first, read-only after: bwrap mounts in order, so a
    // read-only path inside a writable one is mounted on top and stays
    // read-only (kagu's own executable inside its workspace, for one).
    for p in grant.rw.iter().map(String::as_str).chain([workspace]) {
        c.args(["--bind", p, p]);
    }
    c.arg("--ro-bind").arg(&exe).arg(&exe);
    c.arg("--ro-bind").arg(&program).arg(&program);
    for p in &grant.ro {
        c.args(["--ro-bind", p, p]);
    }
    for (source, dest) in pins {
        c.args(["--ro-bind", source, dest]);
    }
    // Last, so no other mount re-exposes what is hidden.
    for p in &grant.hide {
        let path = Path::new(p);
        if path.is_dir() {
            c.args(["--tmpfs", p]);
        } else if path.exists() {
            c.args(["--ro-bind", "/dev/null", p]);
        }
    }
    c.args(["--dir", HOME, "--chdir", workspace]);
    // The environment is bwrap's own, which it passes on: cleared, then only
    // what the worker should see. Never arguments, which every user can read
    // in /proc; an environment is readable only by the same user.
    let home = Path::new(HOME);
    c.env_clear()
        .env("PATH", "/usr/local/bin:/usr/bin:/bin")
        .env("HOME", HOME)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_CACHE_HOME", home.join(".cache"))
        .env("XDG_DATA_HOME", home.join(".local/share"))
        .env("XDG_STATE_HOME", home.join(".local/state"))
        .env("XDG_RUNTIME_DIR", "/tmp")
        .env("TMPDIR", "/tmp")
        .envs(env.iter().map(|(k, v)| (k, v)));
    c.arg("--").arg(&exe).arg("sandbox-init");
    c.arg("--abi").arg(probe.landlock_abi.to_string());
    for p in grant.rw.iter().map(String::as_str).chain([workspace]) {
        c.args(["--rw", p]);
    }
    c.arg("--").arg(&program).args(&argv[1..]);
    Ok(c)
}

/// The command for `argv` under `grant`: confined when the grant's substrate
/// was probed, otherwise run directly with a cleared environment.
pub fn spawnable(
    probe: Option<&Result<Probe, String>>,
    grant: &Grant,
    workspace: &str,
    argv: &[String],
    env: &[(String, String)],
    pins: &[(String, String)],
) -> Result<Command, String> {
    match probe {
        Some(Ok(probe)) => command(probe, grant, workspace, argv, env, pins),
        Some(Err(e)) => Err(format!("substrate bwrap is unavailable: {e}")),
        None => {
            let mut c = Command::new(&argv[0]);
            c.args(&argv[1..])
                .current_dir(workspace)
                .env_clear()
                .env("PATH", std::env::var_os("PATH").unwrap_or_default())
                .envs(env.iter().map(|(k, v)| (k, v)));
            Ok(c)
        }
    }
}

/// The worker's executable as an absolute path, looked up on the host's
/// PATH when it has no `/`.
fn resolve(program: &str) -> Result<PathBuf, String> {
    if program.contains('/') {
        return std::fs::canonicalize(program).map_err(|e| format!("{program}: {e}"));
    }
    which(program).ok_or_else(|| format!("{program} is not on PATH"))
}

fn abi_number(abi: ABI) -> u32 {
    // `ABI` is non-exhaustive, so it cannot be cast; its Debug form is `V<n>`.
    format!("{abi:?}")
        .trim_start_matches('V')
        .parse()
        .unwrap_or(0)
}

/// `kagu sandbox-init`: runs inside bwrap. Applies Landlock to itself, then
/// execs the worker. With `--probe`, reports the Landlock ABI and exits.
pub fn sandbox_init(args: &[String]) -> Result<(), String> {
    if args.first().map(String::as_str) == Some("--probe") {
        let abi = ABI::V6;
        let status = Ruleset::default()
            .handle_access(AccessFs::from_all(abi))
            .and_then(|r| r.create())
            .and_then(|r| r.add_rules(path_beneath_rules(["/"], AccessFs::from_all(abi))))
            .and_then(|r| r.restrict_self())
            .map_err(|e| format!("landlock: {e}"))?;
        let found = match status.landlock {
            LandlockStatus::Available { effective_abi, .. } => abi_number(effective_abi),
            _ => 0,
        };
        println!("landlock_abi {found}");
        return Ok(());
    }
    let mut abi = None;
    let mut rw: Vec<String> = Vec::new();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--abi" => abi = it.next().and_then(|n| n.parse::<i32>().ok()),
            "--rw" => rw.extend(it.next().cloned()),
            "--" => break,
            other => return Err(format!("sandbox-init: unexpected {other:?}")),
        }
    }
    let argv: Vec<&String> = it.collect();
    let (abi, program) = match (abi, argv.first()) {
        (Some(abi), Some(program)) => (ABI::from(abi), program),
        _ => return Err("sandbox-init: usage: --abi <n> [--rw <path>]... -- <argv>".into()),
    };
    // Every right the probed ABI knows, required in full: a partial ruleset
    // would make the recorded levels false.
    let mut ruleset = Ruleset::default()
        .set_compatibility(CompatLevel::HardRequirement)
        .handle_access(AccessFs::from_all(abi))
        .map_err(|e| format!("landlock: {e}"))?;
    if abi_number(abi) >= SCOPE_ABI {
        ruleset = ruleset
            .scope(Scope::from_all(abi))
            .map_err(|e| format!("landlock: {e}"))?;
    }
    let writable = rw.iter().map(String::as_str).chain(["/tmp", "/dev"]);
    let status = ruleset
        .create()
        .and_then(|r| r.add_rules(path_beneath_rules(["/"], AccessFs::from_read(abi))))
        .and_then(|r| r.add_rules(path_beneath_rules(writable, AccessFs::from_all(abi))))
        .and_then(|r| r.restrict_self())
        .map_err(|e| format!("landlock: {e}"))?;
    if status.ruleset != RulesetStatus::FullyEnforced {
        return Err(format!(
            "landlock: ruleset {:?}, not fully enforced",
            status.ruleset
        ));
    }
    eprintln!("{READY}");
    let err = Command::new(program).args(&argv[1..]).exec();
    Err(format!("exec {program}: {err}"))
}

#[cfg(test)]
mod tests {
    use super::{Probe, command, levels};
    use crate::event::Level;
    use crate::profile::{Grant, Network, Substrate};

    fn grant(substrate: Substrate, network: Network) -> Grant {
        Grant {
            substrate,
            network,
            ..Grant::default()
        }
    }

    fn probe(abi: u32) -> Result<Probe, String> {
        Ok(Probe {
            bwrap: "/usr/bin/bwrap".into(),
            landlock_abi: abi,
        })
    }

    #[test]
    fn without_a_sandbox_nothing_is_enforced() {
        let l = levels(&grant(Substrate::None, Network::Host), None).unwrap();
        assert!(
            l.values()
                .all(|v| matches!(v, Level::Unsupported | Level::Unrestricted))
        );
        assert_eq!(l["network"], Level::Unrestricted);
    }

    #[test]
    fn hidden_paths_are_covered_after_every_other_mount() {
        let dir = std::env::temp_dir().join(format!("kagu-hide-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("conf")).unwrap();
        std::fs::write(dir.join("file"), "x").unwrap();
        let p = |name: &str| dir.join(name).to_str().unwrap().to_string();
        let g = Grant {
            ro: vec![p("")],
            hide: vec![p("conf"), p("file"), p("missing")],
            ..grant(Substrate::Bwrap, Network::Host)
        };
        let pins = [(p("file"), p("conf/pinned"))];
        let c = command(
            &probe(6).unwrap(),
            &g,
            "/tmp",
            &["/bin/sh".into()],
            &[],
            &pins,
        )
        .unwrap();
        let args: Vec<String> = c
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let at = |seq: &[&str]| args.windows(seq.len()).position(|w| w == seq);
        let ro = at(&["--ro-bind", &p(""), &p("")]).unwrap();
        let conf = at(&["--tmpfs", &p("conf")]).expect("directory covered by a tmpfs");
        let file = at(&["--ro-bind", "/dev/null", &p("file")]).expect("file covered by /dev/null");
        let pin = at(&["--ro-bind", &p("file"), &p("conf/pinned")]).unwrap();
        assert!(
            ro < conf && ro < file,
            "hidden after the mounts they hide in"
        );
        assert!(pin < conf && pin < file, "hidden after the pinned files");
        assert!(!args.contains(&p("missing")), "a missing path is skipped");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_restriction_nothing_can_enforce_is_rejected() {
        assert!(levels(&grant(Substrate::None, Network::Off), None).is_err());
        let hide = Grant {
            hide: vec!["/etc/claude-code".into()],
            ..grant(Substrate::None, Network::Host)
        };
        assert!(levels(&hide, None).unwrap_err().contains("grant.hide"));
        let unavailable = Err("no user namespaces".to_string());
        let err = levels(&grant(Substrate::Bwrap, Network::Host), Some(&unavailable)).unwrap_err();
        assert!(err.contains("no user namespaces"), "{err}");
    }

    #[test]
    fn credentials_depend_on_scoping_or_no_network() {
        let host = grant(Substrate::Bwrap, Network::Host);
        let off = grant(Substrate::Bwrap, Network::Off);
        assert_eq!(
            levels(&host, Some(&probe(6))).unwrap()["credentials"],
            Level::Enforced
        );
        assert_eq!(
            levels(&host, Some(&probe(5))).unwrap()["credentials"],
            Level::Unsupported
        );
        let l = levels(&off, Some(&probe(5))).unwrap();
        assert_eq!(l["credentials"], Level::Enforced);
        assert_eq!(l["network"], Level::Enforced);
        assert_eq!(l["filesystem"], Level::Enforced);
        assert_eq!(l["process_tree"], Level::Enforced);
    }
}
