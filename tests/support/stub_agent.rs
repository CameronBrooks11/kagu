//! A scripted ACP v1 agent for tests. Its first argument picks the behaviour.

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

fn error(id: &Value, code: i64) {
    send(json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": "stub"}}));
}

fn main() {
    let mode = std::env::args().nth(1).unwrap_or_default();
    if mode == "exit_at_start" {
        return;
    }
    for line in std::io::stdin().lock().lines() {
        let msg: Value = serde_json::from_str(&line.unwrap()).unwrap();
        let id = msg["id"].clone();
        match msg["method"].as_str().unwrap_or("") {
            "initialize" => match mode.as_str() {
                "init_auth" => error(&id, -32000),
                "init_error" => error(&id, -32603),
                "version_2" => reply(&id, json!({"protocolVersion": 2})),
                _ => reply(
                    &id,
                    json!({"protocolVersion": 1, "agentInfo": {"name": "stub", "version": "0"}}),
                ),
            },
            "session/new" => match mode.as_str() {
                "new_auth" => error(&id, -32000),
                _ => reply(&id, json!({"sessionId": "s1"})),
            },
            "session/prompt" => {
                eprintln!("stub prompt");
                let text = match mode.as_str() {
                    "echo_secret" => std::env::var("STUB_SECRET").unwrap_or_default(),
                    _ => "hello".into(),
                };
                send(
                    json!({"jsonrpc": "2.0", "method": "session/update", "params": {
                    "sessionId": "s1",
                    "update": {"sessionUpdate": "agent_message_chunk",
                               "content": {"type": "text", "text": text}}}}),
                );
                match mode.as_str() {
                    "exit_in_prompt" => return,
                    "prompt_error" => error(&id, -32603),
                    "prompt_auth" => error(&id, -32000),
                    stop => {
                        let reason = match stop {
                            "" | "echo_secret" => "end_turn",
                            other => other,
                        };
                        reply(&id, json!({"stopReason": reason}));
                    }
                }
            }
            _ => error(&id, -32601),
        }
    }
}
