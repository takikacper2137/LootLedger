//! SQLite schema v3, shared unchanged with the Python release. Transactions preserve corrections.
use crate::model::Replay;
use chrono::{DateTime, Duration, Local, NaiveDateTime, TimeZone, Utc};
use rusqlite::{Connection, OpenFlags, OptionalExtension, backup::Backup, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::Duration as StdDuration,
};
use thiserror::Error;
use uuid::Uuid;

pub const SCHEMA_V1: &str = "
CREATE TABLE IF NOT EXISTS matches (
 id TEXT PRIMARY KEY, started TEXT NOT NULL, ended TEXT, seconds REAL NOT NULL DEFAULT 0);
CREATE UNIQUE INDEX IF NOT EXISTS one_active ON matches((1)) WHERE ended IS NULL;
CREATE TABLE IF NOT EXISTS events (
 id TEXT PRIMARY KEY, match_id TEXT NOT NULL REFERENCES matches(id),
 created TEXT NOT NULL, video_time REAL, status TEXT NOT NULL
 CHECK(status IN ('confirmed','review','undone','rejected')),
 chest_type TEXT NOT NULL, amount INTEGER NOT NULL CHECK(amount > 0),
 source TEXT NOT NULL, confidence REAL NOT NULL, reason TEXT NOT NULL, evidence TEXT);
CREATE TABLE IF NOT EXISTS audit (
 id INTEGER PRIMARY KEY, created TEXT NOT NULL, event_id TEXT,
 action TEXT NOT NULL, detail TEXT NOT NULL);";
pub const SCHEMA_V2: &str = "
CREATE TABLE IF NOT EXISTS match_meta (
 match_id TEXT PRIMARY KEY REFERENCES matches(id), key TEXT NOT NULL UNIQUE, meta TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS replay_files (
 file TEXT PRIMARY KEY, size INTEGER NOT NULL, mtime REAL NOT NULL,
 status TEXT NOT NULL CHECK(status IN ('imported','skipped')),
 match_id TEXT REFERENCES matches(id), scanned TEXT NOT NULL);";
pub const SCHEMA_V3: &str = "
CREATE TABLE IF NOT EXISTS match_notes (
 match_id TEXT PRIMARY KEY REFERENCES matches(id), note TEXT NOT NULL DEFAULT '',
 tags TEXT NOT NULL DEFAULT '', favorite INTEGER NOT NULL DEFAULT 0);";

#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    Sql(#[from] rusqlite::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Csv(#[from] csv::Error),
    #[error("{0}")]
    Invalid(String),
}
pub type Result<T> = std::result::Result<T, Error>;

pub struct Store {
    pub db: Connection,
    pub path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Event {
    pub id: String,
    pub match_id: String,
    pub created: String,
    pub video_time: Option<f64>,
    pub status: String,
    pub chest_type: String,
    pub amount: i64,
    pub source: String,
    pub confidence: f64,
    pub reason: String,
    pub evidence: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Match {
    pub id: String,
    pub started: String,
    pub ended: Option<String>,
    pub seconds: f64,
    pub total: i64,
    pub broken: i64,
    pub meta: Option<Value>,
    pub note: String,
    pub tags: String,
    pub favorite: bool,
}

#[derive(Debug, Serialize, Default)]
pub struct Totals {
    pub chests: i64,
    pub broken: i64,
    pub matches: i64,
    pub seconds: f64,
    pub best: i64,
}

fn now() -> String {
    Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, false)
}
fn invalid<T>(message: impl Into<String>) -> Result<T> {
    Err(Error::Invalid(message.into()))
}

impl Store {
    fn is_ledger_file(&self, path: &Path) -> bool {
        // SQLite's recovery files must never become export or backup destinations.
        let bases = [
            self.path.clone(),
            fs::canonicalize(&self.path).unwrap_or_else(|_| self.path.clone()),
        ];
        bases.iter().any(|base| {
            ["", "-wal", "-shm", "-journal"].iter().any(|suffix| {
                let mut name = base.as_os_str().to_os_string();
                name.push(suffix);
                same_file(Path::new(&name), path)
            })
        })
    }
    pub fn read_only(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_owned();
        let db = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        db.busy_timeout(StdDuration::from_secs(5))?;
        db.execute_batch("PRAGMA trusted_schema=OFF; PRAGMA query_only=ON;")?;
        let version: i64 = db.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version != 3 {
            return invalid(format!(
                "Expected ledger schema 3, found {version}; open it in the Python app first"
            ));
        }
        Ok(Self { db, path })
    }

    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_owned();
        if path.as_os_str().is_empty() {
            return invalid("Choose a ledger file");
        }
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent)?;
        }
        let db = Connection::open(&path)?;
        db.busy_timeout(StdDuration::from_secs(5))?;
        db.execute_batch(
            "PRAGMA foreign_keys=ON; PRAGMA trusted_schema=OFF; PRAGMA synchronous=FULL;",
        )?;
        let version: i64 = db.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if !(0..=3).contains(&version) {
            return invalid(format!("Unsupported ledger schema {version}"));
        }
        if version == 0
            && db.query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table'",
                [],
                |r| r.get::<_, i64>(0),
            )? != 0
        {
            return invalid("Unversioned database contains unrelated tables");
        }
        let mut store = Self { db, path };
        if version > 0 && version < 3 {
            let backup = store
                .path
                .parent()
                .unwrap_or(Path::new("."))
                .join("backups")
                .join(format!("before-schema-v3-{}.sqlite", Uuid::new_v4()));
            store.backup(&backup)?;
        }
        let tx = store.db.transaction()?;
        tx.execute_batch(&format!(
            "{SCHEMA_V1}{SCHEMA_V2}{SCHEMA_V3}PRAGMA user_version=3;"
        ))?;
        tx.commit()?;
        store.db.execute_batch("PRAGMA journal_mode=WAL;")?;
        Ok(store)
    }

    pub fn totals(&self, include_broken: bool) -> Result<Totals> {
        let mut totals = self.db.query_row("SELECT coalesce(sum(amount),0), coalesce(sum(CASE WHEN source='replay-broken' THEN amount ELSE 0 END),0) FROM events WHERE status='confirmed' AND (?1 OR source!='replay-broken')", [include_broken], |r| Ok(Totals { chests: r.get(0)?, broken: r.get(1)?, ..Totals::default() }))?;
        (totals.matches, totals.seconds) = self.db.query_row(
            "SELECT count(*), coalesce(sum(seconds),0) FROM matches",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        totals.best = self.db.query_row("SELECT coalesce(max(n),0) FROM (SELECT sum(amount) n FROM events WHERE status='confirmed' AND (?1 OR source!='replay-broken') GROUP BY match_id)", [include_broken], |r| r.get(0))?;
        Ok(totals)
    }

    pub fn entries(&self, offset: u32, limit: u32) -> Result<Vec<Event>> {
        let mut stmt = self.db.prepare("SELECT id,match_id,created,video_time,status,chest_type,amount,source,confidence,reason,evidence FROM events ORDER BY created DESC, rowid DESC LIMIT ?1 OFFSET ?2")?;
        Ok(stmt
            .query_map(params![limit, offset], row_event)?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Replay files already read: name -> (size, mtime).
    pub fn replay_files(&self) -> Result<std::collections::HashMap<String, (i64, f64)>> {
        let mut stmt = self
            .db
            .prepare("SELECT file, size, mtime FROM replay_files")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                (r.get::<_, i64>(1)?, r.get::<_, f64>(2)?),
            ))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Replay files whose match was read before per-player details existed.
    pub fn files_without_players(&self) -> Result<std::collections::HashSet<String>> {
        let mut stmt = self.db.prepare(
            "SELECT r.file FROM replay_files r JOIN match_meta m ON m.match_id=r.match_id
             WHERE r.status='imported' AND (json_extract(m.meta,'$.players') IS NULL OR json_extract(m.meta,'$.stats') IS NULL)",
        )?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Read every replay again on the next scan; imports are idempotent and keep corrections.
    pub fn forget_replay_files(&mut self) -> Result<()> {
        self.db.execute("DELETE FROM replay_files", [])?;
        Ok(())
    }

    /// (replay files read, of which could not be read).
    pub fn replay_file_counts(&self) -> Result<(i64, i64)> {
        Ok(self.db.query_row(
            "SELECT count(*), coalesce(sum(status='skipped'),0) FROM replay_files",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?)
    }

    /// Whether the ledger holds a match from this game session (any recorder).
    pub fn has_session(&self, session: &str) -> Result<bool> {
        Ok(self.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM match_meta WHERE key LIKE ?1)",
            params![format!("{session}:%")],
            |r| r.get(0),
        )?)
    }

    /// The match a replay file was imported as, if any.
    pub fn match_for_file(&self, file: &str) -> Result<Option<String>> {
        Ok(self
            .db
            .query_row(
                "SELECT match_id FROM replay_files WHERE file=?1 AND status='imported'",
                params![file],
                |r| r.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten())
    }

    /// Counted openings in one match with a time inside the replay (for the timeline).
    pub fn match_marks(
        &self,
        match_id: &str,
        include_broken: bool,
    ) -> Result<Vec<(f64, String, String)>> {
        let mut stmt = self.db.prepare(
            "SELECT video_time, chest_type, source FROM events WHERE match_id=?1 AND status='confirmed'
             AND (?2 OR source!='replay-broken') AND video_time IS NOT NULL ORDER BY video_time",
        )?;
        let rows = stmt.query_map(params![match_id, include_broken], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Counted openings (confirmed, broken-open ones only when counted) for the statistics.
    pub fn counted_events(&self, include_broken: bool) -> Result<Vec<crate::stats::Ev>> {
        let mut stmt = self.db.prepare(
            "SELECT created, amount, chest_type, source, match_id FROM events
             WHERE status='confirmed' AND (?1 OR source!='replay-broken')",
        )?;
        let rows = stmt.query_map(params![include_broken], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, Option<String>>(4)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (created, amount, chest_type, source, match_id) = row?;
            out.push(crate::stats::Ev {
                when: local_time(&created)?,
                amount,
                chest_type,
                source,
                match_id,
            });
        }
        Ok(out)
    }

    /// Everything the dashboard shows, computed from the ledger.
    pub fn snapshot(
        &self,
        include_broken: bool,
        session_start: Option<DateTime<Local>>,
    ) -> Result<crate::stats::Snapshot> {
        let events = self.counted_events(include_broken)?;
        // Partner recordings of the same matches raise the teammates' numbers; they never touch the ledger.
        let remotes = crate::shared::load(&crate::shared::path_for(&self.path));
        let matches = self
            .history(include_broken, 0, u32::MAX)?
            .into_iter()
            .map(|mut m| {
                if let Some(meta) = m.meta.as_mut() {
                    crate::shared::enrich(meta, &remotes, include_broken);
                }
                Ok(crate::stats::MatchIn {
                    started: local_time(&m.started)?,
                    id: m.id,
                    seconds: m.seconds,
                    total: m.total,
                    broken: m.broken,
                    meta: m.meta,
                    note: m.note,
                    tags: m.tags,
                    favorite: m.favorite,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(crate::stats::compute(
            &events,
            &matches,
            Local::now(),
            session_start,
            include_broken,
        ))
    }

    /// PRAGMA data_version plus our own changes: changes when anyone (this or the old app) writes.
    pub fn version(&self) -> Result<(i64, u64)> {
        Ok((
            self.db.query_row("PRAGMA data_version", [], |r| r.get(0))?,
            self.db.total_changes(),
        ))
    }

    pub fn history(&self, include_broken: bool, offset: u32, limit: u32) -> Result<Vec<Match>> {
        let mut stmt = self.db.prepare("SELECT m.id,m.started,m.ended,m.seconds,
            coalesce(sum(CASE WHEN e.status='confirmed' AND (?1 OR e.source!='replay-broken') THEN e.amount ELSE 0 END),0),
            coalesce(sum(CASE WHEN e.status='confirmed' AND ?1 AND e.source='replay-broken' THEN e.amount ELSE 0 END),0),
            mm.meta, coalesce(n.note,''), coalesce(n.tags,''), coalesce(n.favorite,0)
            FROM matches m LEFT JOIN events e ON m.id=e.match_id LEFT JOIN match_meta mm ON mm.match_id=m.id
            LEFT JOIN match_notes n ON n.match_id=m.id GROUP BY m.id ORDER BY m.started DESC,m.rowid DESC LIMIT ?2 OFFSET ?3")?;
        let rows = stmt.query_map(params![include_broken, limit, offset], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, f64>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, i64>(5)?,
                r.get::<_, Option<String>>(6)?,
                r.get::<_, String>(7)?,
                r.get::<_, String>(8)?,
                r.get::<_, bool>(9)?,
            ))
        })?;
        rows.map(|row| {
            let (id, started, ended, seconds, total, broken, meta, note, tags, favorite) = row?;
            Ok(Match {
                id,
                started,
                ended,
                seconds,
                total,
                broken,
                meta: meta.map(|s| serde_json::from_str(&s)).transpose()?,
                note,
                tags,
                favorite,
            })
        })
        .collect()
    }

    pub fn import_replay(
        &mut self,
        replay: &Replay,
        file: &str,
        size: u64,
        mtime: f64,
    ) -> Result<Option<String>> {
        replay.validate().map_err(Error::Invalid)?;
        if !mtime.is_finite() || size > i64::MAX as u64 {
            return invalid("Invalid replay file metadata");
        }
        let stamp = now();
        let tx = self.db.transaction()?;
        let has_owner = replay.owner.as_ref().is_some_and(|p| {
            p.epic_id.is_some() || p.name.is_some() || p.team.is_some() || p.bot.is_some()
        });
        let id = if !has_owner || replay.events.is_empty() {
            None
        } else {
            Some(replay.match_id())
        };
        if let Some(ref id) = id {
            let key = replay.key();
            let (mine, mut meta) = replay.summarize();
            meta["key"] = json!(key);
            let start = if let Some(start) = replay.timestamp.as_deref().and_then(parse_start) {
                start
            } else {
                file_timestamp(mtime)?
                    .checked_sub_signed(Duration::microseconds(
                        (replay.length_seconds * 1_000_000.0).round_ties_even() as i64,
                    ))
                    .ok_or_else(|| Error::Invalid("Replay timestamp is out of range".into()))?
            };
            let end = start
                .checked_add_signed(Duration::microseconds(
                    (replay.length_seconds * 1_000_000.0).round_ties_even() as i64,
                ))
                .ok_or_else(|| Error::Invalid("Replay timestamp is out of range".into()))?;
            tx.execute("INSERT INTO matches(id,started,ended,seconds) VALUES(?1,?2,?3,?4) ON CONFLICT(id) DO UPDATE SET ended=excluded.ended,seconds=excluded.seconds",
                params![id, iso(start), iso(end), replay.length_seconds])?;
            tx.execute("INSERT INTO match_meta(match_id,key,meta) VALUES(?1,?2,?3) ON CONFLICT(match_id) DO UPDATE SET meta=excluded.meta", params![id, key, meta.to_string()])?;
            let mut added = 0;
            for (event, kind) in mine {
                let created = start
                    .checked_add_signed(Duration::microseconds(
                        (event.t * 1_000_000.0).round_ties_even() as i64,
                    ))
                    .ok_or_else(|| Error::Invalid("Opening timestamp is out of range".into()))?;
                let description = if event.kind == "opened" {
                    "Opened"
                } else {
                    "Broken open (not searched)"
                };
                added += tx.execute("INSERT INTO events VALUES(?1,?2,?3,?4,'confirmed',?5,1,?6,1.0,?7,NULL) ON CONFLICT(id) DO NOTHING", params![
                    replay.event_id(event), id, iso(created), event.t, kind, if event.kind == "opened" {"replay"} else {"replay-broken"},
                    format!("{description} · {} · {} at {:.1}s", event.cls, meta["file"].as_str().unwrap_or(""), event.t)])?;
            }
            if added > 0 {
                tx.execute("INSERT INTO audit(created,event_id,action,detail) VALUES(?1,NULL,'replay imported',?2)", params![stamp, json!({"match": id, "file": file, "added": added}).to_string()])?;
            }
        }
        tx.execute("INSERT INTO replay_files VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(file) DO UPDATE SET size=excluded.size,mtime=excluded.mtime,status=excluded.status,match_id=excluded.match_id,scanned=excluded.scanned",
            params![file, size as i64, mtime, if id.is_some() {"imported"} else {"skipped"}, id, stamp])?;
        tx.commit()?;
        Ok(id)
    }

    pub fn edit(
        &mut self,
        id: &str,
        status: Option<&str>,
        chest_type: Option<&str>,
        amount: Option<i64>,
    ) -> Result<()> {
        if status.is_some_and(|s| !["confirmed", "review", "undone", "rejected"].contains(&s)) {
            return invalid("Invalid event status");
        }
        if chest_type.is_some_and(|s| s.trim().is_empty() || s.trim().chars().count() > 80) {
            return invalid("Type must contain 1–80 characters");
        }
        if amount.is_some_and(|n| !(1..=10000).contains(&n)) {
            return invalid("Count must be between 1 and 10000");
        }
        let tx = self.db.transaction()?;
        let before: Option<Event> = tx.query_row("SELECT id,match_id,created,video_time,status,chest_type,amount,source,confidence,reason,evidence FROM events WHERE id=?1", [id], row_event).optional()?;
        let Some(before) = before else {
            return invalid("Entry no longer exists");
        };
        let mut after = before.clone();
        if let Some(status) = status {
            after.status = status.to_owned();
        }
        if let Some(kind) = chest_type {
            after.chest_type = kind.trim().to_owned();
        }
        if let Some(amount) = amount {
            after.amount = amount;
        }
        tx.execute("UPDATE events SET status=coalesce(?1,status),chest_type=coalesce(?2,chest_type),amount=coalesce(?3,amount) WHERE id=?4", params![status, chest_type.map(str::trim), amount, id])?;
        tx.execute(
            "INSERT INTO audit(created,event_id,action,detail) VALUES(?1,?2,'edited',?3)",
            params![
                now(),
                id,
                json!({"before": before, "after": after}).to_string()
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn undo(&mut self) -> Result<bool> {
        let id: Option<String> = self.db.query_row("SELECT id FROM events WHERE status='confirmed' ORDER BY created DESC,rowid DESC LIMIT 1", [], |r| r.get(0)).optional()?;
        if let Some(id) = id {
            self.edit(&id, Some("undone"), None, None)?;
            return Ok(true);
        }
        Ok(false)
    }

    pub fn add_correction(&mut self, amount: i64, chest_type: &str) -> Result<String> {
        if !(1..=10000).contains(&amount) {
            return invalid("Count must be between 1 and 10000");
        }
        if chest_type.trim().is_empty() || chest_type.trim().chars().count() > 80 {
            return invalid("Type must contain 1–80 characters");
        }
        let tx = self.db.transaction()?;
        let match_id: Option<String> = tx
            .query_row(
                "SELECT id FROM matches ORDER BY ended IS NULL DESC, started DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .optional()?;
        let Some(match_id) = match_id else {
            return invalid(
                "No match yet. Play a match with replays enabled, then correct it here.",
            );
        };
        let id = Uuid::new_v4().to_string();
        let stamp = now();
        tx.execute("INSERT INTO events VALUES(?1,?2,?3,NULL,'confirmed',?4,?5,'manual correction',1.0,'Manual correction',NULL)", params![id, match_id, stamp, chest_type.trim(), amount])?;
        tx.execute(
            "INSERT INTO audit(created,event_id,action,detail) VALUES(?1,?2,'added',?3)",
            params![
                stamp,
                id,
                json!({"amount": amount, "type": chest_type.trim()}).to_string()
            ],
        )?;
        tx.commit()?;
        Ok(id)
    }

    pub fn set_note(&mut self, id: &str, note: &str, tags: &str, favorite: bool) -> Result<()> {
        if note.chars().count() > 2000 || tags.chars().count() > 2000 {
            return invalid("Notes and tags are limited to 2000 characters");
        }
        let mut unique = Vec::new();
        for tag in tags.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            if !unique.contains(&tag) {
                unique.push(tag);
            }
        }
        self.db.execute("INSERT INTO match_notes VALUES(?1,?2,?3,?4) ON CONFLICT(match_id) DO UPDATE SET note=excluded.note,tags=excluded.tags,favorite=excluded.favorite", params![id, note.trim(), unique.join(", "), favorite])?;
        Ok(())
    }

    pub fn backup(&self, path: &Path) -> Result<()> {
        if self.is_ledger_file(path) {
            return invalid("Choose a different backup file");
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let temp = path.with_extension(format!("{}.tmp", Uuid::new_v4()));
        let result = (|| {
            let mut target = Connection::open(&temp)?;
            Backup::new(&self.db, &mut target)?.run_to_completion(
                128,
                StdDuration::from_millis(5),
                None,
            )?;
            target.close().map_err(|(_, e)| e)?;
            fs::OpenOptions::new().write(true).open(&temp)?.sync_all()?;
            fs::rename(&temp, path)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temp);
        }
        result
    }

    pub fn export(&self, path: &Path) -> Result<()> {
        if self.is_ledger_file(path) {
            return invalid("Choose an export file outside the ledger");
        }
        let events = self.entries(0, u32::MAX)?;
        let csv = path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("csv"));
        let temp = path.with_extension(format!("{}.tmp", Uuid::new_v4()));
        let result = (|| {
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp)?;
            if csv {
                file.write_all(&[0xef, 0xbb, 0xbf])?;
                let mut writer = csv::Writer::from_writer(&mut file);
                let fields = [
                    "id",
                    "match_id",
                    "created",
                    "video_time",
                    "status",
                    "chest_type",
                    "amount",
                    "source",
                    "confidence",
                    "reason",
                    "evidence",
                ];
                writer.write_record(fields)?;
                for event in &events {
                    let value = serde_json::to_value(event)?;
                    writer.write_record(fields.map(|key| match &value[key] {
                        Value::Null => String::new(),
                        Value::String(s) if s.starts_with(['=', '+', '-', '@']) => format!("'{s}"),
                        Value::String(s) => s.clone(),
                        other => other.to_string(),
                    }))?;
                }
                writer.flush()?;
            } else {
                let mut matches = Vec::new();
                for m in self.history(true, 0, u32::MAX)? {
                    matches.push(json!({"id": m.id, "started": m.started, "ended": m.ended,
                    "seconds": m.seconds, "total": m.total, "broken": m.broken, "meta": m.meta}));
                }
                let mut notes = serde_json::Map::new();
                let mut stmt = self
                    .db
                    .prepare("SELECT match_id,note,tags,favorite FROM match_notes")?;
                for row in stmt.query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, bool>(3)?,
                    ))
                })? {
                    let (id, note, tags, favorite) = row?;
                    notes.insert(
                        id,
                        json!({"note": note, "tags": tags, "favorite": favorite}),
                    );
                }
                let mut audit = Vec::new();
                let mut stmt = self
                    .db
                    .prepare("SELECT id,created,event_id,action,detail FROM audit")?;
                for row in stmt.query_map([], |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, Option<String>>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, String>(4)?,
                    ))
                })? {
                    let (id, created, event_id, action, detail) = row?;
                    audit.push(json!({"id": id, "created": created, "event_id": event_id, "action": action, "detail": detail}));
                }
                serde_json::to_writer_pretty(
                    &mut file,
                    &json!({"schema": 3, "exported": now(), "matches": matches, "events": events, "notes": notes, "audit": audit}),
                )?;
                file.write_all(b"\n")?;
            }
            file.sync_all()?;
            drop(file);
            fs::rename(&temp, path)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temp);
        }
        result
    }

    pub fn restore(&mut self, path: &Path) -> Result<PathBuf> {
        if self.is_ledger_file(path) {
            return invalid("Choose a backup file");
        }
        let source = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        source.execute_batch("PRAGMA trusted_schema=OFF;")?;
        let integrity: String = source.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
        if integrity != "ok" {
            return invalid("Backup failed its integrity check");
        }
        let version: i64 = source.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if !(1..=3).contains(&version) {
            return invalid("Unsupported backup version");
        }
        let reference = Connection::open_in_memory()?;
        reference.execute_batch(SCHEMA_V1)?;
        if version >= 2 {
            reference.execute_batch(SCHEMA_V2)?;
        }
        if version >= 3 {
            reference.execute_batch(SCHEMA_V3)?;
        }
        if schema(&source)? != schema(&reference)? {
            return invalid("Backup contains an unexpected schema");
        }
        if source.prepare("PRAGMA foreign_key_check")?.exists([])? {
            return invalid("Backup contains invalid match references");
        }
        let safety = self
            .path
            .with_file_name(format!("before-restore-{}.sqlite", Uuid::new_v4()));
        self.backup(&safety)?;
        Backup::new(&source, &mut self.db)?.run_to_completion(
            128,
            StdDuration::from_millis(5),
            None,
        )?;
        let tx = self.db.transaction()?;
        tx.execute_batch(&format!(
            "{SCHEMA_V1}{SCHEMA_V2}{SCHEMA_V3}PRAGMA user_version=3;"
        ))?;
        tx.commit()?;
        Ok(safety)
    }

    pub fn clear(&mut self) -> Result<PathBuf> {
        let safety = self
            .path
            .parent()
            .unwrap_or(Path::new("."))
            .join("backups")
            .join(format!("before-clear-{}.sqlite", Uuid::new_v4()));
        self.backup(&safety)?;
        let tx = self.db.transaction()?;
        tx.execute_batch("DELETE FROM events; DELETE FROM match_notes; DELETE FROM match_meta; DELETE FROM audit; UPDATE replay_files SET match_id=NULL; DELETE FROM matches;")?;
        tx.commit()?;
        Ok(safety)
    }
}

fn schema(db: &Connection) -> Result<Vec<(String, String, String, String)>> {
    Ok(db.prepare("SELECT type,name,tbl_name,sql FROM sqlite_master WHERE sql IS NOT NULL ORDER BY type,name")?.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get::<_, String>(3)?.split_whitespace().collect::<Vec<_>>().join(" "))))?.collect::<rusqlite::Result<Vec<_>>>()?)
}
fn row_event(r: &rusqlite::Row<'_>) -> rusqlite::Result<Event> {
    Ok(Event {
        id: r.get(0)?,
        match_id: r.get(1)?,
        created: r.get(2)?,
        video_time: r.get(3)?,
        status: r.get(4)?,
        chest_type: r.get(5)?,
        amount: r.get(6)?,
        source: r.get(7)?,
        confidence: r.get(8)?,
        reason: r.get(9)?,
        evidence: r.get(10)?,
    })
}
fn same_file(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    let resolve = |path: &Path| {
        fs::canonicalize(path).ok().or_else(|| {
            let file = path.file_name()?;
            let parent = path
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new("."));
            fs::canonicalize(parent)
                .ok()
                .map(|parent| parent.join(file))
        })
    };
    match (resolve(a), resolve(b)) {
        (Some(a), Some(b)) => {
            #[cfg(windows)]
            {
                a.as_os_str()
                    .to_string_lossy()
                    .eq_ignore_ascii_case(&b.as_os_str().to_string_lossy())
            }
            #[cfg(not(windows))]
            {
                a == b
            }
        }
        _ => false,
    }
}
fn file_timestamp(mtime: f64) -> Result<DateTime<Utc>> {
    // Python rounds the fractional seconds separately; rounding the full epoch loses precision.
    DateTime::from_timestamp(mtime.trunc() as i64, 0)
        .and_then(|time| {
            time.checked_add_signed(Duration::microseconds(
                (mtime.fract() * 1_000_000.0).round_ties_even() as i64,
            ))
        })
        .ok_or_else(|| Error::Invalid("File timestamp is out of range".into()))
}

fn parse_start(text: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(text)
        .ok()
        .map(|d| d.to_utc())
        .or_else(|| {
            let naive = NaiveDateTime::parse_from_str(text, "%Y-%m-%dT%H:%M:%S%.f").ok()?;
            Local
                .from_local_datetime(&naive)
                .earliest()
                .map(|d| d.to_utc())
        })
}
fn iso(time: DateTime<Utc>) -> String {
    time.to_rfc3339_opts(
        if time.timestamp_subsec_micros() == 0 {
            chrono::SecondsFormat::Secs
        } else {
            chrono::SecondsFormat::Micros
        },
        false,
    )
}

/// A stored ISO 8601 timestamp in local time. A naive one is local time, as Python's astimezone() reads it.
pub fn local_time(text: &str) -> Result<DateTime<Local>> {
    if let Ok(t) = DateTime::parse_from_rfc3339(text) {
        return Ok(t.with_timezone(&Local));
    }
    NaiveDateTime::parse_from_str(text, "%Y-%m-%dT%H:%M:%S%.f")
        .ok()
        .and_then(|t| Local.from_local_datetime(&t).earliest())
        .ok_or_else(|| Error::Invalid(format!("Unreadable timestamp: {text}")))
}

/// Write rows as CSV (by file extension) or JSON, atomically. Formula-like text is neutralized in CSV.
pub fn export_rows(
    path: &Path,
    rows: &[serde_json::Map<String, Value>],
    fields: &[&str],
) -> Result<()> {
    let csv = path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("csv"));
    let temp = path.with_extension(format!("{}.tmp", Uuid::new_v4()));
    let result = (|| -> Result<()> {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        if csv {
            file.write_all(&[0xef, 0xbb, 0xbf])?;
            let mut writer = csv::Writer::from_writer(&mut file);
            writer.write_record(fields)?;
            for row in rows {
                writer.write_record(fields.iter().map(|key| {
                    match row.get(*key).unwrap_or(&Value::Null) {
                        Value::Null => String::new(),
                        Value::String(s) if s.starts_with(['=', '+', '-', '@']) => format!("'{s}"),
                        Value::String(s) => s.clone(),
                        Value::Bool(b) => {
                            if *b {
                                "True".into()
                            } else {
                                "False".into()
                            }
                        }
                        other => other.to_string(),
                    }
                }))?;
            }
            writer.flush()?;
        } else {
            let payload = json!({"schema": 3, "exported": now(), "rows": rows});
            file.write_all(serde_json::to_string_pretty(&payload)?.as_bytes())?;
        }
        file.sync_all()?;
        Ok(())
    })();
    match result {
        Ok(()) => {
            fs::rename(&temp, path)?;
            Ok(())
        }
        Err(e) => {
            let _ = fs::remove_file(&temp);
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> Replay {
        serde_json::from_str(include_str!("../tests/fixtures/replay-squad.json")).unwrap()
    }
    #[test]
    fn reimport_keeps_corrections_notes_and_identity() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Store::open(dir.path().join("ledger.sqlite")).unwrap();
        let r = fixture();
        let id = s.import_replay(&r, "x.replay", 100, 1.0).unwrap().unwrap();
        let n = s.totals(true).unwrap().chests;
        let event = s.entries(0, 1).unwrap().remove(0);
        s.edit(&event.id, None, Some("Rare chest"), Some(5))
            .unwrap();
        s.set_note(&id, " ranked ", "duo, duo, weekend", true)
            .unwrap();
        s.import_replay(&r, "x.replay", 200, 2.0).unwrap();
        assert_eq!(s.totals(true).unwrap().chests, n + 4);
        let matches = s.history(true, 0, 20).unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].tags, "duo, weekend");
        assert!(matches[0].favorite);
        assert!(s.undo().unwrap());
        let total = s.totals(true).unwrap().chests;
        s.import_replay(&r, "x.replay", 300, 3.0).unwrap();
        assert_eq!(s.totals(true).unwrap().chests, total);
    }
    #[test]
    fn backup_clear_restore_and_invalid_restore_are_safe() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Store::open(dir.path().join("ledger.sqlite")).unwrap();
        s.import_replay(&fixture(), "x.replay", 1, 1.0).unwrap();
        let n = s.totals(true).unwrap().chests;
        let safety = s.clear().unwrap();
        assert_eq!(s.totals(true).unwrap().chests, 0);
        s.restore(&safety).unwrap();
        assert_eq!(s.totals(true).unwrap().chests, n);
        let evil = dir.path().join("evil.sqlite");
        s.backup(&evil).unwrap();
        Connection::open(&evil)
            .unwrap()
            .execute_batch("CREATE TABLE unrelated(x);")
            .unwrap();
        assert!(s.restore(&evil).is_err());
        assert_eq!(s.totals(true).unwrap().chests, n);
        assert!(s.backup(&s.path).is_err());
    }

    #[test]
    fn exports_preserve_schema_and_neutralize_spreadsheet_formulas() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path().join("ledger.sqlite")).unwrap();
        store.import_replay(&fixture(), "x.replay", 1, 1.0).unwrap();
        let event = store.entries(0, 1).unwrap().remove(0);
        store
            .edit(&event.id, None, Some("=unsafe()"), Some(4))
            .unwrap();
        let json_path = dir.path().join("data.json");
        store.export(&json_path).unwrap();
        let data: Value = serde_json::from_slice(&fs::read(&json_path).unwrap()).unwrap();
        assert_eq!(data["schema"], 3);
        assert_eq!(data["events"][0]["chest_type"], "=unsafe()");
        let csv_path = dir.path().join("data.csv");
        store.export(&csv_path).unwrap();
        let bytes = fs::read(&csv_path).unwrap();
        assert!(String::from_utf8_lossy(&bytes).contains("'=unsafe()"));
        assert!(store.export(&store.path).is_err());
        store.export(&json_path).unwrap(); // atomic replacement works on Windows too
    }

    #[test]
    fn fallback_timestamps_round_to_microseconds_like_python() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path().join("ledger.sqlite")).unwrap();
        let replay: Replay =
            serde_json::from_str(include_str!("../tests/fixtures/replay-fallback.json")).unwrap();
        store
            .import_replay(&replay, "fallback.replay", 1, 1.0)
            .unwrap();
        let row = store.history(true, 0, 1).unwrap().remove(0);
        assert_eq!(row.started, "1969-12-31T23:59:59.999000+00:00");
        assert_eq!(row.ended.as_deref(), Some("1970-01-01T00:00:01+00:00"));
        assert_eq!(
            file_timestamp(1_700_000_000.0000014)
                .unwrap()
                .timestamp_subsec_micros(),
            1
        );
    }

    #[test]
    fn recovery_files_and_empty_ledger_paths_cannot_be_overwritten() {
        assert!(Store::open(Path::new("")).is_err());
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path().join("ledger.sqlite")).unwrap();
        store.import_replay(&fixture(), "x.replay", 1, 1.0).unwrap();
        let total = store.totals(true).unwrap().chests;
        for suffix in ["-wal", "-shm", "-journal"] {
            let path = dir.path().join(".").join(format!("ledger.sqlite{suffix}"));
            assert!(store.backup(&path).is_err());
            assert!(store.export(&path).is_err());
            assert!(store.restore(&path).is_err());
        }
        assert_eq!(store.totals(true).unwrap().chests, total);
    }

    #[test]
    fn an_empty_owner_object_is_skipped_like_python() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path().join("ledger.sqlite")).unwrap();
        let mut replay = fixture();
        replay.owner = Some(crate::model::Player::default());
        assert!(
            store
                .import_replay(&replay, "unknown-owner.replay", 1, 1.0)
                .unwrap()
                .is_none()
        );
        assert_eq!(store.totals(true).unwrap().matches, 0);
    }

    #[test]
    fn old_schema_is_backed_up_before_upgrade_and_future_schema_is_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ledger.sqlite");
        let db = Connection::open(&path).unwrap();
        db.execute_batch(&format!("{SCHEMA_V1} PRAGMA user_version=1;"))
            .unwrap();
        db.execute("INSERT INTO matches VALUES('legacy','2026-01-01T00:00:00+00:00','2026-01-01T00:01:00+00:00',60)", []).unwrap();
        drop(db);
        let store = Store::open(&path).unwrap();
        assert_eq!(store.totals(true).unwrap().matches, 1);
        let copies: Vec<_> = fs::read_dir(dir.path().join("backups"))
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        assert_eq!(copies.len(), 1);
        let backup = Connection::open(&copies[0]).unwrap();
        assert_eq!(
            backup
                .query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
        drop(store);
        let db = Connection::open(&path).unwrap();
        db.execute_batch("PRAGMA user_version=99;").unwrap();
        drop(db);
        assert!(Store::open(&path).is_err());
        let db = Connection::open(&path).unwrap();
        assert_eq!(
            db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            99
        );
    }
}
