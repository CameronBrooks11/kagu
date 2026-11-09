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
    let mut prompt_id = Value::Null;
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
            ("session/new", "new_auth") => error(&id, -32000, "login"),
            ("session/new", "new_error") => error(&id, -32603, "broken"),
            ("session/new", "no_session") => reply(&id, json!({})),
            ("session/new", "exit_in_new") => return,
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
                            "" | "bad_utf8" => "end_turn",
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
