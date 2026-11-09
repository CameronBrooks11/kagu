//! The event log: one SQLite database in WAL mode, insert-only.

use std::path::Path;

use rusqlite::{Connection, params};
use serde_json::Value;

use crate::event::{Event, Payload, Source};

pub struct Store {
    conn: Connection,
}

impl Store {
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "FULL")?;
        // AUTOINCREMENT: a deleted seq is never reused, so seq only grows.
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS events (
                seq    INTEGER PRIMARY KEY AUTOINCREMENT,
                run    TEXT NOT NULL,
                ts     TEXT NOT NULL,
                source TEXT NOT NULL,
                type   TEXT NOT NULL,
                v      INTEGER NOT NULL,
                data   TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS events_run ON events (run, seq);",
        )?;
        Ok(Self { conn })
    }

    /// Append one event in its own transaction and return it with its seq.
    pub fn append<P: Payload>(
        &self,
        run: &str,
        source: Source,
        payload: &P,
    ) -> rusqlite::Result<Event> {
        let data = serde_json::to_value(payload).expect("payload serializes");
        self.append_value(run, source, P::TYPE, P::V, data)
    }

    fn append_value(
        &self,
        run: &str,
        source: Source,
        kind: &str,
        v: u32,
        data: Value,
    ) -> rusqlite::Result<Event> {
        let ts = crate::time::now();
        let source_text = serde_json::to_value(source).expect("source serializes");
        self.conn.execute(
            "INSERT INTO events (run, ts, source, type, v, data) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                run,
                ts,
                source_text.as_str().expect("source is a string"),
                kind,
                v,
                data.to_string()
            ],
        )?;
        Ok(Event {
            seq: self.conn.last_insert_rowid(),
            run: run.to_string(),
            ts,
            source,
            kind: kind.to_string(),
            v,
            data,
        })
    }

    /// A run's events in seq order.
    pub fn events(&self, run: &str) -> rusqlite::Result<Vec<Event>> {
        let mut stmt = self.conn.prepare(
            "SELECT seq, run, ts, source, type, v, data FROM events WHERE run = ?1 ORDER BY seq",
        )?;
        let rows = stmt.query_map([run], |r| {
            let source: String = r.get(3)?;
            let data: String = r.get(6)?;
            Ok(Event {
                seq: r.get(0)?,
                run: r.get(1)?,
                ts: r.get(2)?,
                source: serde_json::from_value(Value::String(source)).map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        3,
                        rusqlite::types::Type::Text,
                        e.into(),
                    )
                })?,
                kind: r.get(4)?,
                v: r.get(5)?,
                data: serde_json::from_str(&data).map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        6,
                        rusqlite::types::Type::Text,
                        e.into(),
                    )
                })?,
            })
        })?;
        rows.collect()
    }
}
