use crate::{
    config::Config,
    error::{Error, Result},
    model::{Attachment, Message},
};
use rusqlite::{Connection, params};
use std::collections::HashSet;

pub(crate) const TABLES: &str = "CREATE TABLE IF NOT EXISTS messages(seq INTEGER PRIMARY KEY, id TEXT NOT NULL UNIQUE, topic TEXT NOT NULL, time INTEGER NOT NULL, expires INTEGER NOT NULL, scheduled INTEGER NOT NULL, data TEXT NOT NULL);
CREATE INDEX IF NOT EXISTS messages_topic_seq ON messages(topic,seq);
CREATE INDEX IF NOT EXISTS messages_expiry ON messages(expires);
CREATE INDEX IF NOT EXISTS messages_due ON messages(scheduled,time);
CREATE TABLE IF NOT EXISTS dedup(key TEXT PRIMARY KEY, message_id TEXT NOT NULL, expires INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS dedup_expiry ON dedup(expires);";

// The Rust cache uses an ordered cursor and a compact serialized message. Import
// one row at a time, in a transaction, after a WAL-aware backup of the original.
pub(crate) fn initialize(db: &mut Connection, config: &Config) -> Result<()> {
    let app_id: i64 = db.query_row("PRAGMA application_id", [], |r| r.get(0))?;
    let tables: i64 = db.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table'",
        [],
        |r| r.get(0),
    )?;
    if tables == 0 || app_id == 0x4e545253 {
        db.execute_batch(TABLES)?;
        db.execute_batch("PRAGMA application_id=1314148947; PRAGMA user_version=1;")?;
        return Ok(());
    }
    let standard: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='schemaVersion') AND EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='messages')", [], |r|r.get(0))?;
    if !standard || app_id != 0 {
        return Err(Error::bad("cache-file is not an ntfy message database"));
    }
    let version: i64 = db.query_row("SELECT version FROM schemaVersion WHERE id=1", [], |r| {
        r.get(0)
    })?;
    if !(1..=17).contains(&version) {
        return Err(Error::bad(
            "unsupported Go ntfy cache schema version (supported: 1-17)",
        ));
    }
    let columns: HashSet<String> = db
        .prepare("PRAGMA table_info(messages)")?
        .query_map([], |r| r.get(1))?
        .collect::<rusqlite::Result<_>>()?;
    if ![
        "id", "time", "topic", "message", "title", "priority", "tags",
    ]
    .iter()
    .all(|c| columns.contains(*c))
    {
        return Err(Error::bad(
            "cache-file has an incomplete ntfy message schema",
        ));
    }
    crate::sqlite_backup::backup(db, &config.cache_file, &format!("go-v{version}"))?;
    let tx = db.transaction()?;
    tx.execute_batch("ALTER TABLE messages RENAME TO go_messages;")?;
    tx.execute_batch(TABLES)?;
    let field = |name: &str, default: &str| {
        if columns.contains(name) {
            format!("\"{name}\"")
        } else {
            default.to_string()
        }
    };
    let retention = Config::duration(&config.cache_duration).as_secs();
    let selected = [
        field("mid", "CAST(id AS TEXT)"),
        field("time", "0"),
        field("topic", "''"),
        field("message", "''"),
        field("title", "''"),
        field("priority", "0"),
        field("tags", "''"),
        field("event", "'message'"),
        field("expires", &format!("time+{retention}")),
        field("sequence_id", "''"),
        field("click", "''"),
        field("icon", "''"),
        field("actions", "''"),
        field("content_type", "''"),
        field("encoding", "''"),
        field("attachment_name", "''"),
        field("attachment_type", "''"),
        field("attachment_size", "0"),
        field("attachment_expires", "0"),
        field("attachment_url", "''"),
        field("published", "1"),
        field("attachment_deleted", "0"),
    ]
    .join(",");
    {
        let mut query = tx.prepare(&format!(
            "SELECT {selected} FROM go_messages ORDER BY {}",
            if columns.contains("mid") {
                "id"
            } else {
                "time,id"
            }
        ))?;
        let mut rows = query.query([])?;
        let mut seq = 0_i64;
        while let Some(row) = rows.next()? {
            seq += 1;
            let tags: String = row.get(6)?;
            let actions: String = row.get(12)?;
            let mut message = Message {
                id: row.get(0)?,
                time: row.get(1)?,
                topic: row.get(2)?,
                message: row.get(3)?,
                title: row.get(4)?,
                priority: row.get(5)?,
                tags: if tags.is_empty() {
                    vec![]
                } else {
                    tags.split(',').map(str::to_string).collect()
                },
                event: row.get(7)?,
                expires: row.get(8)?,
                sequence_id: row.get(9)?,
                click: row.get(10)?,
                icon: row.get(11)?,
                actions: if actions.is_empty() {
                    vec![]
                } else {
                    serde_json::from_str(&actions)?
                },
                content_type: row.get(13)?,
                encoding: row.get(14)?,
                ..Message::default()
            };
            let url: String = row.get(19)?;
            if !url.is_empty() {
                message.attachment = Some(Attachment {
                    deleted: row.get(21)?,
                    name: row.get(15)?,
                    content_type: row.get(16)?,
                    size: row.get(17)?,
                    expires: row.get(18)?,
                    url,
                });
            }
            let published: bool = row.get(20)?;
            tx.execute("INSERT INTO messages(seq,id,topic,time,expires,scheduled,data) VALUES(?1,?2,?3,?4,?5,?6,?7)",params![seq,message.id,message.topic,message.time,message.expires,!published,serde_json::to_string(&message)?])?;
        }
    }
    tx.execute_batch("DROP TABLE go_messages; DROP TABLE schemaVersion; PRAGMA application_id=1314148947; PRAGMA user_version=1;")?;
    tx.commit()?;
    tracing::info!(
        version,
        "imported Go ntfy message cache; original database preserved in pre-rust backup"
    );
    Ok(())
}
