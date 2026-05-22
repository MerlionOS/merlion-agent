//! `merlion checkpoints` — inspect and prune the SQLite-backed session
//! store at `~/.merlion/sessions.db`.
//!
//! The session DB is owned by [`merlion_session::SessionDB`], which
//! defines the schema (`sessions`, `messages`, `messages_fts`). This
//! module talks to that database for maintenance operations that the
//! `SessionDB` public API does not expose: stats, listing, age-based
//! pruning, and VACUUM. Migrations are still routed through
//! `SessionDB::open` so this command stays in lock-step with whatever
//! schema the session crate is currently on.
//!
//! See the wiring spec at the bottom of this file for how this hooks
//! into `main.rs`.

// The module is intentionally not yet dispatched from `main.rs` — the
// wiring is documented at the bottom of the file. Suppress dead-code
// lints until the caller is wired up so `cargo clippy -- -D warnings`
// stays green.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use clap::Subcommand;
use rusqlite::Connection;

#[derive(Debug, Subcommand)]
pub enum CheckpointsAction {
    /// Show sessions DB stats: file size, row counts per table,
    /// oldest + newest session timestamps.
    Stats,
    /// List sessions, newest first, with id / title / message-count /
    /// last-activity timestamp.
    List {
        /// Limit how many rows to show (default 20).
        #[arg(short, long, default_value_t = 20)]
        limit: usize,
    },
    /// Delete sessions older than the given duration (e.g. "30d", "12h").
    /// Use `--dry-run` to preview without modifying the DB.
    Prune {
        /// Age threshold, e.g. "30d", "12h", "1w".
        #[arg(long)]
        older_than: String,
        /// Show what would be deleted without committing.
        #[arg(long)]
        dry_run: bool,
    },
    /// Run SQLite VACUUM to reclaim space after prunes.
    Vacuum,
}

pub async fn run(action: CheckpointsAction) -> Result<()> {
    match action {
        CheckpointsAction::Stats => stats(),
        CheckpointsAction::List { limit } => list(limit),
        CheckpointsAction::Prune {
            older_than,
            dry_run,
        } => prune(&older_than, dry_run),
        CheckpointsAction::Vacuum => vacuum(),
    }
}

/// Resolve the sessions DB path the same way `SessionDB::open_default`
/// does: respect `MERLION_HOME`, fall back to `~/.merlion`, then append
/// `sessions.db`.
fn sessions_db_path() -> PathBuf {
    let home = std::env::var("MERLION_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            dirs::home_dir()
                .map(|h| h.join(".merlion"))
                .unwrap_or_else(|| PathBuf::from(".merlion"))
        });
    home.join("sessions.db")
}

/// Run `SessionDB::open` against the resolved path to ensure migrations
/// are applied, then drop it so the WAL file is released before we open
/// our own connection for maintenance queries.
fn ensure_schema(path: &Path) -> Result<()> {
    let _db = merlion_session::SessionDB::open(path)
        .with_context(|| format!("open session DB at {}", path.display()))?;
    Ok(())
}

fn open_conn(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path)
        .with_context(|| format!("open SQLite connection to {}", path.display()))?;
    conn.pragma_update(None, "foreign_keys", "ON")
        .context("enable foreign_keys pragma")?;
    Ok(conn)
}

fn stats() -> Result<()> {
    let path = sessions_db_path();
    ensure_schema(&path)?;
    let conn = open_conn(&path)?;

    let size = std::fs::metadata(&path)
        .map(|m| m.len())
        .with_context(|| format!("stat {}", path.display()))?;

    let session_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))
        .context("count sessions")?;
    let message_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))
        .context("count messages")?;

    let oldest: Option<String> = conn
        .query_row("SELECT MIN(created_at) FROM sessions", [], |r| {
            r.get::<_, Option<String>>(0)
        })
        .context("query oldest session timestamp")?;
    let newest: Option<String> = conn
        .query_row("SELECT MAX(updated_at) FROM sessions", [], |r| {
            r.get::<_, Option<String>>(0)
        })
        .context("query newest session timestamp")?;

    let fts_count: Option<i64> = if table_exists(&conn, "messages_fts")? {
        Some(
            conn.query_row("SELECT COUNT(*) FROM messages_fts", [], |r| r.get(0))
                .context("count FTS rows")?,
        )
    } else {
        None
    };

    println!("Sessions DB: {}", path.display());
    println!("  size:        {}", human_size(size));
    println!("  sessions:    {session_count}");
    println!("  messages:    {message_count}");
    if let Some(c) = fts_count {
        println!("  fts rows:    {c}");
    } else {
        println!("  fts rows:    (table missing)");
    }
    println!("  oldest:      {}", oldest.as_deref().unwrap_or("(none)"));
    println!("  newest:      {}", newest.as_deref().unwrap_or("(none)"));
    Ok(())
}

fn list(limit: usize) -> Result<()> {
    let path = sessions_db_path();
    ensure_schema(&path)?;
    let conn = open_conn(&path)?;

    let mut stmt = conn
        .prepare(
            "SELECT s.id,
                    s.title,
                    (SELECT COUNT(*) FROM messages m WHERE m.session_id = s.id) AS msgs,
                    s.updated_at,
                    (SELECT payload FROM messages m
                     WHERE m.session_id = s.id AND m.role = 'user'
                     ORDER BY m.ord ASC LIMIT 1) AS first_user
             FROM sessions s
             ORDER BY s.updated_at DESC
             LIMIT ?",
        )
        .context("prepare session list query")?;

    let rows = stmt
        .query_map([limit as i64], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, Option<String>>(4)?,
            ))
        })
        .context("query sessions")?;

    let mut entries: Vec<(String, String, i64, String)> = Vec::new();
    for row in rows {
        let (id, title, msgs, updated_at, first_user) = row?;
        let title_disp = title
            .filter(|t| !t.is_empty())
            .or_else(|| first_user.and_then(extract_first_user_text))
            .unwrap_or_else(|| "(untitled)".to_string());
        let title_disp = truncate(&title_disp, 32);
        let ts = format_ts_short(&updated_at);
        entries.push((id, title_disp, msgs, ts));
    }

    if entries.is_empty() {
        println!("(no sessions)");
        return Ok(());
    }

    let header_id = "ID";
    let header_title = "TITLE";
    let header_msgs = "MSGS";
    let header_last = "LAST ACTIVITY";
    println!("{header_id:<14}  {header_title:<32}  {header_msgs:>5}  {header_last}");
    for (id, title, msgs, ts) in entries {
        let id_short = truncate(&id, 12);
        let id_disp = format!("{id_short}…");
        println!("{id_disp:<14}  {title:<32}  {msgs:>5}  {ts}");
    }
    Ok(())
}

fn prune(older_than: &str, dry_run: bool) -> Result<()> {
    let cutoff = compute_cutoff(older_than, Utc::now())?;
    let path = sessions_db_path();
    ensure_schema(&path)?;
    let mut conn = open_conn(&path)?;

    let cutoff_str = cutoff.to_rfc3339();
    let stale: Vec<String> = {
        let mut stmt = conn
            .prepare("SELECT id FROM sessions WHERE updated_at < ?")
            .context("prepare stale-session query")?;
        let rows = stmt
            .query_map([&cutoff_str], |r| r.get::<_, String>(0))
            .context("query stale sessions")?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .context("collect stale session ids")?
    };

    if stale.is_empty() {
        println!("No sessions older than {older_than} (cutoff {cutoff_str}).");
        return Ok(());
    }

    if dry_run {
        println!(
            "Would prune {} session(s) older than {older_than} (cutoff {cutoff_str}):",
            stale.len()
        );
        for id in &stale {
            println!("  {id}");
        }
        println!("(dry-run — DB not modified)");
        return Ok(());
    }

    // Foreign keys are ON and `messages.session_id` declares `ON DELETE
    // CASCADE` (see `merlion-session` migrate()), so a single DELETE on
    // `sessions` is sufficient to evict the messages too. We still drive
    // it inside a transaction so a SIGINT mid-prune leaves the store
    // consistent.
    let tx = conn.transaction().context("begin prune transaction")?;
    {
        let mut del = tx
            .prepare("DELETE FROM sessions WHERE id = ?")
            .context("prepare delete-session statement")?;
        for id in &stale {
            del.execute([id])
                .with_context(|| format!("delete session {id}"))?;
        }
    }
    // FTS rows live keyed by rowid of `messages`, which the cascade
    // removes from `messages`, but the `messages_fts` virtual table has
    // no FK link — clean it up explicitly so search doesn't return
    // dangling snippets.
    {
        let mut del_fts = tx
            .prepare("DELETE FROM messages_fts WHERE session_id = ?")
            .context("prepare delete-fts statement")?;
        for id in &stale {
            del_fts
                .execute([id])
                .with_context(|| format!("delete fts rows for session {id}"))?;
        }
    }
    tx.commit().context("commit prune transaction")?;

    println!("Pruned {} session(s).", stale.len());
    println!("Tip: run `merlion checkpoints vacuum` to reclaim disk space.");
    Ok(())
}

fn vacuum() -> Result<()> {
    let path = sessions_db_path();
    ensure_schema(&path)?;
    let before = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);

    let conn = open_conn(&path)?;
    conn.execute_batch("VACUUM").context("run VACUUM")?;
    drop(conn);

    let after = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    println!("VACUUM complete.");
    println!("  before: {}", human_size(before));
    println!("  after:  {}", human_size(after));
    if after < before {
        println!("  freed:  {}", human_size(before - after));
    }
    Ok(())
}

fn table_exists(conn: &Connection, name: &str) -> Result<bool> {
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name = ?",
            [name],
            |r| r.get(0),
        )
        .with_context(|| format!("check existence of table {name}"))?;
    Ok(n > 0)
}

/// Parse `"30d"`, `"12h"`, `"1w"`, etc. via `humantime` and subtract
/// from `now` to get the cutoff timestamp.
fn compute_cutoff(older_than: &str, now: DateTime<Utc>) -> Result<DateTime<Utc>> {
    let dur: Duration = humantime::parse_duration(older_than)
        .with_context(|| format!("parse --older-than `{older_than}`"))?;
    let secs = chrono::Duration::from_std(dur)
        .with_context(|| format!("convert duration `{older_than}` to chrono::Duration"))?;
    Ok(now - secs)
}

/// Pretty-print a byte count as B / KB / MB / GB. Uses 1024-based units
/// because that's what `du -h` and friends report.
fn human_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let b = bytes as f64;
    if b >= GB {
        format!("{:.2} GB", b / GB)
    } else if b >= MB {
        format!("{:.2} MB", b / MB)
    } else if b >= KB {
        format!("{:.1} KB", b / KB)
    } else {
        format!("{bytes} B")
    }
}

/// Trim an ISO-8601 timestamp like `2026-05-21T16:57:32.123+00:00` down
/// to `2026-05-21 16:57` for table display.
fn format_ts_short(rfc3339: &str) -> String {
    match DateTime::parse_from_rfc3339(rfc3339) {
        Ok(dt) => dt.with_timezone(&Utc).format("%Y-%m-%d %H:%M").to_string(),
        Err(_) => rfc3339.to_string(),
    }
}

fn truncate(s: &str, max: usize) -> String {
    let cleaned: String = s.chars().map(|c| if c == '\n' { ' ' } else { c }).collect();
    let n = cleaned.chars().count();
    if n <= max {
        cleaned
    } else {
        let head: String = cleaned.chars().take(max.saturating_sub(1)).collect();
        format!("{head}…")
    }
}

/// `messages.payload` is a JSON-encoded `merlion_core::Message`. Pull
/// out the `content` field as a best-effort title fallback when the
/// session has no explicit `title`.
fn extract_first_user_text(payload: String) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(&payload).ok()?;
    v.get("content")
        .and_then(|c| c.as_str())
        .map(|s| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn human_size_formats_units() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(1024), "1.0 KB");
        assert_eq!(human_size(1536), "1.5 KB");
        assert_eq!(human_size(1024 * 1024), "1.00 MB");
        assert_eq!(human_size(1024 * 1024 * 1024), "1.00 GB");
    }

    #[test]
    fn compute_cutoff_subtracts_duration() {
        let now = Utc.with_ymd_and_hms(2026, 5, 21, 12, 0, 0).unwrap();
        let cutoff = compute_cutoff("1d", now).unwrap();
        assert_eq!(cutoff, Utc.with_ymd_and_hms(2026, 5, 20, 12, 0, 0).unwrap());

        let cutoff = compute_cutoff("12h", now).unwrap();
        assert_eq!(cutoff, Utc.with_ymd_and_hms(2026, 5, 21, 0, 0, 0).unwrap());

        let cutoff = compute_cutoff("1w", now).unwrap();
        assert_eq!(cutoff, Utc.with_ymd_and_hms(2026, 5, 14, 12, 0, 0).unwrap());
    }

    #[test]
    fn compute_cutoff_rejects_garbage() {
        let now = Utc::now();
        assert!(compute_cutoff("not-a-duration", now).is_err());
        assert!(compute_cutoff("", now).is_err());
    }

    #[test]
    fn truncate_keeps_short_strings_intact() {
        assert_eq!(truncate("hello", 32), "hello");
        assert_eq!(truncate("", 32), "");
    }

    #[test]
    fn truncate_shortens_long_strings_with_ellipsis() {
        let out = truncate("abcdefghij", 5);
        assert_eq!(out.chars().count(), 5);
        assert!(out.ends_with('…'));
    }

    #[test]
    fn truncate_flattens_newlines() {
        assert_eq!(truncate("a\nb", 32), "a b");
    }

    #[test]
    fn format_ts_short_trims_to_minute() {
        assert_eq!(
            format_ts_short("2026-05-21T16:57:32+00:00"),
            "2026-05-21 16:57"
        );
    }

    #[test]
    fn format_ts_short_passes_through_garbage() {
        assert_eq!(format_ts_short("not a timestamp"), "not a timestamp");
    }

    #[test]
    fn extract_first_user_text_pulls_content_field() {
        let payload = r#"{"role":"user","content":"hello world"}"#.to_string();
        assert_eq!(
            extract_first_user_text(payload),
            Some("hello world".to_string())
        );
    }

    #[test]
    fn extract_first_user_text_returns_none_on_non_string_content() {
        let payload = r#"{"role":"user","content":[{"type":"text","text":"hi"}]}"#.to_string();
        assert_eq!(extract_first_user_text(payload), None);
    }

    #[test]
    fn prune_round_trip_against_real_db() {
        // End-to-end: create a SessionDB, drop an old + new session in,
        // run prune(), confirm only the old one is gone. We override
        // MERLION_HOME so this test never touches the user's real DB.
        let tmp = tempfile::tempdir().unwrap();
        let prev_home = std::env::var("MERLION_HOME").ok();
        std::env::set_var("MERLION_HOME", tmp.path());

        let path = sessions_db_path();
        {
            let conn = {
                let _db = merlion_session::SessionDB::open(&path).unwrap();
                Connection::open(&path).unwrap()
            };
            let old_ts = "2020-01-01T00:00:00+00:00";
            let new_ts = Utc::now().to_rfc3339();
            conn.execute(
                "INSERT INTO sessions (id, created_at, updated_at, title) VALUES (?, ?, ?, ?)",
                rusqlite::params!["old", old_ts, old_ts, "old"],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO sessions (id, created_at, updated_at, title) VALUES (?, ?, ?, ?)",
                rusqlite::params!["new", new_ts, new_ts, "new"],
            )
            .unwrap();
        }

        prune("1d", false).unwrap();

        let conn = Connection::open(&path).unwrap();
        let ids: Vec<String> = conn
            .prepare("SELECT id FROM sessions ORDER BY id")
            .unwrap()
            .query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(ids, vec!["new".to_string()]);

        match prev_home {
            Some(v) => std::env::set_var("MERLION_HOME", v),
            None => std::env::remove_var("MERLION_HOME"),
        }
    }
}

// -----------------------------------------------------------------------------
// WIRING SPEC — apply to `crates/merlion-cli/src/main.rs`.
//
// 1. Add a module declaration near the other `mod` lines at the top of main.rs:
//
//        mod checkpoints_cmd;
//
// 2. Add a new variant to the `Command` enum:
//
//        /// Inspect and prune the sessions database at
//        /// `~/.merlion/sessions.db`.
//        ///
//        /// Subcommands: `stats`, `list`, `prune --older-than <DUR>
//        /// [--dry-run]`, `vacuum`. Prune deletes whole sessions whose
//        /// last activity is older than the given humantime duration
//        /// (e.g. `30d`, `12h`, `1w`); the schema cascades so messages
//        /// and FTS rows are removed too.
//        Checkpoints {
//            #[command(subcommand)]
//            action: checkpoints_cmd::CheckpointsAction,
//        },
//
// 3. Add a dispatch arm in the `match cli.command.unwrap_or(...)` block in
//    `main()`:
//
//        Command::Checkpoints { action } => checkpoints_cmd::run(action).await,
//
// 4. No new clap derives are required in main.rs — `CheckpointsAction`
//    already derives `clap::Subcommand` in this file.
// -----------------------------------------------------------------------------
