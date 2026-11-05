use crate::{
    auth::{Auth, Identity},
    auth_db::random_id,
    config::Config,
    error::{Error, Result},
    http::{App, context_parts},
    model::{Envelope, now, topic_valid},
    sqlite::Sqlite,
};
use axum::{
    Json,
    body::to_bytes,
    extract::{Request, State},
    http::{Method, StatusCode},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use futures_util::{StreamExt, stream};
use p256::{
    SecretKey,
    ecdsa::{Signature, SigningKey, signature::Signer},
    elliptic_curve::sec1::ToEncodedPoint,
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{path::Path, sync::Arc, time::Duration};
use tokio::sync::mpsc;

pub fn keys() -> (String, String) {
    let key = SecretKey::random(&mut rand::rngs::OsRng);
    (
        URL_SAFE_NO_PAD.encode(key.to_bytes()),
        URL_SAFE_NO_PAD.encode(key.public_key().to_encoded_point(false).as_bytes()),
    )
}
fn key(config: &Config) -> Result<SigningKey> {
    SigningKey::from_slice(
        &URL_SAFE_NO_PAD
            .decode(&config.web_push_private_key)
            .map_err(|_| Error::bad("invalid web-push-private-key"))?,
    )
    .map_err(|_| Error::bad("invalid web-push-private-key"))
}
pub fn validate_config(config: &Config) -> Result<()> {
    if [
        &config.web_push_public_key,
        &config.web_push_private_key,
        &config.web_push_file,
        &config.web_push_email_address,
    ]
    .iter()
    .all(|v| v.is_empty())
    {
        return Ok(());
    }
    if [
        &config.web_push_public_key,
        &config.web_push_private_key,
        &config.web_push_file,
        &config.web_push_email_address,
        &config.base_url,
    ]
    .iter()
    .any(|v| v.is_empty())
    {
        return Err(Error::bad(
            "Web Push requires both VAPID keys, web-push-file, web-push-email-address and base-url",
        ));
    }
    let signing = key(config)?;
    if URL_SAFE_NO_PAD.encode(signing.verifying_key().to_encoded_point(false).as_bytes())
        != config.web_push_public_key
    {
        return Err(Error::bad("Web Push public and private keys do not match"));
    }
    if !config.web_push_email_address.contains('@')
        || config.web_push_email_address.contains(['\r', '\n'])
    {
        return Err(Error::bad("invalid web-push-email-address"));
    }
    if Config::duration(&config.web_push_expiry_warning_duration)
        >= Config::duration(&config.web_push_expiry_duration)
    {
        return Err(Error::bad("Web Push warning duration must precede expiry"));
    }
    Ok(())
}
pub fn endpoint_allowed(endpoint: &str) -> bool {
    let Ok(url) = url::Url::parse(endpoint) else {
        return false;
    };
    if url.scheme() != "https"
        || url.port().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return false;
    }
    let Some(host) = url.host_str() else {
        return false;
    };
    matches!(
        host,
        "fcm.googleapis.com"
            | "jmt17.google.com"
            | "updates.push.services.mozilla.com"
            | "web.push.apple.com"
    ) || [".mozaws.net", ".notify.windows.com"].iter().any(|suffix| {
        host.strip_suffix(suffix)
            .is_some_and(|prefix| !prefix.is_empty() && !prefix.contains('.'))
    })
}
#[derive(Clone)]
struct Subscription {
    id: String,
    endpoint: String,
    auth: String,
    p256dh: String,
    user: String,
}
fn subscription(row: &rusqlite::Row<'_>) -> rusqlite::Result<Subscription> {
    Ok(Subscription {
        id: row.get(0)?,
        endpoint: row.get(1)?,
        auth: row.get(2)?,
        p256dh: row.get(3)?,
        user: row.get(4)?,
    })
}
#[derive(Deserialize)]
struct Registration {
    endpoint: String,
    #[serde(default)]
    auth: String,
    #[serde(default)]
    p256dh: String,
    #[serde(default)]
    topics: Vec<String>,
}
#[derive(Clone)]
struct Worker {
    db: Sqlite,
    config: Arc<Config>,
    auth: Arc<Auth>,
    client: reqwest::Client,
    signing: SigningKey,
}
pub struct WebPush {
    db: Sqlite,
    tx: mpsc::Sender<Arc<Envelope>>,
}
impl WebPush {
    pub(crate) fn new(config: Arc<Config>, auth: Arc<Auth>) -> Result<Option<Arc<Self>>> {
        if config.web_push_public_key.is_empty() {
            return Ok(None);
        }
        let path = Path::new(&config.web_push_file);
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        if !path.exists() && path.as_os_str() != ":memory:" {
            crate::sqlite_backup::secure_create(path)?;
        }
        let connection = Connection::open(path)?;
        connection.busy_timeout(Duration::from_secs(5))?;
        let count: i64 = connection.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table'",
            [],
            |r| r.get(0),
        )?;
        if count > 0 {
            let valid:bool=connection.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='subscription') AND EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='subscription_topic')",[],|r|r.get(0))?;
            if !valid {
                return Err(Error::bad("web-push-file is not an ntfy Web Push database"));
            }
            let version: i64 =
                connection.query_row("SELECT version FROM schemaVersion WHERE id=1", [], |r| {
                    r.get(0)
                })?;
            if version != 1 {
                return Err(Error::bad("unsupported Web Push database version"));
            }
        } else {
            connection.execute_batch(include_str!("schema/webpush_v1.sql"))?;
        }
        connection.execute_batch("PRAGMA foreign_keys=ON;PRAGMA journal_mode=WAL;PRAGMA synchronous=FULL;PRAGMA cache_size=-512;")?;
        connection.execute_batch(&config.web_push_startup_queries)?;
        let db = Sqlite::new(
            connection,
            config.database_queue_size,
            "ntfy-webpush-sqlite",
        )?;
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(Error::internal)?;
        let worker = Worker {
            db: db.clone(),
            signing: key(&config)?,
            config: config.clone(),
            auth,
            client,
        };
        let (tx, mut rx) = mpsc::channel(config.database_queue_size);
        tokio::runtime::Handle::try_current().map_err(Error::internal)?.spawn(async move {
            let mut interval=tokio::time::interval(Duration::from_secs(3600));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {tokio::select! {
                message=rx.recv()=>{let Some(message)=message else{break};if let Err(error)=worker.publish(message).await{tracing::warn!(%error,"Web Push delivery failed");}},
                _=interval.tick()=>{if let Err(error)=worker.maintain().await{tracing::warn!(%error,"Web Push maintenance failed");}},
            }}
        });
        Ok(Some(Arc::new(Self { db, tx })))
    }
    pub fn publish(&self, message: Arc<Envelope>) {
        if self.tx.try_send(message).is_err() {
            tracing::warn!("Web Push queue full; local message delivery succeeded");
        }
    }
}
impl Worker {
    async fn authorized(&self, subscription: &Subscription, topic: &str) -> Result<bool> {
        let identity = if subscription.user.is_empty() {
            Identity::default()
        } else {
            let Some(db) = &self.auth.database else {
                return Ok(false);
            };
            let id = subscription.user.clone();
            let user: Option<(String, String)> = db
                .run(move |db| {
                    Ok(db
                        .query_row(
                            "SELECT user,role FROM user WHERE id=?1 AND deleted IS NULL",
                            [id],
                            |r| Ok((r.get(0)?, r.get(1)?)),
                        )
                        .optional()?)
                })
                .await?;
            let Some((name, role)) = user else {
                return Ok(false);
            };
            Identity {
                name: Some(name),
                id: Some(subscription.user.clone()),
                admin: role == "admin",
                token: None,
            }
        };
        Ok(self.auth.authorize(&identity, topic, false).await.is_ok())
    }
    async fn publish(&self, message: Arc<Envelope>) -> Result<()> {
        let payload=serde_json::to_vec(&json!({"event":"message","subscription_id":format!("{}/{}",self.config.base_url.trim_end_matches('/'),message.message.topic),"message":message.message})).map_err(Error::internal)?;
        let mut after = String::new();
        loop {
            let topic = message.message.topic.clone();
            let cursor = after.clone();
            let rows=self.db.run(move|db|{let mut q=db.prepare("SELECT s.id,s.endpoint,s.key_auth,s.key_p256dh,s.user_id FROM subscription s JOIN subscription_topic t ON t.subscription_id=s.id WHERE t.topic=?1 AND s.id>?2 ORDER BY s.id LIMIT 64")?;Ok(q.query_map(params![topic,cursor],subscription)?.collect::<rusqlite::Result<Vec<_>>>()?)}).await?;
            if rows.is_empty() {
                break;
            }
            after = rows.last().unwrap().id.clone();
            stream::iter(rows)
                .map(|sub| {
                    let payload = &payload;
                    let topic = &message.message.topic;
                    async move {
                        match self.authorized(&sub, topic).await {
                            Ok(true) => {
                                if let Err(error) = self.send(&sub, payload).await {
                                    tracing::warn!(%error,"Web Push endpoint delivery failed");
                                }
                            }
                            Ok(false) => {
                                let id = sub.id;
                                let _ = self
                                    .db
                                    .run(move |db| {
                                        db.execute("DELETE FROM subscription WHERE id=?1", [id])?;
                                        Ok(())
                                    })
                                    .await;
                            }
                            Err(error) => tracing::warn!(%error,"Web Push authorization failed"),
                        }
                    }
                })
                .buffer_unordered(16)
                .collect::<Vec<_>>()
                .await;
        }
        Ok(())
    }
    async fn send(&self, sub: &Subscription, payload: &[u8]) -> Result<()> {
        if !endpoint_allowed(&sub.endpoint) {
            return Err(Error::bad("untrusted Web Push endpoint"));
        }
        let (authorization, body) = encrypt(
            &self.signing,
            &self.config.web_push_email_address,
            sub,
            payload,
        )?;
        let response = self
            .client
            .post(&sub.endpoint)
            .header("authorization", authorization)
            .header("content-type", "application/octet-stream")
            .header("content-encoding", "aes128gcm")
            .header("urgency", "high")
            .header(
                "ttl",
                Config::duration(&self.config.cache_duration).as_secs(),
            )
            .body(body)
            .send()
            .await
            .map_err(Error::internal)?;
        let status = response.status();
        if matches!(status.as_u16(), 404 | 410) {
            let id = sub.id.clone();
            self.db
                .run(move |db| {
                    db.execute("DELETE FROM subscription WHERE id=?1", [id])?;
                    Ok(())
                })
                .await?;
        }
        if !status.is_success() {
            return Err(Error::new(
                StatusCode::BAD_GATEWAY,
                format!("Web Push service returned {status}"),
            ));
        }
        Ok(())
    }
    async fn maintain(&self) -> Result<()> {
        let expires =
            now() - Config::duration(&self.config.web_push_expiry_duration).as_secs() as i64;
        let warn = now()
            - Config::duration(&self.config.web_push_expiry_warning_duration).as_secs() as i64;
        self.db
            .run(move |db| {
                db.execute("DELETE FROM subscription WHERE updated_at<=?1", [expires])?;
                Ok(())
            })
            .await?;
        let mut after = String::new();
        loop {
            let cursor = after.clone();
            let rows=self.db.run(move|db|{let mut q=db.prepare("SELECT id,endpoint,key_auth,key_p256dh,user_id FROM subscription WHERE warned_at=0 AND updated_at<=?1 AND id>?2 ORDER BY id LIMIT 64")?;Ok(q.query_map(params![warn,cursor],subscription)?.collect::<rusqlite::Result<Vec<_>>>()?)}).await?;
            if rows.is_empty() {
                break;
            }
            after = rows.last().unwrap().id.clone();
            stream::iter(rows)
                .map(|sub| async move {
                    if self
                        .send(&sub, b"{\"event\":\"subscription_expiring\"}")
                        .await
                        .is_ok()
                    {
                        let _ = self
                            .db
                            .run(move |db| {
                                db.execute(
                                    "UPDATE subscription SET warned_at=?1 WHERE id=?2",
                                    params![now(), sub.id],
                                )?;
                                Ok(())
                            })
                            .await;
                    }
                })
                .buffer_unordered(16)
                .collect::<Vec<_>>()
                .await;
        }
        Ok(())
    }
}
fn encrypt(
    signing: &SigningKey,
    email: &str,
    sub: &Subscription,
    payload: &[u8],
) -> Result<(String, Vec<u8>)> {
    let url =
        url::Url::parse(&sub.endpoint).map_err(|_| Error::bad("invalid Web Push endpoint"))?;
    let header = URL_SAFE_NO_PAD.encode(b"{\"typ\":\"JWT\",\"alg\":\"ES256\"}");
    let claims=URL_SAFE_NO_PAD.encode(serde_json::to_vec(&json!({"aud":url.origin().ascii_serialization(),"exp":now()+12*3600,"sub":format!("mailto:{email}")})).map_err(Error::internal)?);
    let input = format!("{header}.{claims}");
    let signature: Signature = signing.sign(input.as_bytes());
    let auth = format!(
        "vapid t={input}.{}, k={}",
        URL_SAFE_NO_PAD.encode(signature.to_bytes()),
        URL_SAFE_NO_PAD.encode(signing.verifying_key().to_encoded_point(false).as_bytes())
    );
    let p256dh = URL_SAFE_NO_PAD
        .decode(&sub.p256dh)
        .map_err(|_| Error::bad("invalid p256dh key"))?;
    let secret = URL_SAFE_NO_PAD
        .decode(&sub.auth)
        .map_err(|_| Error::bad("invalid auth key"))?;
    let encrypted = ece::encrypt(&p256dh, &secret, payload).map_err(Error::internal)?;
    Ok((auth, encrypted))
}
pub async fn registration(State(app): State<Arc<App>>, request: Request) -> Result<Json<Value>> {
    let push = app
        .broker
        .webpush
        .as_ref()
        .ok_or_else(|| Error::bad("Web Push is not enabled"))?;
    let (parts, peer) = context_parts(&request);
    let ip = crate::http::visitor_ip(&app.config, &parts.headers, peer);
    let (_, identity, _) = app.context((parts, peer), true).await?;
    let method = request.method().clone();
    let bytes = to_bytes(request.into_body(), 65536)
        .await
        .map_err(|_| Error::bad("Web Push request too large"))?;
    let value: Registration = serde_json::from_slice(&bytes)?;
    if !endpoint_allowed(&value.endpoint) {
        return Err(Error::bad("unknown Web Push endpoint"));
    }
    if method == Method::DELETE {
        push.db
            .run(move |db| {
                db.execute(
                    "DELETE FROM subscription WHERE endpoint=?1",
                    [value.endpoint],
                )?;
                Ok(())
            })
            .await?;
        return Ok(Json(json!({"success":true})));
    }
    if value.topics.len() > 50 {
        return Err(Error::bad("too many Web Push topics"));
    }
    let key = URL_SAFE_NO_PAD
        .decode(&value.p256dh)
        .map_err(|_| Error::bad("invalid Web Push p256dh"))?;
    p256::PublicKey::from_sec1_bytes(&key).map_err(|_| Error::bad("invalid Web Push p256dh"))?;
    if URL_SAFE_NO_PAD
        .decode(&value.auth)
        .map_err(|_| Error::bad("invalid Web Push auth"))?
        .len()
        != 16
    {
        return Err(Error::bad("invalid Web Push auth length"));
    }
    for topic in &value.topics {
        topic_valid(topic)?;
        app.auth.authorize(&identity, topic, false).await?;
    }
    push.db.run(move|db|{let tx=db.transaction()?;
        let exists:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM subscription WHERE endpoint=?1)",[&value.endpoint],|r|r.get(0))?;
        let count:i64=tx.query_row("SELECT COUNT(*) FROM subscription WHERE subscriber_ip=?1",[ip.to_string()],|r|r.get(0))?;
        if !exists && count>=1000{return Err(Error::new(StatusCode::TOO_MANY_REQUESTS,"Web Push endpoint limit reached"));}
        let id:String=tx.query_row("INSERT INTO subscription(id,endpoint,key_auth,key_p256dh,user_id,subscriber_ip,updated_at,warned_at) VALUES(?1,?2,?3,?4,?5,?6,?7,0) ON CONFLICT(endpoint) DO UPDATE SET key_auth=excluded.key_auth,key_p256dh=excluded.key_p256dh,user_id=excluded.user_id,subscriber_ip=excluded.subscriber_ip,updated_at=excluded.updated_at,warned_at=0 RETURNING id",params![random_id("",12),value.endpoint,value.auth,value.p256dh,identity.id.unwrap_or_default(),ip.to_string(),now()],|r|r.get(0))?;
        tx.execute("DELETE FROM subscription_topic WHERE subscription_id=?1",[&id])?;
        for topic in value.topics{tx.execute("INSERT OR IGNORE INTO subscription_topic(subscription_id,topic) VALUES(?1,?2)",params![id,topic])?;}
        tx.commit()?;Ok(())}).await?;
    Ok(Json(json!({"success":true})))
}

#[cfg(test)]
mod tests {
    use super::*;
    use p256::ecdsa::signature::Verifier;
    #[test]
    fn vapid_signature_and_payload_are_valid_for_browser_keys() {
        let signing = SigningKey::random(&mut rand::rngs::OsRng);
        let (browser, secret) = ece::generate_keypair_and_auth_secret().unwrap();
        let sub = Subscription {
            id: "test".into(),
            endpoint: "https://updates.push.services.mozilla.com/wpush/v2/test".into(),
            auth: URL_SAFE_NO_PAD.encode(secret),
            p256dh: URL_SAFE_NO_PAD.encode(browser.pub_as_raw().unwrap()),
            user: String::new(),
        };
        let payload = b"{\"event\":\"message\",\"message\":{\"message\":\"private\"}}";
        let (auth, encrypted) = encrypt(&signing, "admin@example.com", &sub, payload).unwrap();
        let clear = ece::decrypt(&browser.raw_components().unwrap(), &secret, &encrypted).unwrap();
        assert_eq!(clear, payload);
        let jwt = auth
            .strip_prefix("vapid t=")
            .unwrap()
            .split(", k=")
            .next()
            .unwrap();
        let parts: Vec<_> = jwt.split('.').collect();
        assert_eq!(parts.len(), 3);
        let signature = Signature::from_slice(&URL_SAFE_NO_PAD.decode(parts[2]).unwrap()).unwrap();
        signing
            .verifying_key()
            .verify(format!("{}.{}", parts[0], parts[1]).as_bytes(), &signature)
            .unwrap();
        let claims: Value =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[1]).unwrap()).unwrap();
        assert_eq!(claims["aud"], "https://updates.push.services.mozilla.com");
        assert_eq!(claims["sub"], "mailto:admin@example.com");
        assert!(claims["exp"].as_i64().unwrap() > now());
    }
    #[test]
    fn endpoint_allowlist_checks_authority_and_rejects_lookalike_paths() {
        for endpoint in [
            "https://fcm.googleapis.com/send/123",
            "https://web.push.apple.com/123",
            "https://wns2-par.notify.windows.com/123",
        ] {
            assert!(endpoint_allowed(endpoint));
        }
        for endpoint in [
            "http://fcm.googleapis.com/123",
            "https://fcm.googleapis.com.evil.example/123",
            "https://evil.example/path.mozaws.net/123",
            "https://foo.bar.mozaws.net/123",
            "https://fcm.googleapis.com@127.0.0.1/123",
            "https://fcm.googleapis.com:8443/123",
            "https://127.0.0.1/123",
        ] {
            assert!(!endpoint_allowed(endpoint), "{endpoint}");
        }
    }
}
