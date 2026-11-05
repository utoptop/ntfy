use crate::{
    config::Config,
    error::{Error, Result},
    model::Envelope,
};
use reqwest::{
    Client,
    header::{AUTHORIZATION, HeaderMap, HeaderValue},
};
use sha2::{Digest, Sha256};
use std::{sync::Arc, time::Duration};
use tokio::{sync::mpsc, task::JoinSet};

pub struct Upstream {
    tx: mpsc::Sender<Arc<Envelope>>,
}
impl Upstream {
    pub fn new(config: &Config) -> Result<Option<Self>> {
        if config.upstream_base_url.is_empty() {
            return Ok(None);
        }
        let mut headers = HeaderMap::new();
        if !config.upstream_access_token.is_empty() {
            headers.insert(
                AUTHORIZATION,
                HeaderValue::from_str(&format!("Bearer {}", config.upstream_access_token))
                    .map_err(|_| Error::bad("invalid upstream-access-token"))?,
            );
        }
        let client = Client::builder()
            .default_headers(headers)
            .user_agent(concat!("ntfy-rs/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(Error::internal)?;
        let base = config.base_url.trim_end_matches('/').to_owned();
        let upstream = config.upstream_base_url.trim_end_matches('/').to_owned();
        let (tx, mut rx) = mpsc::channel::<Arc<Envelope>>(config.database_queue_size);
        let runtime = tokio::runtime::Handle::try_current().map_err(Error::internal)?;
        runtime.spawn(async move {
            let mut tasks=JoinSet::new();
            loop {
                tokio::select! {
                    _=tasks.join_next(),if !tasks.is_empty()=>{},
                    message=rx.recv(),if tasks.len()<16=>{
                        let Some(message)=message else{break};
                        let hash=format!("{:x}",Sha256::digest(format!("{base}/{}",message.message.topic).as_bytes()));
                        let url=format!("{upstream}/{hash}");let client=client.clone();
                        tasks.spawn(async move {
                            let result=async {
                                let mut response=client.post(url).header("X-Poll-ID",&message.message.id).body("").send().await?.error_for_status()?;
                                // Consume small replies to reuse the connection, while bounding untrusted response data.
                                let mut bytes=0;
                                while let Some(chunk)=response.chunk().await? {bytes+=chunk.len();if bytes>65536 {break;}}
                                Ok::<_,reqwest::Error>(())
                            }.await;
                            if let Err(error)=result {tracing::warn!(%error,message_id=%message.message.id,"upstream poll request failed");}
                        });
                    }
                }
            }
            while tasks.join_next().await.is_some() {}
        });
        Ok(Some(Self { tx }))
    }
    pub fn publish(&self, message: Arc<Envelope>) {
        if !message.message.suppress_upstream && self.tx.try_send(message).is_err() {
            tracing::warn!("upstream poll queue full; local message delivery succeeded");
        }
    }
}
