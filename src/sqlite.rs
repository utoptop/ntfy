use crate::error::{Error, Result};
use rusqlite::Connection;
use std::{
    sync::{Arc, mpsc},
    thread::JoinHandle,
};
use tokio::sync::oneshot;
type Job = Box<dyn FnOnce(&mut Connection) + Send>;

// The final sender closes the queue and joins the SQLite worker. Closing a server
// must finish its WAL checkpoint before another instance opens the same files.
pub(crate) struct WorkerSender<T> {
    inner: Arc<Worker<T>>,
}
struct Worker<T> {
    sender: Option<mpsc::SyncSender<T>>,
    thread: Option<JoinHandle<()>>,
}
impl<T> Clone for WorkerSender<T> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}
impl<T> WorkerSender<T> {
    pub fn new(sender: mpsc::SyncSender<T>, thread: JoinHandle<()>) -> Self {
        Self {
            inner: Arc::new(Worker {
                sender: Some(sender),
                thread: Some(thread),
            }),
        }
    }
    pub fn try_send(&self, value: T) -> std::result::Result<(), mpsc::TrySendError<T>> {
        self.inner.sender.as_ref().unwrap().try_send(value)
    }
}
impl<T> Drop for Worker<T> {
    fn drop(&mut self) {
        drop(self.sender.take());
        if let Some(thread) = self.thread.take() {
            // A queued job may itself own the last handle to its worker.
            if thread.thread().id() != std::thread::current().id() {
                let _ = thread.join();
            }
        }
    }
}

#[derive(Clone)]
pub(crate) struct Sqlite {
    tx: WorkerSender<Job>,
}
impl Sqlite {
    pub fn new(mut connection: Connection, capacity: usize, name: &str) -> Result<Self> {
        let (tx, rx) = mpsc::sync_channel::<Job>(capacity);
        let thread = std::thread::Builder::new()
            .name(name.into())
            .spawn(move || {
                while let Ok(job) = rx.recv() {
                    job(&mut connection);
                }
                let _ = connection.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)");
            })?;
        Ok(Self {
            tx: WorkerSender::new(tx, thread),
        })
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

#[cfg(test)]
mod tests {
    use super::WorkerSender;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    };

    #[test]
    fn last_handle_drains_jobs_and_waits_for_worker_cleanup() {
        let (tx, rx) = mpsc::sync_channel::<usize>(64);
        let count = Arc::new(AtomicUsize::new(0));
        let closed = Arc::new(AtomicBool::new(false));
        let worker_count = count.clone();
        let worker_closed = closed.clone();
        let thread = std::thread::spawn(move || {
            for amount in rx {
                worker_count.fetch_add(amount, Ordering::SeqCst);
            }
            worker_closed.store(true, Ordering::SeqCst);
        });
        let sender = WorkerSender::new(tx, thread);
        let clone = sender.clone();
        for _ in 0..64 {
            sender.try_send(1).unwrap();
        }
        drop(sender);
        assert!(!closed.load(Ordering::SeqCst));
        drop(clone);
        assert!(closed.load(Ordering::SeqCst));
        assert_eq!(count.load(Ordering::SeqCst), 64);
    }
}
