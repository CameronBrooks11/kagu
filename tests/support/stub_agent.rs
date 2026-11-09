//! A scripted ACP v1 agent for tests. Its first argument picks the behaviour;
//! `STUB_SECRET` is echoed by the `leak` mode.

use std::io::{BufRead, Write};

use serde_json::{Value, json};

fn send(v: Value) {
    let mut out = std::io::stdout().lock();
    writeln!(out, "{v}").unwrap();
    out.flush().unwrap();
}

fn reply(id: &Value, result: Value) {
    send(json!({"jsonrpc": "2.0", "id": id, "result": result}));
}

fn error(id: &Value, code: i64, message: &str) {
    send(json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}));
}

fn update(text: &str) {
    send(
        json!({"jsonrpc": "2.0", "method": "session/update", "params": {
        "sessionId": "s1",
        "update": {"sessionUpdate": "agent_message_chunk",
                   "content": {"type": "text", "text": text}}}}),
    );
}

fn main() {
    let mode = std::env::args().nth(1).unwrap_or_default();
    let secret = std::env::var("STUB_SECRET").unwrap_or_default();
    if mode == "exit_at_start" {
        return;
    }
    let home = std::env::var("STUB_HOME").unwrap_or_default();
    if mode == "auth" && std::env::args().nth(2).as_deref() == Some("login") {
        // The login method: the worker writes its own credential.
        let token = std::env::var("STUB_TOKEN").unwrap_or_default();
        std::fs::write(format!("{home}/credentials"), token).unwrap();
        // A login that rewrites settings: confined, the pin stops it.
        let _ = std::fs::write(format!("{home}/settings.json"), "planted");
        return;
    }
    if mode == "linger" {
        // A descendant that would outlive its run if nothing contained it.
        std::thread::sleep(std::time::Duration::from_secs(60));
        return;
    }
    let mut prompt_id = Value::Null;
    let mut new_params = Value::Null;
    for line in std::io::stdin().lock().lines() {
        let msg: Value = serde_json::from_str(&line.unwrap()).unwrap();
        let id = msg["id"].clone();
        let Some(method) = msg["method"].as_str() else {
            // A response to our own request (`ask`, `ask_unknown`): report it, finish.
            update(&msg.to_string());
            reply(&prompt_id, json!({"stopReason": "end_turn"}));
            continue;
        };
        match (method, mode.as_str()) {
            ("initialize", "init_auth") => error(&id, -32000, "login"),
            ("initialize", "init_error") => error(&id, -32603, "broken"),
            ("initialize", "version_2") => reply(&id, json!({"protocolVersion": 2})),
            ("initialize", "orphan_exit") => {
                // A descendant keeps stdout open after the agent exits.
                // Deliberately not waited for: it must outlive this process.
                let _orphan = std::process::Command::new("sleep")
                    .arg("10")
                    .spawn()
                    .unwrap();
                std::mem::forget(_orphan);
                return;
            }
            ("initialize", "auth") => {
                let terminal = msg["params"]["clientCapabilities"]["auth"]["terminal"] == true;
                let methods = if terminal {
                    json!([{"id": "stub-login", "name": "Stub login", "type": "terminal",
                            "args": ["login"], "env": {"STUB_TOKEN": "t0k"}}])
                } else {
                    json!([])
                };
                reply(&id, json!({"protocolVersion": 1, "authMethods": methods}));
            }
            ("initialize", _) => {
                if mode == "bad_utf8" {
                    std::io::stderr().write_all(b"\xff\xfe bytes\n").unwrap();
                }
                let name = if mode == "leak" {
                    secret.as_str()
                } else {
                    "stub"
                };
                reply(
                    &id,
                    json!({"protocolVersion": 1, "agentInfo": {"name": name, "version": "0"}}),
                );
            }
            // `hang` stops when asked; `hang_hard` never does.
            ("session/cancel", "hang") => reply(&prompt_id, json!({"stopReason": "cancelled"})),
            ("session/cancel", _) => {}
            ("session/new", "new_auth") => error(&id, -32000, "login"),
            ("session/new", "new_error") => error(&id, -32603, "broken"),
            ("session/new", "no_session") => reply(&id, json!({})),
            ("session/new", "exit_in_new") => return,
            ("session/new", "auth")
                if !std::path::Path::new(&format!("{home}/credentials")).exists() =>
            {
                error(&id, -32000, "login required")
            }
            ("session/new", "echo_new") => {
                new_params = msg["params"].clone();
                reply(&id, json!({"sessionId": "s1"}));
            }
            ("session/new", _) => {
                let sid = if mode == "leak" {
                    secret.as_str()
                } else {
                    "s1"
                };
                reply(&id, json!({"sessionId": sid}));
            }
            ("session/prompt", _) => {
                eprintln!("stub prompt");
                prompt_id = id.clone();
                match mode.as_str() {
                    "leak" => {
                        update(&secret);
                        // Not a session update: kept raw, as JSON text.
                        send(
                            json!({"jsonrpc": "2.0", "method": "_stub/note", "params": {"text": secret}}),
                        );
                        eprintln!("stderr {secret}");
                        println!("plain stdout {secret}");
                        error(&id, -32603, &secret);
                    }
                    "leak_stop" => {
                        // Routes a JSON-escaped value can hide in.
                        eprintln!("{}", json!({"log": secret}));
                        println!("{}", json!([secret]));
                        send(
                            json!({"jsonrpc": "2.0", "method": "session/update", "params": {
                            "sessionId": "s1",
                            "update": {"sessionUpdate": "agent_message_chunk",
                                       "content": {"type": "text", "text": "k"},
                                       secret.as_str(): 1}}}),
                        );
                        reply(&id, json!({"stopReason": secret}));
                    }
                    "leak_number" => {
                        eprintln!("{secret}");
                        eprintln!("{{\"pin\": {secret}}}");
                        reply(&id, json!({"stopReason": "end_turn"}));
                    }
                    "hang" | "hang_hard" => update("working"),
                    // Try to reach past the grant; report what was reachable.
                    "probe" => {
                        let req: Value = serde_json::from_str(
                            msg["params"]["prompt"][0]["text"].as_str().unwrap_or("{}"),
                        )
                        .unwrap_or_default();
                        update(&probe(&req).to_string());
                        reply(&id, json!({"stopReason": "end_turn"}));
                    }
                    "auth" => {
                        // Read the pinned file, then try to plant over it.
                        let settings = format!("{home}/settings.json");
                        let found = std::fs::read_to_string(&settings).ok();
                        let planted = std::fs::write(&settings, "planted").is_ok();
                        let transcript = std::fs::write(format!("{home}/transcript"), "t").is_ok();
                        update(
                            &json!({"settings": found, "planted": planted, "transcript": transcript})
                                .to_string(),
                        );
                        reply(&id, json!({"stopReason": "end_turn"}));
                    }
                    "echo_new" => {
                        update(&new_params.to_string());
                        reply(&id, json!({"stopReason": "end_turn"}));
                    }
                    "noise" => {
                        println!("not json");
                        reply(&id, json!({"stopReason": "end_turn"}));
                    }
                    "ask" => send(
                        json!({"jsonrpc": "2.0", "id": 99, "method": "session/request_permission",
                        "params": {"sessionId": "s1", "toolCall": {"toolCallId": "t1"}, "options": [
                            {"optionId": "yes", "name": "Allow", "kind": "allow_once"},
                            {"optionId": "no", "name": "Reject", "kind": "reject_once"}]}}),
                    ),
                    "ask_unknown" => send(
                        json!({"jsonrpc": "2.0", "id": 98, "method": "fs/read_text_file",
                        "params": {"sessionId": "s1", "path": "/etc/passwd"}}),
                    ),
                    "exit_in_prompt" => {
                        update("hello");
                        return;
                    }
                    "prompt_error" => error(&id, -32603, "broken"),
                    "prompt_auth" => error(&id, -32000, "login"),
                    stop => {
                        update("hello");
                        let reason = match stop {
                            "" | "bad_utf8" | "auth" => "end_turn",
                            other => other,
                        };
                        reply(&id, json!({"stopReason": reason}));
                    }
                }
            }
            _ => error(&id, -32601, "unknown"),
        }
    }
}

/// Each attempt in `req`, and whether it succeeded.
fn probe(req: &Value) -> Value {
    use std::os::linux::net::SocketAddrExt;
    let path = |k: &str| req[k].as_str().unwrap_or("").to_string();
    let mut out = serde_json::Map::new();
    out.insert(
        "read_outside".into(),
        json!(std::fs::read(path("outside")).is_ok()),
    );
    out.insert(
        "write_outside".into(),
        json!(std::fs::write(path("outside_new"), "x").is_ok()),
    );
    out.insert(
        "write_workspace".into(),
        json!(std::fs::write("written-by-stub", "x").is_ok()),
    );
    out.insert(
        "env_leaked".into(),
        json!(std::env::var_os("KAGU_TEST_LEAK").is_some()),
    );
    // Writable to its own process without Landlock; no mount hides it.
    out.insert(
        "write_proc_comm".into(),
        json!(std::fs::write("/proc/self/comm", "stub").is_ok()),
    );
    let addr =
        std::os::unix::net::SocketAddr::from_abstract_name(path("abstract").as_bytes()).unwrap();
    out.insert(
        "abstract_socket".into(),
        json!(std::os::unix::net::UnixStream::connect_addr(&addr).is_ok()),
    );
    let tcp = "1.1.1.1:53".parse().unwrap();
    out.insert(
        "network".into(),
        json!(
            std::net::TcpStream::connect_timeout(&tcp, std::time::Duration::from_secs(3)).is_ok()
        ),
    );
    // A descendant that outlives this process unless the tree is contained.
    let marker = path("marker");
    let me = std::env::current_exe().unwrap();
    std::mem::forget(
        std::process::Command::new(me)
            .args(["linger", &marker])
            .spawn()
            .unwrap(),
    );
    Value::Object(out)
}
