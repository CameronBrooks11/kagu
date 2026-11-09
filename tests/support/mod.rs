//! A kagu host for one test: started on a private database and socket,
//! killed on drop.

// Shared by several test crates; each uses part of it.
#![allow(dead_code)]

use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

pub struct Host {
    child: Child,
    pub db: PathBuf,
    pub socket: PathBuf,
    env: Vec<(String, String)>,
}

impl Host {
    pub fn start(dir: &Path, env: &[(&str, &str)]) -> Self {
        let env: Vec<(String, String)> = env
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let db = dir.join("kagu.db");
        let socket = dir.join("sock").join("kagu.sock");
        let child = spawn(&db, &socket, &env);
        Self {
            child,
            db,
            socket,
            env,
        }
    }

    /// A CLI command against this host.
    pub fn cli(&self, args: &[&str]) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_kagu"));
        c.args(args).arg("--socket").arg(&self.socket);
        c
    }

    /// Kill the host as a crash would, and start it again on the same log.
    pub fn crash_and_restart(&mut self) {
        self.child.kill().unwrap();
        self.child.wait().unwrap();
        self.child = spawn(&self.db, &self.socket, &self.env);
    }
}

fn spawn(db: &Path, socket: &Path, env: &[(String, String)]) -> Child {
    let child = Command::new(env!("CARGO_BIN_EXE_kagu"))
        .arg("serve")
        .arg("--db")
        .arg(db)
        .arg("--socket")
        .arg(socket)
        .envs(env.iter().map(|(k, v)| (k, v)))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while UnixStream::connect(socket).is_err() {
        assert!(Instant::now() < deadline, "host did not start");
        std::thread::sleep(Duration::from_millis(20));
    }
    child
}

impl Drop for Host {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
