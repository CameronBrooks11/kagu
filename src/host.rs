//! The host: owns the log and the client socket, and drives each run on its
//! own thread (spec: Protocols). JSON-RPC 2.0, one message per line.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use serde_json::{Value, json};

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::api;
use crate::event::{State, TaskSubmitted};
use crate::log::Log;
use crate::run::{self, Cancel};

struct Host {
    log: Arc<Log>,
    /// Runs that have started and not finished.
    active: Arc<Mutex<BTreeMap<String, Arc<Cancel>>>>,
}

type RpcError = (i64, String);

fn invalid(e: impl ToString) -> RpcError {
    (-32602, e.to_string())
}

fn failed(e: impl ToString) -> RpcError {
    (-32000, e.to_string())
}

fn params<P: DeserializeOwned>(params: &Value) -> Result<P, RpcError> {
    // A method that takes none may be sent no params at all.
    let params = if params.is_null() {
        json!({})
    } else {
        params.clone()
    };
    serde_json::from_value(params).map_err(invalid)
}

fn result<R: Serialize>(r: R) -> Result<Value, RpcError> {
    Ok(serde_json::to_value(r).expect("result serializes"))
}

/// Removes a run from `active` when its thread ends, by return or by panic.
struct Active {
    runs: Arc<Mutex<BTreeMap<String, Arc<Cancel>>>>,
    id: String,
}

impl Drop for Active {
    fn drop(&mut self) {
        let mut runs = self.runs.lock().unwrap_or_else(|e| e.into_inner());
        runs.remove(&self.id);
    }
}

pub fn serve(db: &Path, socket: &Path) -> Result<(), String> {
    // Exclusive before anything touches the log, so a second host cannot
    // mark a live host's runs interrupted. Held while serving.
    let _lock = lock_db(db)?;
    let listener = bind(socket)?;
    let log = Arc::new(Log::open(db)?);
    recover(&log)?;
    eprintln!("kagu: serving on {}", socket.display());
    let host = Arc::new(Host {
        log,
        active: Arc::default(),
    });
    for stream in listener.incoming().flatten() {
        let host = Arc::clone(&host);
        thread::spawn(move || host.client(stream));
    }
    Ok(())
}

/// Mark every unfinished run `interrupted`: workers die with the host, and
/// the host did not observe how they ended.
pub fn recover(log: &Log) -> Result<(), String> {
    for id in log.snapshot().runs.keys() {
        run::interrupt(log, id)?;
    }
    Ok(())
}

/// One host per log: an exclusive lock on `<db>.lock`, released when the
/// returned file is dropped or the process ends.
pub fn lock_db(db: &Path) -> Result<std::fs::File, String> {
    let path = std::path::PathBuf::from(format!("{}.lock", db.display()));
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(std::fs::TryLockError::WouldBlock) => {
            Err(format!("another kagu host is using {}", db.display()))
        }
        Err(std::fs::TryLockError::Error(e)) => Err(format!("{}: {e}", path.display())),
    }
}

/// This process's user, as the owner of its own /proc entry (Linux).
fn own_uid() -> Result<u32, String> {
    std::fs::metadata("/proc/self")
        .map(|m| m.uid())
        .map_err(|e| format!("/proc/self: {e}"))
}

/// A private socket: its directory is 0700 and the socket 0600.
fn bind(socket: &Path) -> Result<UnixListener, String> {
    let dir = socket.parent().ok_or("socket path has no directory")?;
    match std::fs::metadata(dir) {
        // An existing directory is never changed: refuse one another user
        // owns or can write to, since they could replace the socket.
        Ok(meta) if meta.permissions().mode() & 0o022 != 0 || meta.uid() != own_uid()? => {
            return Err(format!(
                "{} is not private to this user; choose a directory only you own and can write",
                dir.display()
            ));
        }
        Ok(_) => {}
        Err(_) => std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .map_err(|e| format!("{}: {e}", dir.display()))?,
    }
    if socket.exists() {
        if UnixStream::connect(socket).is_ok() {
            return Err(format!(
                "a kagu host is already serving {}",
                socket.display()
            ));
        }
        std::fs::remove_file(socket).map_err(|e| format!("{}: {e}", socket.display()))?;
    }
    let listener = UnixListener::bind(socket).map_err(|e| format!("{}: {e}", socket.display()))?;
    std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| format!("{}: {e}", socket.display()))?;
    Ok(listener)
}

fn send(out: &mut UnixStream, msg: &Value) -> bool {
    let mut line = msg.to_string();
    line.push('\n');
    out.write_all(line.as_bytes())
        .and_then(|_| out.flush())
        .is_ok()
}

impl Host {
    fn client(&self, stream: UnixStream) {
        let Ok(mut out) = stream.try_clone() else {
            return;
        };
        for line in BufReader::new(stream).lines().map_while(Result::ok) {
            let req: Value = match serde_json::from_str(&line) {
                Ok(v) => v,
                Err(e) => {
                    let err = json!({"code": -32700, "message": e.to_string()});
                    send(
                        &mut out,
                        &json!({"jsonrpc": "2.0", "id": null, "error": err}),
                    );
                    continue;
                }
            };
            let id = req.get("id").cloned().unwrap_or(Value::Null);
            let method = req.get("method").and_then(Value::as_str).unwrap_or("");
            let params = req.get("params").cloned().unwrap_or(Value::Null);
            if method == "log.subscribe" {
                // The connection becomes a stream of events until it closes.
                self.subscribe(&mut out, id, &params);
                return;
            }
            let reply = match self.call(method, &params) {
                Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
                Err((code, message)) => {
                    let err = json!({"code": code, "message": message});
                    json!({"jsonrpc": "2.0", "id": id, "error": err})
                }
            };
            if !send(&mut out, &reply) {
                return;
            }
        }
    }

    fn call(&self, method: &str, p: &Value) -> Result<Value, RpcError> {
        match method {
            "task.submit" => {
                let task: TaskSubmitted = params(p)?;
                let run = run::submit(&self.log, task).map_err(failed)?;
                result(api::Submitted { run })
            }
            "task.continue" => {
                let c: api::Continue = params(p)?;
                let run = run::continue_run(&self.log, &c.run, c.input).map_err(failed)?;
                result(api::Submitted { run })
            }
            "run.start" => {
                self.start(params::<api::RunRef>(p)?.run)?;
                result(api::Done {})
            }
            "run.cancel" => {
                self.cancel(&params::<api::RunRef>(p)?.run)?;
                result(api::Done {})
            }
            "log.read" => {
                let run = params::<api::RunRef>(p)?.run;
                let events = self.log.events(&run).map_err(failed)?;
                result(api::Events { events })
            }
            "host.status" => {
                params::<api::NoParams>(p)?;
                let active = self
                    .active
                    .lock()
                    .expect("active lock")
                    .keys()
                    .cloned()
                    .collect();
                result(api::Status {
                    api: api::VERSION,
                    active,
                })
            }
            "profile.list" => {
                params::<api::NoParams>(p)?;
                result(api::Profiles {
                    profiles: profiles().map_err(failed)?,
                })
            }
            _ => Err((-32601, format!("method not found: {method}"))),
        }
    }

    fn start(&self, id: String) -> Result<(), RpcError> {
        let mut active = self.active.lock().expect("active lock");
        let state = self.log.view(&id).and_then(|v| v.state);
        if state != Some(State::Submitted) || active.contains_key(&id) {
            return Err(failed(format!("run {id} is not waiting to start")));
        }
        let cancel = Arc::new(Cancel::default());
        active.insert(id.clone(), Arc::clone(&cancel));
        drop(active);
        let (log, active) = (Arc::clone(&self.log), Arc::clone(&self.active));
        thread::spawn(move || {
            let _active = Active {
                runs: active,
                id: id.clone(),
            };
            if let Err(e) = run::start(&log, &id, &cancel, &|k| std::env::var(k).ok()) {
                eprintln!("kagu: run {id}: {e}");
            }
        });
        Ok(())
    }

    fn cancel(&self, id: &str) -> Result<(), RpcError> {
        let active = self.active.lock().expect("active lock");
        if let Some(cancel) = active.get(id) {
            cancel.request();
            return Ok(());
        }
        // Submitted but never started: there is no actor to ask.
        if self.log.view(id).and_then(|v| v.state) == Some(State::Submitted) {
            return run::cancel_unstarted(&self.log, id).map_err(failed);
        }
        Err(failed(format!("run {id} is not active")))
    }

    fn subscribe(&self, out: &mut UnixStream, id: Value, p: &Value) {
        let reply = params::<api::Subscribe>(p).and_then(|sub| {
            let snapshot = self.log.snapshot();
            let cursor = sub.after.unwrap_or(snapshot.seq);
            result(snapshot).map(|r| (r, cursor))
        });
        let mut cursor = match reply {
            Ok((r, cursor)) => {
                if !send(out, &json!({"jsonrpc": "2.0", "id": id, "result": r})) {
                    return;
                }
                cursor
            }
            Err((code, message)) => {
                let err = json!({"code": code, "message": message});
                send(out, &json!({"jsonrpc": "2.0", "id": id, "error": err}));
                return;
            }
        };
        loop {
            let Ok(events) = self.log.after(cursor, Duration::from_secs(5)) else {
                return;
            };
            for e in events {
                cursor = e.seq;
                let note = json!({"jsonrpc": "2.0", "method": "log.event", "params": e});
                if !send(out, &note) {
                    return;
                }
            }
        }
    }
}

/// Profile names under `$XDG_CONFIG_HOME/kagu/profiles/`.
fn profiles() -> Result<Vec<String>, String> {
    let dir = crate::paths::config()?.join("profiles");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Ok(Vec::new());
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            name.strip_suffix(".toml").map(str::to_string)
        })
        .collect();
    names.sort();
    Ok(names)
}
