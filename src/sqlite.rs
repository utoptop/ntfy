use crate::error::{Error, Result};
use rusqlite::Connection;
use std::sync::mpsc;
use tokio::sync::oneshot;
type Job = Box<dyn FnOnce(&mut Connection) + Send>;
#[derive(Clone)]
pub(crate) struct Sqlite {
    tx: mpsc::SyncSender<Job>,
}
impl Sqlite {
    pub fn new(mut connection: Connection, capacity: usize, name: &str) -> Result<Self> {
        let (tx, rx) = mpsc::sync_channel::<Job>(capacity);
        std::thread::Builder::new()
            .name(name.into())
            .spawn(move || {
                while let Ok(job) = rx.recv() {
                    job(&mut connection);
                }
                let _ = connection.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)");
            })?;
        Ok(Self { tx })
    }
    pub async fn run<T: Send + 'static>(
        &self,
        job: impl FnOnce(&mut Connection) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .try_send(Box::new(move |db| {
                let _ = tx.send(job(db));
            }))
            .map_err(|_| Error::busy())?;
        rx.await.map_err(Error::internal)?
    }
}
