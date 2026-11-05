use crate::error::Result;
use std::{
    collections::HashMap,
    future::Future,
    hash::Hash,
    sync::{Arc, Mutex, Weak},
};
use tokio::sync::OnceCell;

type Pending<V> = Weak<OnceCell<Result<V>>>;

// Share only a query that is currently in flight. Completed results are removed
// before waking callers, so this does not add a TTL to permission revocation.
pub(crate) struct ConcurrentReads<K, V> {
    pending: Mutex<HashMap<K, Pending<V>>>,
    capacity: usize,
}
impl<K: Clone + Eq + Hash, V: Clone> ConcurrentReads<K, V> {
    pub fn new(capacity: usize) -> Self {
        Self {
            pending: Mutex::new(HashMap::new()),
            capacity,
        }
    }
    pub fn invalidate(&self) {
        self.pending.lock().unwrap().clear();
    }
    pub async fn run(&self, key: K, query: impl Future<Output = Result<V>>) -> Result<V> {
        let cell = {
            let mut pending = self.pending.lock().unwrap();
            if let Some(cell) = pending.get(&key).and_then(Weak::upgrade) {
                cell
            } else {
                if pending.len() >= self.capacity {
                    pending.retain(|_, cell| cell.strong_count() != 0);
                }
                let cell = Arc::new(OnceCell::new());
                if pending.len() < self.capacity {
                    pending.insert(key.clone(), Arc::downgrade(&cell));
                }
                cell
            }
        };
        cell.get_or_init(|| async {
            let result = query.await;
            let mut pending = self.pending.lock().unwrap();
            if pending
                .get(&key)
                .is_some_and(|old| old.ptr_eq(&Arc::downgrade(&cell)))
            {
                pending.remove(&key);
            }
            result
        })
        .await
        .clone()
    }
}

#[cfg(test)]
mod tests {
    use super::ConcurrentReads;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    #[tokio::test]
    async fn concurrent_reads_share_work_without_caching_completed_values() {
        let group = Arc::new(ConcurrentReads::new(16));
        let calls = Arc::new(AtomicUsize::new(0));
        let entered = Arc::new(AtomicUsize::new(0));
        let release = Arc::new(tokio::sync::Notify::new());
        let mut jobs = Vec::new();
        for _ in 0..100 {
            let (group, calls, release) = (group.clone(), calls.clone(), release.clone());
            let entered = entered.clone();
            jobs.push(tokio::spawn(async move {
                entered.fetch_add(1, Ordering::SeqCst);
                group
                    .run("topic", async {
                        calls.fetch_add(1, Ordering::SeqCst);
                        release.notified().await;
                        Ok(7)
                    })
                    .await
                    .unwrap()
            }));
        }
        // No await separates incrementing entered from joining the pending query.
        while entered.load(Ordering::SeqCst) != 100 {
            tokio::task::yield_now().await;
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        release.notify_one();
        for job in jobs {
            assert_eq!(job.await.unwrap(), 7);
        }
        assert_eq!(group.run("topic", async { Ok(9) }).await.unwrap(), 9);
        assert!(group.pending.lock().unwrap().is_empty());
    }
}
