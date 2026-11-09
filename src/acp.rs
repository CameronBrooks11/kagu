//! ACP v1 over a worker's stdio: newline-delimited JSON-RPC 2.0.
//!
//! Reader threads turn the worker's stdout and stderr into messages on one
//! channel, so the run actor handles everything from the worker in order.

use std::io::{self, BufRead, BufReader, Write};
use std::process::{Child, ChildStdin};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::thread;
use std::time::Instant;

use serde_json::{Value, json};

pub enum Msg {
    /// A JSON-RPC message from stdout.
    Rpc(Value),
    /// A stdout line that is not a JSON object, or a stderr line.
    Raw { stream: &'static str, text: String },
    /// The worker closed stdout.
    Closed,
}

pub struct Conn {
    stdin: Option<ChildStdin>,
    rx: Receiver<Msg>,
    next_id: u64,
}

impl Conn {
    /// Take the child's piped stdio.
    pub fn attach(child: &mut Child) -> Self {
        let (tx, rx) = channel();
        let stdout = child.stdout.take().expect("stdout is piped");
        let stderr = child.stderr.take().expect("stderr is piped");
        let err_tx = tx.clone();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if line.trim().is_empty() {
                    continue;
                }
                let msg = match serde_json::from_str::<Value>(&line) {
                    Ok(v) if v.is_object() => Msg::Rpc(v),
                    _ => Msg::Raw {
                        stream: "stdout",
                        text: line,
                    },
                };
                if tx.send(msg).is_err() {
                    return;
                }
            }
            let _ = tx.send(Msg::Closed);
        });
        thread::spawn(move || {
            for text in BufReader::new(stderr).lines().map_while(Result::ok) {
                if err_tx
                    .send(Msg::Raw {
                        stream: "stderr",
                        text,
                    })
                    .is_err()
                {
                    return;
                }
            }
        });
        Self {
            stdin: child.stdin.take(),
            rx,
            next_id: 1,
        }
    }

    fn send(&mut self, msg: &Value) -> io::Result<()> {
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| io::Error::from(io::ErrorKind::BrokenPipe))?;
        let mut line = msg.to_string();
        line.push('\n');
        stdin.write_all(line.as_bytes())?;
        stdin.flush()
    }

    /// Send a request and return its id.
    pub fn request(&mut self, method: &str, params: Value) -> io::Result<u64> {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))?;
        Ok(id)
    }

    pub fn respond(&mut self, id: Value, result: Value) -> io::Result<()> {
        self.send(&json!({"jsonrpc": "2.0", "id": id, "result": result}))
    }

    pub fn respond_error(&mut self, id: Value, code: i64, message: &str) -> io::Result<()> {
        self.send(&json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}))
    }

    pub fn recv(&self) -> Msg {
        self.rx.recv().unwrap_or(Msg::Closed)
    }

    /// The next message, or `None` once `deadline` passes.
    pub fn recv_until(&self, deadline: Instant) -> Option<Msg> {
        let wait = deadline.saturating_duration_since(Instant::now());
        match self.rx.recv_timeout(wait) {
            Ok(msg) => Some(msg),
            Err(RecvTimeoutError::Disconnected) => Some(Msg::Closed),
            Err(RecvTimeoutError::Timeout) => None,
        }
    }

    /// Close the worker's stdin: the end of the conversation.
    pub fn close(&mut self) {
        self.stdin = None;
    }
}
