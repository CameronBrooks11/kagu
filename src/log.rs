//! The host's event log: one writer at a time, an in-memory projection of
//! run states rebuilt from the log at start, and a way for subscribers to wait
//! for new events.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::Duration;

use serde_json::Value;

use crate::api::Snapshot;
use crate::event::{Event, RunView, Source};
use crate::store::Store;

/// Events read per batch when rebuilding or replaying.
const BATCH: i64 = 1024;

struct Inner {
    store: Store,
    /// Projection: every run's view. Rebuildable from the log (Invariant 1).
    runs: BTreeMap<String, RunView>,
    /// Highest committed seq.
    last: i64,
}

pub struct Log {
    inner: Mutex<Inner>,
    appended: Condvar,
}

impl Log {
    pub fn open(path: &Path) -> Result<Self, String> {
        let store = Store::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut inner = Inner {
            store,
            runs: BTreeMap::new(),
            last: 0,
        };
        loop {
            let batch = inner
                .store
                .after(inner.last, BATCH)
                .map_err(|e| e.to_string())?;
            let Some(tail) = batch.last() else { break };
            inner.last = tail.seq;
            for e in &batch {
                apply(&mut inner.runs, e)?;
            }
        }
        Ok(Self {
            inner: Mutex::new(inner),
            appended: Condvar::new(),
        })
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().expect("log lock")
    }

    pub fn append(
        &self,
        run: &str,
        source: Source,
        kind: &str,
        v: u32,
        data: Value,
    ) -> Result<Event, String> {
        let mut inner = self.lock();
        let event = inner
            .store
            .append_value(run, source, kind, v, data)
            .map_err(|e| format!("event log: {e}"))?;
        inner.last = event.seq;
        apply(&mut inner.runs, &event)?;
        drop(inner);
        self.appended.notify_all();
        Ok(event)
    }

    pub fn view(&self, run: &str) -> Option<RunView> {
        self.lock().runs.get(run).cloned()
    }

    /// Non-terminal runs and the seq they reflect, read together.
    pub fn snapshot(&self) -> Snapshot {
        let inner = self.lock();
        let runs = inner
            .runs
            .iter()
            .filter(|(_, v)| !v.state.is_some_and(|s| s.is_terminal()))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        Snapshot {
            runs,
            seq: inner.last,
        }
    }

    /// Events after `seq`, waiting up to `wait` for one if there are none.
    pub fn after(&self, seq: i64, wait: Duration) -> Result<Vec<Event>, String> {
        let mut inner = self.lock();
        if inner.last <= seq {
            inner = self
                .appended
                .wait_timeout_while(inner, wait, |i| i.last <= seq)
                .expect("log lock")
                .0;
        }
        inner.store.after(seq, BATCH).map_err(|e| e.to_string())
    }

    pub fn events(&self, run: &str) -> Result<Vec<Event>, String> {
        self.lock().store.events(run).map_err(|e| e.to_string())
    }
}

fn apply(runs: &mut BTreeMap<String, RunView>, e: &Event) -> Result<(), String> {
    runs.entry(e.run.clone())
        .or_default()
        .apply(e)
        .map_err(|err| format!("event {}: {err}", e.seq))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use serde_json::json;

    use super::Log;
    use crate::event::Source;

    /// A subscriber that snapshots, then follows while others append, sees
    /// every later event exactly once and in order.
    #[test]
    fn snapshot_then_follow_misses_and_repeats_nothing() {
        let path = std::env::temp_dir().join(format!("kagu-log-{}.db", std::process::id()));
        let log = Arc::new(Log::open(&path).unwrap());
        // Writers append a first batch, wait for the snapshot, then append
        // a second batch while the subscriber follows.
        let first = Arc::new(std::sync::Barrier::new(5));
        let snapshotted = Arc::new(std::sync::Barrier::new(5));
        let writers: Vec<_> = (0..4)
            .map(|w| {
                let (log, first, snapshotted) = (
                    Arc::clone(&log),
                    Arc::clone(&first),
                    Arc::clone(&snapshotted),
                );
                std::thread::spawn(move || {
                    let run = format!("r{w}");
                    for i in 0..200 {
                        if i == 100 {
                            first.wait();
                            snapshotted.wait();
                        }
                        log.append(&run, Source::Host, "test.tick", 1, json!(i))
                            .unwrap();
                    }
                })
            })
            .collect();
        first.wait();
        let snapshot = log.snapshot();
        snapshotted.wait();
        let mut seen = Vec::new();
        let mut cursor = snapshot.seq;
        loop {
            let batch = log.after(cursor, Duration::from_millis(200)).unwrap();
            if batch.is_empty() && writers.iter().all(|w| w.is_finished()) {
                break;
            }
            for e in batch {
                cursor = e.seq;
                seen.push(e.seq);
            }
        }
        for w in writers {
            w.join().unwrap();
        }
        // Computed independently of `after`: every run's events, by seq.
        let mut expected: Vec<i64> = (0..4)
            .flat_map(|w| log.events(&format!("r{w}")).unwrap())
            .map(|e| e.seq)
            .filter(|&seq| seq > snapshot.seq)
            .collect();
        expected.sort_unstable();
        drop(log);
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
        assert_eq!(expected.len(), 400, "the second batch follows the snapshot");
        assert_eq!(seen, expected);
    }
}
