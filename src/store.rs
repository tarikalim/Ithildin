//! The event store: one SQLite file shared by the app, `ithildin mcp` and the importers.

use std::path::{Path, PathBuf};

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension as _, params};
use serde_json::Value;

use crate::contract::Description;

/// Index i holds the migration to user_version i+1.
const MIGRATIONS: &[&str] = &["
CREATE TABLE events(
  id     INTEGER PRIMARY KEY,
  at     INTEGER NOT NULL,                 -- unix ms
  source TEXT NOT NULL,                    -- 'board', 'notes', 'claude', ...
  entity TEXT NOT NULL,                    -- 'card-dune', ...
  kind   TEXT NOT NULL,                    -- 'state', 'activity', 'command', ...
  data   TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(data) AND json_type(data) = 'object'),
  ref    TEXT,                             -- importer's own id; makes re-imports add nothing twice
  UNIQUE (source, ref)
);
CREATE INDEX events_by_time ON events(source, kind, at);
", "
CREATE TABLE sources(
  name    TEXT PRIMARY KEY,
  title   TEXT,
  columns TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(columns) AND json_type(columns) = 'array')
);
", "
-- A configured source is read by Ithildin itself (git-paths, jsonl); the others are fed from outside.
ALTER TABLE sources ADD COLUMN config TEXT CHECK (config IS NULL OR json_valid(config));
ALTER TABLE sources ADD COLUMN synced_at INTEGER;   -- unix ms of the last read
ALTER TABLE sources ADD COLUMN error TEXT;          -- why the last read failed, NULL when it worked
-- How far a jsonl source has read each file, in bytes.
CREATE TABLE cursors(
  source TEXT NOT NULL,
  file   TEXT NOT NULL,
  offset INTEGER NOT NULL,
  PRIMARY KEY (source, file)
);
", "
-- A hidden source is left out of the window and not read (Settings → Sources).
ALTER TABLE sources ADD COLUMN hidden INTEGER NOT NULL DEFAULT 0;
"];

/// One row of `events`.
#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    pub at: i64,
    pub source: String,
    pub entity: String,
    pub kind: String,
    pub data: Value,
    pub r#ref: Option<String>,
}

/// What writing an event did.
#[derive(Debug, PartialEq)]
pub enum Write {
    Added,
    /// An event with the same source and ref existed with other values; it now has these.
    Updated,
    Unchanged,
}

/// What one source holds: used to list sources and their time span.
#[derive(Debug, PartialEq)]
pub struct SourceSummary {
    pub source: String,
    pub kind: String,
    pub count: i64,
    pub first: i64,
    pub last: i64,
}

#[derive(Clone)]
pub struct SourceSetting {
    pub name: String,
    pub title: Option<String>,
    pub config: Option<Value>,
    pub hidden: bool,
    pub synced_at: Option<i64>,
    pub error: Option<String>,
    pub events: i64,
}

pub struct Store {
    conn: Connection,
}

impl Store {
    /// $ITHILDIN_DB, else <data dir>/Ithildin/ithildin.db (~/Library/Application Support on macOS).
    pub fn default_path() -> PathBuf {
        if let Some(p) = std::env::var_os("ITHILDIN_DB") {
            return PathBuf::from(p);
        }
        dirs::data_dir()
            .unwrap_or_default()
            .join("Ithildin")
            .join("ithildin.db")
    }

    pub fn open_default() -> Result<Self> {
        Self::open(&Self::default_path())
    }

    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        Self::init(Connection::open(path)?)
    }

    /// A scratch database: tests, and add_source's dry run.
    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.query_row("PRAGMA journal_mode = WAL", [], |_| Ok(()))?;
        conn.execute_batch("PRAGMA synchronous = NORMAL; PRAGMA busy_timeout = 5000;")?;
        let store = Store { conn };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&self) -> Result<()> {
        let version: i64 = self
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))?;
        for (i, sql) in MIGRATIONS.iter().enumerate().skip(version as usize) {
            let tx = self.conn.unchecked_transaction()?;
            tx.execute_batch(sql)?;
            tx.execute_batch(&format!("PRAGMA user_version = {}", i + 1))?;
            tx.commit()?;
        }
        Ok(())
    }

    /// Runs `f` in one transaction; importers use it so thousands of inserts are one commit.
    pub fn tx<T>(&self, f: impl FnOnce() -> Result<T>) -> Result<T> {
        let tx = self.conn.unchecked_transaction()?;
        let out = f()?;
        tx.commit()?;
        Ok(out)
    }

    /// Changes when another connection (e.g. `ithildin mcp`) commits.
    pub fn data_version(&self) -> Result<i64> {
        Ok(self
            .conn
            .query_row("PRAGMA data_version", [], |r| r.get(0))?)
    }

    /// Adds an event. An event with the same source and ref is replaced, so a sender that
    /// learns more about past events (a title, a group) can send them again.
    pub fn add(&self, e: &Event) -> Result<Write> {
        let existed = match &e.r#ref {
            Some(r) => self
                .conn
                .prepare_cached("SELECT 1 FROM events WHERE source = ?1 AND ref = ?2")?
                .query_row(params![e.source, r], |_| Ok(()))
                .optional()?
                .is_some(),
            None => false,
        };
        let changed = self
            .conn
            .prepare_cached(
                "INSERT INTO events(at, source, entity, kind, data, ref) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(source, ref) DO UPDATE SET at = excluded.at, entity = excluded.entity,
                   kind = excluded.kind, data = excluded.data
                 WHERE at != excluded.at OR entity != excluded.entity OR kind != excluded.kind
                   OR data != excluded.data",
            )?
            .execute(params![e.at, e.source, e.entity, e.kind, e.data.to_string(), e.r#ref])?;
        Ok(match (existed, changed) {
            (false, _) => Write::Added,
            (true, 0) => Write::Unchanged,
            (true, _) => Write::Updated,
        })
    }

    pub fn has_ref(&self, source: &str, r#ref: &str) -> Result<bool> {
        Ok(self
            .conn
            .prepare_cached("SELECT 1 FROM events WHERE source = ?1 AND ref = ?2")?
            .query_row([source, r#ref], |_| Ok(()))
            .optional()?
            .is_some())
    }

    /// Records how a source wants to be shown; replaces an earlier description.
    pub fn describe(&self, d: &Description) -> Result<()> {
        self.conn
            .prepare_cached(
                "INSERT INTO sources(name, title, columns) VALUES (?1, ?2, ?3)
                 ON CONFLICT(name) DO UPDATE SET title = excluded.title, columns = excluded.columns",
            )?
            .execute(params![d.source, d.title, serde_json::to_string(&d.columns)?])?;
        Ok(())
    }

    pub fn description(&self, source: &str) -> Result<Option<Description>> {
        let row = self
            .conn
            .prepare_cached("SELECT title, columns FROM sources WHERE name = ?1")?
            .query_row([source], |r| {
                Ok((r.get::<_, Option<String>>(0)?, r.get::<_, String>(1)?))
            })
            .optional()?;
        Ok(row.map(|(title, columns)| Description {
            source: source.to_string(),
            title,
            columns: serde_json::from_str(&columns).unwrap_or_default(),
        }))
    }

    /// Stores how Ithildin reads a source itself, with its title and columns. A changed config
    /// drops the source's events and read positions, so the next read rebuilds it from scratch
    /// and nothing an earlier config produced stays behind. Returns whether that happened.
    pub fn configure(&self, d: &Description, config: &Value) -> Result<bool> {
        self.tx(|| {
            let changed = self.config(&d.source)?.is_some_and(|old| &old != config);
            if changed {
                self.conn.execute("DELETE FROM events WHERE source = ?1", [&d.source])?;
                self.conn.execute("DELETE FROM cursors WHERE source = ?1", [&d.source])?;
            }
            self.conn
                .prepare_cached(
                    "INSERT INTO sources(name, title, columns, config) VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT(name) DO UPDATE SET title = excluded.title, columns = excluded.columns,
                       config = excluded.config, error = NULL",
                )?
                .execute(params![d.source, d.title, serde_json::to_string(&d.columns)?, config.to_string()])?;
            Ok(changed)
        })
    }

    /// Sources Ithildin reads itself and that are not hidden: (name, config).
    pub fn configured(&self) -> Result<Vec<(String, Value)>> {
        let mut stmt = self.conn.prepare_cached("SELECT name, config FROM sources WHERE config IS NOT NULL AND NOT hidden ORDER BY name")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        rows.map(|r| {
            let (name, config) = r?;
            Ok((name, serde_json::from_str(&config)?))
        })
        .collect()
    }

    pub fn config(&self, source: &str) -> Result<Option<Value>> {
        let config: Option<Option<String>> = self
            .conn
            .prepare_cached("SELECT config FROM sources WHERE name = ?1")?
            .query_row([source], |r| r.get(0))
            .optional()?;
        Ok(config
            .flatten()
            .map(|c| serde_json::from_str(&c))
            .transpose()?)
    }

    /// Records the outcome of reading a configured source.
    pub fn synced(&self, source: &str, at: i64, error: Option<&str>) -> Result<()> {
        self.conn
            .prepare_cached("UPDATE sources SET synced_at = ?2, error = ?3 WHERE name = ?1")?
            .execute(params![source, at, error])?;
        Ok(())
    }

    pub fn cursor(&self, source: &str, file: &str) -> Result<u64> {
        let offset: Option<i64> = self
            .conn
            .prepare_cached("SELECT offset FROM cursors WHERE source = ?1 AND file = ?2")?
            .query_row([source, file], |r| r.get(0))
            .optional()?;
        Ok(offset.unwrap_or(0) as u64)
    }

    pub fn set_cursor(&self, source: &str, file: &str, offset: u64) -> Result<()> {
        self.conn
            .prepare_cached(
                "INSERT INTO cursors(source, file, offset) VALUES (?1, ?2, ?3)
                 ON CONFLICT(source, file) DO UPDATE SET offset = excluded.offset",
            )?
            .execute(params![source, file, offset as i64])?;
        Ok(())
    }

    /// Every source that has events.
    /// Sources with events that are not hidden.
    pub fn source_names(&self) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT DISTINCT source FROM events WHERE source NOT IN (SELECT name FROM sources WHERE hidden) ORDER BY source",
        )?;
        let rows = stmt.query_map([], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Events of a source in [from, to], oldest first. `kind` and `entity` narrow it further.
    pub fn between(
        &self,
        source: &str,
        kind: Option<&str>,
        entity: Option<&str>,
        from: i64,
        to: i64,
        limit: i64,
    ) -> Result<Vec<Event>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT at, source, entity, kind, data, ref FROM events
             WHERE source = ?1 AND (?2 IS NULL OR kind = ?2) AND (?3 IS NULL OR entity = ?3)
               AND at BETWEEN ?4 AND ?5
             ORDER BY at, id LIMIT ?6",
        )?;
        let rows = stmt.query_map(params![source, kind, entity, from, to, limit], row_event)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Deletes a source's events, read positions and settings; returns the events deleted.
    pub fn remove_source(&self, source: &str) -> Result<usize> {
        self.tx(|| {
            let n = self
                .conn
                .execute("DELETE FROM events WHERE source = ?1", [source])?;
            self.conn
                .execute("DELETE FROM cursors WHERE source = ?1", [source])?;
            self.conn
                .execute("DELETE FROM sources WHERE name = ?1", [source])?;
            Ok(n)
        })
    }

    /// Distinct entities per source.
    pub fn entity_counts(&self) -> Result<std::collections::HashMap<String, i64>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT source, count(DISTINCT entity) FROM events GROUP BY source")?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn set_hidden(&self, source: &str, hidden: bool) -> Result<()> {
        self.conn
            .prepare_cached("INSERT INTO sources(name, hidden) VALUES (?1, ?2) ON CONFLICT(name) DO UPDATE SET hidden = excluded.hidden")?
            .execute(params![source, hidden])?;
        Ok(())
    }

    /// Every source, configured or fed from outside, hidden or not, for Settings.
    pub fn source_settings(&self) -> Result<Vec<SourceSetting>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT n.name, s.title, s.config, coalesce(s.hidden, 0), s.synced_at, s.error,
                    (SELECT count(*) FROM events e WHERE e.source = n.name)
             FROM (SELECT name FROM sources UNION SELECT DISTINCT source FROM events) n
             LEFT JOIN sources s ON s.name = n.name ORDER BY n.name",
        )?;
        let rows = stmt.query_map([], |r| {
            let config: Option<String> = r.get(2)?;
            Ok(SourceSetting {
                name: r.get(0)?,
                title: r.get(1)?,
                config: config.and_then(|c| serde_json::from_str(&c).ok()),
                hidden: r.get(3)?,
                synced_at: r.get(4)?,
                error: r.get(5)?,
                events: r.get(6)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn sources(&self) -> Result<Vec<SourceSummary>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT source, kind, count(*), min(at), max(at) FROM events
             GROUP BY source, kind ORDER BY source, kind",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(SourceSummary {
                source: r.get(0)?,
                kind: r.get(1)?,
                count: r.get(2)?,
                first: r.get(3)?,
                last: r.get(4)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

fn row_event(r: &rusqlite::Row) -> rusqlite::Result<Event> {
    let data: String = r.get(4)?;
    Ok(Event {
        at: r.get(0)?,
        source: r.get(1)?,
        entity: r.get(2)?,
        kind: r.get(3)?,
        // The CHECK constraint guarantees a JSON object.
        data: serde_json::from_str(&data).unwrap_or_default(),
        r#ref: r.get(5)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ev(at: i64, entity: &str, kind: &str, data: Value, r: Option<&str>) -> Event {
        Event {
            at,
            source: "s".into(),
            entity: entity.into(),
            kind: kind.into(),
            data,
            r#ref: r.map(Into::into),
        }
    }

    #[test]
    fn hidden_sources_leave_the_window_and_the_reader() -> Result<()> {
        let s = Store::open_in_memory()?;
        s.add(&ev(1, "e", "x", json!({}), None))?;
        let d = Description {
            source: "git".into(),
            title: None,
            columns: vec![],
        };
        s.configure(&d, &json!({ "type": "git-paths" }))?;
        s.add(&Event {
            source: "git".into(),
            ..ev(1, "e", "x", json!({}), None)
        })?;
        assert_eq!(s.source_names()?, ["git", "s"]);
        s.set_hidden("git", true)?;
        s.set_hidden("s", true)?; // fed from outside: no sources row yet
        assert!(s.source_names()?.is_empty() && s.configured()?.is_empty());
        let rows = s.source_settings()?;
        assert_eq!(
            rows.iter()
                .map(|r| (r.name.as_str(), r.hidden, r.events, r.config.is_some()))
                .collect::<Vec<_>>(),
            [("git", true, 1, true), ("s", true, 1, false)]
        );
        s.set_hidden("s", false)?;
        assert_eq!(s.source_names()?, ["s"]);
        Ok(())
    }

    #[test]
    fn describe_replaces_the_earlier_description() -> Result<()> {
        let s = Store::open_in_memory()?;
        assert_eq!(s.description("s")?, None);
        let d = |cols: &[&str]| Description {
            source: "s".into(),
            title: Some("S".into()),
            columns: cols.iter().map(|c| c.to_string()).collect(),
        };
        s.describe(&d(&["new"]))?;
        s.describe(&d(&["new", "done"]))?;
        assert_eq!(s.description("s")?, Some(d(&["new", "done"])));
        Ok(())
    }

    #[test]
    fn reconfiguring_a_source_drops_what_the_old_config_made() -> Result<()> {
        let s = Store::open_in_memory()?;
        let d = Description {
            source: "s".into(),
            title: None,
            columns: vec![],
        };
        assert!(!s.configure(&d, &json!({ "type": "jsonl", "v": 1 }))?);
        s.add(&ev(1, "a", "note", json!({}), Some("r")))?;
        s.set_cursor("s", "f", 10)?;
        assert!(
            !s.configure(&d, &json!({ "type": "jsonl", "v": 1 }))?,
            "same config: kept"
        );
        assert_eq!(s.between("s", None, None, 0, 9, 9)?.len(), 1);
        assert!(
            s.configure(&d, &json!({ "type": "jsonl", "v": 2 }))?,
            "new config: rebuilt"
        );
        assert!(s.between("s", None, None, 0, 9, 9)?.is_empty());
        assert_eq!(s.cursor("s", "f")?, 0);
        Ok(())
    }

    #[test]
    fn add_is_idempotent_by_ref() -> Result<()> {
        let s = Store::open_in_memory()?;
        assert_eq!(
            s.add(&ev(1, "a", "state", json!({"to": "new"}), Some("c1/a")))?,
            Write::Added
        );
        assert_eq!(
            s.add(&ev(1, "a", "state", json!({"to": "new"}), Some("c1/a")))?,
            Write::Unchanged
        );
        // Same ref, richer data: the stored event is replaced, not duplicated.
        assert_eq!(
            s.add(&ev(
                1,
                "a",
                "state",
                json!({"to": "new", "label": "A"}),
                Some("c1/a")
            ))?,
            Write::Updated
        );
        assert_eq!(s.between("s", None, None, 0, 9, 9)?[0].data["label"], "A");
        // Events without a ref are never deduplicated.
        assert_eq!(s.add(&ev(2, "a", "note", json!({}), None))?, Write::Added);
        assert_eq!(s.add(&ev(2, "a", "note", json!({}), None))?, Write::Added);
        Ok(())
    }

    #[test]
    fn rejects_data_that_is_not_an_object() -> Result<()> {
        let s = Store::open_in_memory()?;
        assert!(s.add(&ev(1, "a", "x", json!([1, 2]), None)).is_err());
        Ok(())
    }
}
