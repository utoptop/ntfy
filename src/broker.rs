use crate::{
    auth::Auth,
    config::Config,
    error::{Error, Result},
    model::Envelope,
    upstream::Upstream,
    webpush::WebPush,
};
use axum::http::StatusCode;
use futures_util::stream::SelectAll;
use std::{
    collections::HashMap,
    net::IpAddr,
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicU64, Ordering},
    },
    time::Instant,
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, broadcast};
use tokio_stream::wrappers::BroadcastStream;

pub struct Broker {
    topics: RwLock<HashMap<String, broadcast::Sender<Arc<Envelope>>>>,
    slots: Arc<Semaphore>,
    config: Arc<Config>,
    upstream: Option<Upstream>,
    pub(crate) webpush: Option<Arc<WebPush>>,
    pub published: AtomicU64,
    pub lagged: AtomicU64,
}
pub struct Subscription {
    pub streams: SelectAll<BroadcastStream<Arc<Envelope>>>,
    topics: Vec<String>,
    broker: Arc<Broker>,
    _global: OwnedSemaphorePermit,
    _visitor: OwnedSemaphorePermit,
}
impl Drop for Subscription {
    fn drop(&mut self) {
        self.streams.clear();
        let mut topics = self.broker.topics.write().unwrap();
        for name in &self.topics {
            if topics.get(name).is_some_and(|s| s.receiver_count() == 0) {
                topics.remove(name);
            }
        }
    }
}
impl Broker {
    pub fn new(config: Arc<Config>, auth: Arc<Auth>) -> Result<Arc<Self>> {
        let upstream = Upstream::new(&config)?;
        let webpush = WebPush::new(config.clone(), auth)?;
        Ok(Arc::new(Self {
            upstream,
            webpush,
            topics: RwLock::new(HashMap::new()),
            slots: Arc::new(Semaphore::new(config.subscription_limit)),
            config,
            published: AtomicU64::new(0),
            lagged: AtomicU64::new(0),
        }))
    }
    pub fn subscribe(
        self: &Arc<Self>,
        names: Vec<String>,
        visitor: Arc<Visitor>,
    ) -> Result<Subscription> {
        let global = self
            .slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| limited("subscription limit reached"))?;
        let visitor = visitor
            .slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| limited("visitor subscription limit reached"))?;
        let mut topics = self.topics.write().unwrap();
        if topics.len() + names.iter().filter(|n| !topics.contains_key(*n)).count()
            > self.config.topic_limit
        {
            return Err(limited("active topic limit reached"));
        }
        let streams = names
            .iter()
            .map(|name| {
                let tx = topics
                    .entry(name.clone())
                    .or_insert_with(|| broadcast::channel(self.config.subscriber_buffer_size).0);
                BroadcastStream::new(tx.subscribe())
            })
            .collect();
        Ok(Subscription {
            streams,
            topics: names,
            broker: self.clone(),
            _global: global,
            _visitor: visitor,
        })
    }
    pub fn publish(&self, message: Arc<Envelope>) {
        self.published.fetch_add(1, Ordering::Relaxed);
        if let Some(push) = &self.webpush {
            push.publish(message.clone());
        }
        if let Some(upstream) = &self.upstream {
            upstream.publish(message.clone());
        }
        if let Some(sender) = self.topics.read().unwrap().get(&message.message.topic) {
            let _ = sender.send(message);
        }
    }
    pub fn connections(&self) -> usize {
        self.config.subscription_limit - self.slots.available_permits()
    }
    pub fn topics(&self) -> usize {
        self.topics.read().unwrap().len()
    }
}
fn limited(message: &str) -> Error {
    Error::new(StatusCode::TOO_MANY_REQUESTS, message)
}

struct Bucket {
    tokens: f64,
    updated: Instant,
}
pub struct Visitor {
    pub ip: IpAddr,
    bucket: Mutex<Bucket>,
    slots: Arc<Semaphore>,
}
pub struct Visitors {
    entries: Mutex<HashMap<IpAddr, Arc<Visitor>>>,
    config: Arc<Config>,
}
impl Visitors {
    pub fn new(config: Arc<Config>) -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            config,
        }
    }
    pub fn get(&self, ip: IpAddr) -> Result<Arc<Visitor>> {
        let mut entries = self.entries.lock().unwrap();
        if let Some(v) = entries.get(&ip) {
            return Ok(v.clone());
        }
        if entries.len() >= self.config.visitor_limit {
            entries.retain(|_, v| {
                v.slots.available_permits() != self.config.visitor_subscription_limit
                    || v.bucket.lock().unwrap().updated.elapsed().as_secs() < 600
            });
            if entries.len() >= self.config.visitor_limit {
                return Err(Error::busy());
            }
        }
        let v = Arc::new(Visitor {
            ip,
            bucket: Mutex::new(Bucket {
                tokens: self.config.visitor_request_limit_burst as f64,
                updated: Instant::now(),
            }),
            slots: Arc::new(Semaphore::new(self.config.visitor_subscription_limit)),
        });
        entries.insert(ip, v.clone());
        Ok(v)
    }
    pub fn charge(&self, visitor: &Visitor) -> Result<()> {
        let mut bucket = visitor.bucket.lock().unwrap();
        let now = Instant::now();
        bucket.tokens = (bucket.tokens
            + now.duration_since(bucket.updated).as_secs_f64()
                / Config::duration(&self.config.visitor_request_limit_replenish).as_secs_f64())
        .min(self.config.visitor_request_limit_burst as f64);
        bucket.updated = now;
        if bucket.tokens < 1.0 {
            return Err(limited("request rate limit reached"));
        }
        bucket.tokens -= 1.0;
        Ok(())
    }
}
