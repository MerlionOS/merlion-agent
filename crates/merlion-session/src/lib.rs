//! SQLite-backed session store. Modeled on hermes's `SessionDB` in
//! `hermes_state.py` — sessions table + messages table + FTS5 search.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use merlion_core::Message;
use rusqlite::{params, Connection};

pub struct SessionDB {
    conn: Connection,
}

#[derive(Debug, Clone)]
pub struct SessionRow {
    pub id: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub title: Option<String>,
    pub message_count: i64,
}

impl SessionDB {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }
        let conn = Connection::open(path).with_context(|| format!("open {}", path.display()))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        let db = Self { conn };
        db.migrate()?;
        Ok(db)
    }

    pub fn open_default() -> Result<Self> {
        let home = std::env::var("MERLION_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                dirs::home_dir()
                    .map(|h| h.join(".merlion"))
                    .unwrap_or_else(|| PathBuf::from(".merlion"))
            });
        Self::open(home.join("sessions.db"))
    }

    fn migrate(&self) -> Result<()> {
        self.conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS sessions (
                id          TEXT PRIMARY KEY,
                created_at  TEXT NOT NULL,
                updated_at  TEXT NOT NULL,
                title       TEXT
            );
            CREATE TABLE IF NOT EXISTS messages (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id  TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                ord         INTEGER NOT NULL,
                role        TEXT NOT NULL,
                payload     TEXT NOT NULL,
                created_at  TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_messages_session
                ON messages(session_id, ord);
            CREATE VIRTUAL TABLE IF NOT EXISTS messages_fts USING fts5(
                content,
                session_id UNINDEXED,
                content='',
                tokenize='porter unicode61'
            );
            "#,
        )?;
        Ok(())
    }

    pub fn create_session(&self, id: &str, title: Option<&str>) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "INSERT INTO sessions (id, created_at, updated_at, title) VALUES (?, ?, ?, ?)",
            params![id, now, now, title],
        )?;
        Ok(())
    }

    pub fn touch(&self, id: &str) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE sessions SET updated_at = ? WHERE id = ?",
            params![now, id],
        )?;
        Ok(())
    }

    pub fn append_message(&self, session_id: &str, msg: &Message) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let role = serde_json::to_value(msg.role)?
            .as_str()
            .unwrap_or("user")
            .to_string();
        let payload = serde_json::to_string(msg)?;
        let next_ord: i64 = self.conn.query_row(
            "SELECT COALESCE(MAX(ord), -1) + 1 FROM messages WHERE session_id = ?",
            params![session_id],
            |r| r.get(0),
        )?;
        self.conn.execute(
            "INSERT INTO messages (session_id, ord, role, payload, created_at)
             VALUES (?, ?, ?, ?, ?)",
            params![session_id, next_ord, role, payload, now],
        )?;
        if let Some(text) = msg.content.as_deref() {
            self.conn.execute(
                "INSERT INTO messages_fts (rowid, content, session_id)
                 VALUES (last_insert_rowid(), ?, ?)",
                params![text, session_id],
            )?;
        }
        self.touch(session_id)?;
        Ok(())
    }

    pub fn load_messages(&self, session_id: &str) -> Result<Vec<Message>> {
        let mut stmt = self
            .conn
            .prepare("SELECT payload FROM messages WHERE session_id = ? ORDER BY ord ASC")?;
        let rows = stmt.query_map(params![session_id], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for r in rows {
            let payload = r?;
            let m: Message = serde_json::from_str(&payload)?;
            out.push(m);
        }
        Ok(out)
    }

    pub fn list_sessions(&self, limit: usize) -> Result<Vec<SessionRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT s.id, s.created_at, s.updated_at, s.title,
                    (SELECT COUNT(*) FROM messages m WHERE m.session_id = s.id)
             FROM sessions s
             ORDER BY s.updated_at DESC
             LIMIT ?",
        )?;
        let rows = stmt.query_map(params![limit as i64], |r| {
            Ok(SessionRow {
                id: r.get(0)?,
                created_at: parse_ts(r.get::<_, String>(1)?),
                updated_at: parse_ts(r.get::<_, String>(2)?),
                title: r.get(3)?,
                message_count: r.get(4)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<(String, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT session_id, snippet(messages_fts, 0, '«', '»', '…', 24)
             FROM messages_fts WHERE messages_fts MATCH ?
             ORDER BY rank LIMIT ?",
        )?;
        let rows = stmt.query_map(params![query, limit as i64], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

fn parse_ts(s: String) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(&s)
        .map(|d| d.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now())
}
