//! A client of the host's socket.

use std::io::{BufRead, BufReader, Lines, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;

use serde_json::{Value, json};

use crate::event::Event;

pub struct Client {
    lines: Lines<BufReader<UnixStream>>,
    out: UnixStream,
    next_id: u64,
}

impl Client {
    pub fn connect(socket: &Path) -> Result<Self, String> {
        let stream = UnixStream::connect(socket).map_err(|e| {
            format!(
                "no kagu host at {} ({e}); start one with `kagu serve`",
                socket.display()
            )
        })?;
        let out = stream.try_clone().map_err(|e| e.to_string())?;
        Ok(Self {
            lines: BufReader::new(stream).lines(),
            out,
            next_id: 1,
        })
    }

    pub fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        let mut line =
            json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string();
        line.push('\n');
        self.out
            .write_all(line.as_bytes())
            .map_err(|e| format!("host: {e}"))?;
        loop {
            let line = self
                .lines
                .next()
                .ok_or("host closed the connection")?
                .map_err(|e| format!("host: {e}"))?;
            let msg: Value = serde_json::from_str(&line).map_err(|e| format!("host: {e}"))?;
            if msg.get("id") != Some(&json!(id)) {
                continue;
            }
            if let Some(err) = msg.get("error") {
                let message = err
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("error");
                return Err(message.to_string());
            }
            return Ok(msg.get("result").cloned().unwrap_or(Value::Null));
        }
    }

    /// Subscribe to the log. Returns the snapshot (`runs`, `seq`) and the
    /// events that follow it, or follow `after` when given.
    pub fn subscribe(
        mut self,
        after: Option<i64>,
    ) -> Result<(Value, impl Iterator<Item = Result<Event, String>>), String> {
        let params = match after {
            Some(seq) => json!({ "after": seq }),
            None => json!({}),
        };
        let snapshot = self.call("log.subscribe", params)?;
        let events = self.lines.map(|line| {
            let line = line.map_err(|e| format!("host: {e}"))?;
            let msg: Value = serde_json::from_str(&line).map_err(|e| format!("host: {e}"))?;
            serde_json::from_value(msg["params"].clone()).map_err(|e| format!("host: {e}"))
        });
        Ok((snapshot, events))
    }
}
