//! ACP v1 over a worker's stdio: newline-delimited JSON-RPC 2.0.
//!
//! Reader threads turn the worker's stdout and stderr into messages on one
//! channel, and a waiter thread reports the worker's exit on it, so the run
//! actor handles everything from the worker in order.

use std::io::{self, BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ExitStatus};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// Longest line kept from a worker; the rest of a longer line is dropped.
const MAX_LINE: usize = 16 << 20;

pub enum Msg {
    /// A JSON-RPC message from stdout.
    Rpc(Value),
    /// A stdout line that is not a JSON object, or a stderr line.
    Raw { stream: &'static str, text: String },
    /// The worker closed stdout.
    Closed,
    /// The worker closed stderr.
    ErrClosed,
    /// The worker process exited.
    Exited(ExitStatus),
    /// The host asks the run to cancel (a wake-up; the run's flag decides).
    Cancel,
}

pub struct Conn {
    stdin: Option<ChildStdin>,
    rx: Receiver<Msg>,
    next_id: u64,
    child: Arc<Mutex<Child>>,
    pid: u32,
    waker: SyncSender<Msg>,
}

/// Read one line, keeping at most `MAX_LINE` bytes. `Some(truncated)`, or
/// `None` at end of input.
fn read_line(r: &mut impl BufRead, buf: &mut Vec<u8>) -> io::Result<Option<bool>> {
    buf.clear();
    let (mut truncated, mut any) = (false, false);
    loop {
        let avail = r.fill_buf()?;
        if avail.is_empty() {
            return Ok(any.then_some(truncated));
        }
        any = true;
        let (take, done) = match avail.iter().position(|&b| b == b'\n') {
            Some(i) => (i + 1, true),
            None => (avail.len(), false),
        };
        let room = MAX_LINE.saturating_sub(buf.len());
        truncated |= take > room;
        buf.extend_from_slice(&avail[..take.min(room)]);
        r.consume(take);
        if done {
            if buf.last() == Some(&b'\n') {
                buf.pop();
            }
            return Ok(Some(truncated));
        }
    }
}

/// Forward each line as a message: lossy UTF-8, so no byte ends the stream.
fn forward(stream: impl io::Read, name: &'static str, tx: &SyncSender<Msg>, json: bool) {
    let mut reader = BufReader::new(stream);
    let mut buf = Vec::new();
    while let Ok(Some(truncated)) = read_line(&mut reader, &mut buf) {
        let mut text = String::from_utf8_lossy(&buf).into_owned();
        if text.trim().is_empty() {
            continue;
        }
        let parsed = (json && !truncated)
            .then(|| serde_json::from_str::<Value>(&text).ok())
            .flatten()
            .filter(Value::is_object);
        let msg = match parsed {
            Some(v) => Msg::Rpc(v),
            None => {
                if truncated {
                    text.push_str(" [truncated]");
                }
                Msg::Raw { stream: name, text }
            }
        };
        if tx.send(msg).is_err() {
            return;
        }
    }
}

impl Conn {
    /// Take the child's piped stdio and watch for its exit.
    pub fn attach(mut child: Child) -> Self {
        // Bounded: a worker that outpaces the log blocks on its own pipe.
        let (tx, rx) = sync_channel(64);
        let stdout = child.stdout.take().expect("stdout is piped");
        let stderr = child.stderr.take().expect("stderr is piped");
        let stdin = child.stdin.take();
        let pid = child.id();
        let child = Arc::new(Mutex::new(child));

        let waker = tx.clone();
        let out_tx = tx.clone();
        thread::spawn(move || {
            forward(stdout, "stdout", &out_tx, true);
            let _ = out_tx.send(Msg::Closed);
        });
        let err_tx = tx.clone();
        thread::spawn(move || {
            forward(stderr, "stderr", &err_tx, false);
            let _ = err_tx.send(Msg::ErrClosed);
        });
        let waited = Arc::clone(&child);
        thread::spawn(move || {
            loop {
                let status = waited.lock().expect("child lock").try_wait();
                match status {
                    Ok(Some(s)) => {
                        let _ = tx.send(Msg::Exited(s));
                        return;
                    }
                    Ok(None) => thread::sleep(Duration::from_millis(20)),
                    Err(_) => return,
                }
            }
        });
        Self {
            stdin,
            rx,
            next_id: 1,
            child,
            pid,
            waker,
        }
    }

    /// A sender that can wake the run's actor from another thread.
    pub fn waker(&self) -> SyncSender<Msg> {
        self.waker.clone()
    }

    pub fn pid(&self) -> u32 {
        self.pid
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

    pub fn notify(&mut self, method: &str, params: Value) -> io::Result<()> {
        self.send(&json!({"jsonrpc": "2.0", "method": method, "params": params}))
    }

    pub fn respond_error(&mut self, id: Value, code: i64, message: &str) -> io::Result<()> {
        self.send(&json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}))
    }

    pub fn recv(&self) -> Msg {
        self.rx.recv().unwrap_or(Msg::Closed)
    }

    /// The next message, or `None` once `deadline` passes or every sender is gone.
    pub fn recv_until(&self, deadline: Instant) -> Option<Msg> {
        let wait = deadline.saturating_duration_since(Instant::now());
        self.rx.recv_timeout(wait).ok()
    }

    /// Close the worker's stdin: the end of the conversation.
    pub fn close(&mut self) {
        self.stdin = None;
    }

    pub fn kill(&self) {
        let _ = self.child.lock().expect("child lock").kill();
    }
}
