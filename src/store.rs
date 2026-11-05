use crate::{
    broker::Broker,
    config::Config,
    error::{Error, Result},
    model::{Envelope, Message, now},
    sqlite::WorkerSender,
};
use rusqlite::{Connection, OptionalExtension, params};
use std::{
    collections::HashSet,
    sync::{Arc, mpsc},
    thread,
};
use tokio::sync::oneshot;

type Reply<T> = oneshot::Sender<Result<T>>;
type Job = Box<dyn FnOnce(&mut Database) + Send>;
enum Command {
    Publish {
        message: Box<Message>,
        cache: bool,
        dedup: Option<String>,
        reply: Reply<Arc<Envelope>>,
    },
    Job(Job),
}
#[derive(Clone)]
pub struct Store {
    tx: WorkerSender<Command>,
}
struct Database {
    connection: Connection,
    seq: i64,
    count: usize,
    config: Arc<Config>,
    broker: Arc<Broker>,
}

#[derive(Clone)]
pub enum Since {
    None,
    All,
    Latest,
    Time(i64),
    Id(String),
    Sequence(i64),
}
pub struct Snapshot {
    pub messages: Vec<Arc<Envelope>>,
    pub watermark: i64,
    pub truncated: bool,
}

impl Store {
    pub fn open(config: Arc<Config>, broker: Arc<Broker>) -> Result<Self> {
        if let Some(parent) = config
            .cache_file
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)?;
        }
        let mut connection = if config.cache_file.as_os_str().is_empty() {
            Connection::open_in_memory()?
        } else {
            Connection::open(&config.cache_file)?
        };
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        crate::cache_migration::initialize(&mut connection, &config)?;
        connection.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA cache_size=-2048; PRAGMA journal_size_limit=16777216;")?;
        let seq = connection.query_row("SELECT COALESCE(MAX(seq),0) FROM messages", [], |r| {
            r.get(0)
        })?;
        let count = connection.query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))?;
        let mut db = Database {
            connection,
            seq,
            count,
            config: config.clone(),
            broker,
        };
        let (tx, rx) = mpsc::sync_channel(config.database_queue_size);
        let thread = thread::Builder::new()
            .name("ntfy-sqlite".into())
            .spawn(move || {
                let mut pending = None;
                while let Some(command) = pending.take().or_else(|| rx.recv().ok()) {
                    match command {
                        Command::Job(job) => job(&mut db),
                        publish @ Command::Publish { .. } => {
                            // Group concurrent writes into one durable transaction; no timer or artificial latency.
                            let mut batch = vec![publish];
                            while batch.len() < 64 {
                                match rx.try_recv() {
                                    Ok(c @ Command::Publish { .. }) => batch.push(c),
                                    Ok(c) => {
                                        pending = Some(c);
                                        break;
                                    }
                                    Err(_) => break,
                                }
                            }
                            db.publish_batch(batch);
                        }
                    }
                }
                let _ = db
                    .connection
                    .execute_batch("PRAGMA wal_checkpoint(TRUNCATE)");
            })?;
        Ok(Self {
            tx: WorkerSender::new(tx, thread),
        })
    }
    fn send(&self, command: Command) -> Result<()> {
        self.tx.try_send(command).map_err(|_| Error::busy())
    }
    async fn run<T: Send + 'static>(
        &self,
        job: impl FnOnce(&mut Database) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let (reply, rx) = oneshot::channel();
        self.send(Command::Job(Box::new(move |db| {
            let _ = reply.send(job(db));
        })))?;
        rx.await.map_err(Error::internal)?
    }
    pub async fn publish(
        &self,
        message: Message,
        cache: bool,
        dedup: Option<String>,
    ) -> Result<Arc<Envelope>> {
        let (reply, rx) = oneshot::channel();
        self.send(Command::Publish {
            message: Box::new(message),
            cache,
            dedup,
            reply,
        })?;
        rx.await.map_err(Error::internal)?
    }
    pub async fn snapshot(
        &self,
        topics: Vec<String>,
        since: Since,
        scheduled: bool,
    ) -> Result<Snapshot> {
        self.snapshot_until(topics, since, scheduled, None).await
    }
    pub async fn snapshot_until(
        &self,
        topics: Vec<String>,
        since: Since,
        scheduled: bool,
        until: Option<i64>,
    ) -> Result<Snapshot> {
        self.run(move |db| {
            let mut result = Snapshot { messages: Vec::new(), watermark: until.unwrap_or(db.seq), truncated: false };
            if matches!(since, Since::None) { return Ok(result); }
            let mut bytes = 0;
            let (after_seq, after_time) = match &since {
                Since::Id(id) => {
                    // Restrict ID lookup to authorized topics; IDs of private topics reveal nothing.
                    let row: Option<(i64, String)> = db.connection.query_row("SELECT seq,topic FROM messages WHERE id=?1", [id], |r| Ok((r.get(0)?,r.get(1)?))).optional()?;
                    (row.filter(|(_, t)| topics.contains(t)).map(|(s, _)| s).unwrap_or(0), 0)
                },
                Since::Time(t) => (0, *t),
                Since::Sequence(seq) => (*seq, 0),
                _ => (0, 0),
            };
            // One globally ordered query is essential: truncating per-topic lists can skip
            // messages from another topic when clients resume from the last returned ID.
            let placeholders = std::iter::repeat_n("?", topics.len()).collect::<Vec<_>>().join(",");
            let condition = format!("topic IN ({placeholders}) AND seq>? AND seq<=? AND time>=? AND expires>? AND (scheduled=0 OR ?)");
            let sql = if matches!(since, Since::Latest) {
                format!("SELECT seq,data FROM messages WHERE seq IN (SELECT MAX(seq) FROM messages WHERE {condition} GROUP BY topic) ORDER BY seq LIMIT ?")
            } else { format!("SELECT seq,data FROM messages WHERE {condition} ORDER BY seq LIMIT ?") };
            let mut values: Vec<rusqlite::types::Value> = topics.into_iter().map(Into::into).collect();
            values.extend([after_seq.into(), result.watermark.into(), after_time.into(), now().into(), (scheduled as i64).into(), ((db.config.replay_limit+1) as i64).into()]);
            let mut stmt = db.connection.prepare(&sql)?;
            let mut rows = stmt.query(rusqlite::params_from_iter(values))?;
            while let Some(row) = rows.next()? {
                let seq = row.get(0)?;
                let json: String = row.get(1)?;
                if result.messages.len() >= db.config.replay_limit || bytes + json.len() > db.config.replay_bytes_limit { result.truncated = true; break; }
                bytes += json.len();
                result.messages.push(Arc::new(Envelope::new(seq, true, serde_json::from_str(&json)?)));
            }
            Ok(result)
        }).await
    }
    pub async fn maintain(&self) -> Result<()> {
        self.run(|db| {
            let time = now();
            let tx = db.connection.transaction()?;
            tx.execute("DELETE FROM messages WHERE expires<=?1", [time])?;
            tx.execute("DELETE FROM dedup WHERE expires<=?1", [time])?;
            let mut due = Vec::new();
            {
                let mut stmt = tx.prepare("SELECT seq,data FROM messages WHERE scheduled=1 AND time<=?1 ORDER BY time,seq LIMIT 256")?;
                for row in stmt.query_map([time], |r| Ok((r.get::<_, i64>(0)?,r.get::<_, String>(1)?)))? {
                    let (seq, json) = row?;
                    let message: Message = serde_json::from_str(&json)?;
                    // Give delayed messages a new publication sequence for reconnect cursors.
                    db.seq += 1;
                    tx.execute("UPDATE messages SET scheduled=0,seq=?1 WHERE seq=?2", params![db.seq, seq])?;
                    due.push(Arc::new(Envelope::new(db.seq, true, message)));
                }
            }
            tx.commit()?;
            db.count = db.connection.query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))?;
            for envelope in due { db.broker.publish(envelope); }
            Ok(())
        }).await
    }
    pub async fn attachment(&self, id: String) -> Result<Option<Message>> {
        self.run(move |db| {
            let data: Option<String> = db
                .connection
                .query_row(
                    "SELECT data FROM messages WHERE id=?1 AND expires>?2",
                    params![id, now()],
                    |r| r.get(0),
                )
                .optional()?;
            data.map(|data| serde_json::from_str(&data).map_err(Error::from))
                .transpose()
        })
        .await
    }
    pub async fn attachment_ids(&self) -> Result<HashSet<String>> {
        self.run(|db| {
            let mut stmt = db.connection.prepare("SELECT id FROM messages WHERE expires>?1 AND json_extract(data,'$.attachment.expires')>?1 AND COALESCE(json_extract(data,'$.attachment.deleted'),0)=0")?;
            let rows = stmt.query_map([now()], |r| r.get::<_, String>(0))?;
            Ok(rows.collect::<std::result::Result<HashSet<_>, _>>()?)
        }).await
    }
    pub async fn expire_topic(&self, topic: String) -> Result<()> {
        self.run(move |db| {
            db.connection.execute(
                "UPDATE messages SET expires=?1 WHERE topic=?2",
                params![now(), topic],
            )?;
            Ok(())
        })
        .await
    }
    pub async fn health(&self) -> Result<()> {
        self.run(|db| {
            db.connection.query_row("SELECT 1", [], |_| Ok(()))?;
            Ok(())
        })
        .await
    }
}

impl Database {
    fn publish_batch(&mut self, batch: Vec<Command>) {
        let old_seq = self.seq;
        let old_count = self.count;
        let mut results = Vec::new();
        let result = (|| -> Result<()> {
            let tx = self.connection.transaction()?;
            let mut evicted = false;
            for command in &batch {
                let Command::Publish {
                    message,
                    cache,
                    dedup,
                    ..
                } = command
                else {
                    unreachable!()
                };
                if let Some(key) = dedup {
                    let data: Option<(i64, String)> = tx.query_row("SELECT m.seq,m.data FROM dedup d JOIN messages m ON m.id=d.message_id WHERE d.key=?1 AND d.expires>?2", params![key, now()], |r| Ok((r.get(0)?,r.get(1)?))).optional()?;
                    if let Some((seq, data)) = data {
                        results.push((
                            Arc::new(Envelope::new(seq, true, serde_json::from_str(&data)?)),
                            false,
                        ));
                        continue;
                    }
                }
                self.seq += 1;
                let scheduled = message.time > now();
                let envelope = Arc::new(Envelope::new(self.seq, *cache, *message.clone()));
                if *cache {
                    if !message.sequence_id.is_empty()
                        && matches!(message.event.as_str(), "message" | "message_delete")
                    {
                        let removed = tx.execute("DELETE FROM messages WHERE topic=?1 AND scheduled=1 AND (json_extract(data,'$.sequence_id')=?2 OR id=?2)", params![message.topic,message.sequence_id])?;
                        self.count -= removed;
                        evicted |= removed > 0;
                    }
                    if self.count >= self.config.cache_max_messages {
                        let deleted = tx.execute("DELETE FROM messages WHERE seq IN (SELECT seq FROM messages WHERE scheduled=0 ORDER BY seq LIMIT ?1)", [self.config.cache_max_messages.clamp(1, 256) as i64])?;
                        self.count -= deleted;
                        evicted |= deleted > 0;
                        if self.count >= self.config.cache_max_messages {
                            return Err(Error::busy());
                        }
                    }
                    tx.execute("INSERT INTO messages(seq,id,topic,time,expires,scheduled,data) VALUES (?1,?2,?3,?4,?5,?6,?7)", params![envelope.seq, message.id, message.topic, message.time, message.expires, scheduled, std::str::from_utf8(&envelope.json).unwrap()])?;
                    self.count += 1;
                    if let Some(key) = dedup {
                        tx.execute("INSERT OR REPLACE INTO dedup(key,message_id,expires) VALUES (?1,?2,?3)", params![key, message.id, message.expires])?;
                    }
                }
                results.push((envelope, !scheduled));
            }
            // Eviction must also evict dedup entries, so the auxiliary table stays bounded.
            if evicted {
                tx.execute(
                    "DELETE FROM dedup WHERE message_id NOT IN (SELECT id FROM messages)",
                    [],
                )?;
            }
            tx.commit()?;
            Ok(())
        })();
        if let Err(error) = result {
            self.seq = old_seq;
            self.count = old_count;
            for command in batch {
                if let Command::Publish { reply, .. } = command {
                    let _ = reply.send(Err(error.clone()));
                }
            }
        } else {
            for (command, (envelope, live)) in batch.into_iter().zip(results) {
                if live {
                    self.broker.publish(envelope.clone());
                }
                if let Command::Publish { reply, .. } = command {
                    let _ = reply.send(Ok(envelope));
                }
            }
        }
    }
}
