//! `kagu auth <profile>`: sign the worker in through its own advertised
//! login, in the profile's harness home (spec: Auth). kagu never sees the
//! credentials: the worker writes them to its home.

use std::process::Stdio;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::acp::{Conn, Msg};
use crate::profile::{self, Network, Substrate};
use crate::substrate;

/// How long the worker may take to answer `initialize`.
const ANSWER: Duration = Duration::from_secs(30);

/// Run a `terminal` login method the worker advertises, attached to this
/// terminal. Returns the login's exit code.
pub fn auth(profile_arg: &str, method: Option<&str>) -> Result<i32, String> {
    let path = profile::resolve(profile_arg)?;
    let loaded = profile::load(&path)?;
    let worker = &loaded.profile.worker;
    if worker.home_env.is_empty() {
        return Err("the profile has no harness home (worker.home_env)".into());
    }
    let env_var = |k: &str| std::env::var(k).ok();
    let home = profile::prepare_home(&env_var, &path, worker, &loaded.profile.grant)?.path;
    let mut env: Vec<(String, String)> = worker
        .home_env
        .iter()
        .map(|v| (v.clone(), home.clone()))
        .collect();
    for (name, reference) in &worker.env {
        let var = profile::reference_var(reference).expect("validated at load");
        let value =
            env_var(var).ok_or_else(|| format!("credential reference {reference} is unset"))?;
        env.push((name.clone(), value));
    }
    // Login reaches the provider, and may redirect a browser to loopback.
    let mut grant = loaded.profile.grant.clone();
    grant.network = Network::Host;
    let probe = (grant.substrate == Substrate::Bwrap).then(substrate::probe);

    let methods = terminal_methods(probe.as_ref(), &grant, &home, &worker.command, &env)?;
    let chosen = match method {
        Some(id) => methods.iter().find(|m| m["id"] == id),
        None => methods.first(),
    };
    let Some(chosen) = chosen else {
        let ids: Vec<&str> = methods.iter().filter_map(|m| m["id"].as_str()).collect();
        return Err(format!(
            "no matching terminal login method; the worker offers {ids:?}"
        ));
    };
    eprintln!(
        "kagu: {} ({})",
        chosen["name"].as_str().unwrap_or(""),
        chosen["id"].as_str().unwrap_or("")
    );
    let mut argv = worker.command.clone();
    argv.extend(
        chosen["args"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|a| a.as_str().map(str::to_string)),
    );
    if let Some(extra) = chosen["env"].as_object() {
        env.extend(
            extra
                .iter()
                .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string()))),
        );
    }
    let status = substrate::spawnable(probe.as_ref(), &grant, &home, &argv, &env, &[])?
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .map_err(|e| format!("{}: {e}", argv[0]))?;
    Ok(status.code().unwrap_or(1))
}

/// The worker's `terminal` auth methods, from an `initialize` that declares
/// the client can run them.
fn terminal_methods(
    probe: Option<&Result<substrate::Probe, String>>,
    grant: &profile::Grant,
    home: &str,
    argv: &[String],
    env: &[(String, String)],
) -> Result<Vec<Value>, String> {
    let child = substrate::spawnable(probe, grant, home, argv, env, &[])?
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("{}: {e}", argv[0]))?;
    let mut conn = Conn::attach(child);
    let init = json!({"protocolVersion": 1, "clientCapabilities": {"auth": {"terminal": true}}});
    let id = conn
        .request("initialize", init)
        .map_err(|e| format!("worker: {e}"))?;
    let deadline = Instant::now() + ANSWER;
    let mut said = Vec::new();
    let result = loop {
        match conn.recv_until(deadline) {
            Some(Msg::Rpc(msg)) if msg.get("method").is_none() && msg["id"] == json!(id) => {
                break msg;
            }
            Some(Msg::Raw { text, .. }) if text != substrate::READY => said.push(text),
            Some(Msg::Closed) | None => {
                conn.kill();
                return Err(format!(
                    "the worker did not answer initialize: {}",
                    said.join("; ")
                ));
            }
            Some(_) => {}
        }
    };
    conn.kill();
    if let Some(err) = result.get("error") {
        return Err(format!("initialize: {err}"));
    }
    Ok(result["result"]["authMethods"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|m| m["type"] == "terminal")
        .cloned()
        .collect())
}
