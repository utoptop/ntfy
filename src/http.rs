use crate::{
    auth::{Auth, Identity, token_hash},
    broker::{Broker, Visitor, Visitors},
    config::Config,
    error::{Error, Result},
    model::{Attachment, Envelope, Message, RESERVED, now, topic_valid},
    params::{Filter, Params, actions, comma, priority},
    store::{Since, Store},
};
use axum::{
    Json, Router,
    body::{Body, Bytes, to_bytes},
    extract::{
        ConnectInfo, Path, Request, State,
        ws::{Message as WsMessage, WebSocket, WebSocketUpgrade},
    },
    http::{HeaderMap, HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    convert::Infallible,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{io::AsyncWriteExt, sync::Semaphore};
use tokio_util::{io::ReaderStream, sync::CancellationToken};
use tower_http::{
    cors::{Any, CorsLayer},
    services::ServeDir,
};

pub struct App {
    pub config: Arc<Config>,
    pub store: Store,
    pub broker: Arc<Broker>,
    auth: Auth,
    visitors: Visitors,
    requests: Arc<Semaphore>,
    replays: Arc<Semaphore>,
    attachment_bytes: Arc<AtomicU64>,
    pub shutdown: CancellationToken,
}

impl App {
    pub fn new(config: Config) -> Result<Arc<Self>> {
        config.validate().map_err(|e| Error::bad(e.to_string()))?;
        std::fs::create_dir_all(&config.attachment_cache_dir)?;
        let used = std::fs::read_dir(&config.attachment_cache_dir)?
            .filter_map(|e| e.ok())
            .filter_map(|e| e.metadata().ok())
            .filter(|m| m.is_file())
            .map(|m| m.len())
            .sum();
        let config = Arc::new(config);
        let broker = Broker::new(config.clone());
        let store = Store::open(config.clone(), broker.clone())?;
        Ok(Arc::new(Self {
            auth: Auth::new(config.clone()),
            visitors: Visitors::new(config.clone()),
            requests: Arc::new(Semaphore::new(config.request_limit)),
            replays: Arc::new(Semaphore::new(8)),
            config,
            store,
            broker,
            attachment_bytes: Arc::new(AtomicU64::new(used)),
            shutdown: CancellationToken::new(),
        }))
    }
    pub fn router(self: &Arc<Self>) -> Router {
        Router::new()
            .route("/", get(root).post(publish_json).put(publish_json))
            .route("/v1/health", get(health))
            .route("/v1/config", get(web_config))
            .route("/v1/account", get(account_limits))
            .route("/config.js", get(config_js))
            .route("/manifest.webmanifest", get(manifest))
            .route("/app", get(root))
            .route("/settings", get(root))
            .route("/metrics", get(metrics))
            .route(
                "/_matrix/push/v1/notify",
                get(matrix_discovery).post(matrix_notify),
            )
            .route("/file/{id}", get(attachment))
            .route(
                "/{topic}",
                get(topic_get).post(publish_topic).put(publish_topic),
            )
            .route("/{topic}/json", get(subscribe_json))
            .route("/{topic}/sse", get(subscribe_sse))
            .route("/{topic}/raw", get(subscribe_raw))
            .route("/{topic}/ws", get(subscribe_ws))
            .route("/{topic}/auth", get(topic_auth))
            .route(
                "/{topic}/publish",
                get(publish_topic).post(publish_topic).put(publish_topic),
            )
            .route(
                "/{topic}/send",
                get(publish_topic).post(publish_topic).put(publish_topic),
            )
            .route(
                "/{topic}/trigger",
                get(publish_topic).post(publish_topic).put(publish_topic),
            )
            .fallback_service(ServeDir::new(&self.config.web_root))
            .layer(middleware::from_fn_with_state(self.clone(), admission))
            .layer(
                CorsLayer::new()
                    .allow_origin(Any)
                    .allow_methods(Any)
                    .allow_headers(Any)
                    .expose_headers([header::HeaderName::from_static("x-messages-truncated")]),
            )
            .with_state(self.clone())
    }
    pub fn start_maintenance(self: &Arc<Self>) -> tokio::task::JoinHandle<()> {
        let app = self.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(1));
            let mut iterations = 0u64;
            loop {
                tokio::select! { _ = app.shutdown.cancelled() => break, _ = tick.tick() => {} }
                if let Err(e) = app.store.maintain().await {
                    tracing::error!(error=%e, "database maintenance failed");
                }
                iterations += 1;
                if iterations % 60 == 0 {
                    if let Err(e) = app.clean_attachments().await {
                        tracing::error!(error=%e, "attachment cleanup failed");
                    }
                }
            }
        })
    }
    async fn clean_attachments(&self) -> Result<()> {
        let retained = self.store.attachment_ids().await?;
        let mut entries = tokio::fs::read_dir(&self.config.attachment_cache_dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            let name = entry.file_name().to_string_lossy().into_owned();
            let meta = entry.metadata().await?;
            if !meta.is_file()
                || retained.contains(&name)
                || meta.modified()?.elapsed().unwrap_or_default().as_secs() < 60
            {
                continue;
            }
            if tokio::fs::remove_file(entry.path()).await.is_ok() {
                self.attachment_bytes
                    .fetch_sub(meta.len(), Ordering::Relaxed);
            }
        }
        Ok(())
    }
    async fn context(
        &self,
        (params, ip): (Params, IpAddr),
        charge: bool,
    ) -> Result<(Params, Identity, Arc<Visitor>)> {
        let visitor = self.visitors.get(ip)?;
        if charge {
            self.visitors.charge(&visitor)?;
        }
        let identity = self
            .auth
            .authenticate(&params.headers, &params.query)
            .await?;
        Ok((params, identity, visitor))
    }
}

fn context_parts(request: &Request) -> (Params, IpAddr) {
    (
        Params::new(request.headers().clone(), request.uri()),
        request
            .extensions()
            .get::<ConnectInfo<SocketAddr>>()
            .map(|i| i.0.ip())
            .unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST)),
    )
}

async fn admission(State(app): State<Arc<App>>, request: Request, next: Next) -> Response {
    let Ok(_permit) = app.requests.clone().try_acquire_owned() else {
        return Error::busy().into_response();
    };
    // Bound parsing, upload and initial history preparation, not the long-lived response stream.
    let mut response = match tokio::time::timeout(
        Config::duration(&app.config.request_timeout),
        next.run(request),
    )
    .await
    {
        Ok(response) => response,
        Err(_) => Error::new(StatusCode::REQUEST_TIMEOUT, "request timed out").into_response(),
    };
    response.headers_mut().insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    response
}

async fn health(State(app): State<Arc<App>>) -> Result<Json<Value>> {
    app.store.health().await?;
    Ok(Json(json!({"healthy":true})))
}
fn config_value(_app: &App) -> Value {
    json!({"base_url":"", "app_root":"/", "enable_login":false, "require_login":false, "enable_signup":false, "enable_payments":false, "enable_calls":false, "enable_emails":false, "enable_reset_password":false, "enable_reservations":false, "enable_web_push":false, "billing_contact":"", "web_push_public_key":"", "disallowed_topics":RESERVED, "config_hash":env!("CARGO_PKG_VERSION")})
}
async fn web_config(State(app): State<Arc<App>>) -> Json<Value> {
    Json(config_value(&app))
}
async fn account_limits(State(app): State<Arc<App>>, request: Request) -> Result<Json<Value>> {
    let (_, identity, _) = app.context(context_parts(&request), true).await?;
    let used = app.attachment_bytes.load(Ordering::Relaxed);
    Ok(Json(json!({
        "username": identity.name.as_deref().unwrap_or("*"),
        "role": if identity.admin { "admin" } else { "user" },
        "limits": {
            "basis":"server", "messages":0, "messages_expiry_duration":Config::duration(&app.config.cache_duration).as_secs(),
            "emails":0,"calls":0,"reservations":0,
            "attachment_file_size":app.config.attachment_file_size_limit,
            "attachment_total_size":app.config.attachment_total_size_limit,
            "attachment_expiry_duration":Config::duration(&app.config.attachment_expiry_duration).as_secs(),
            "attachment_bandwidth":0
        },
        "stats": {"messages":0,"messages_remaining":0,"emails":0,"emails_remaining":0,"calls":0,"calls_remaining":0,"reservations":0,"reservations_remaining":0,
            "attachment_total_size":used,"attachment_total_size_remaining":app.config.attachment_total_size_limit.saturating_sub(used)}
    })))
}
async fn config_js(State(app): State<Arc<App>>) -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "text/javascript; charset=utf-8"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        format!("var config = {};\n", config_value(&app)),
    )
}
async fn manifest() -> Json<Value> {
    Json(
        json!({"name":"ntfy", "short_name":"ntfy", "start_url":"/", "scope":"/", "display":"standalone", "theme_color":"#317f6f", "background_color":"#ffffff", "icons":[{"src":"/static/images/pwa-192x192.png","sizes":"192x192","type":"image/png"}]}),
    )
}
async fn root(State(app): State<Arc<App>>) -> Response {
    match tokio::fs::read(app.config.web_root.join("index.html")).await {
        Ok(html) => ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], html).into_response(),
        Err(_) => Json(json!({"name":"ntfy-rs", "version":env!("CARGO_PKG_VERSION"), "matrix_gateway":"/_matrix/push/v1/notify"})).into_response(),
    }
}
async fn topic_get(
    State(app): State<Arc<App>>,
    Path(topic): Path<String>,
    request: Request,
) -> Result<Response> {
    // Vite/PWA root assets also match the single-segment topic route.
    if topic.contains('.') {
        if topic.starts_with('.') || topic.contains(['/', '\\']) {
            return Err(Error::new(StatusCode::NOT_FOUND, "not found"));
        }
        let file = tokio::fs::File::open(app.config.web_root.join(&topic))
            .await
            .map_err(|_| Error::new(StatusCode::NOT_FOUND, "not found"))?;
        let mut response = Body::from_stream(ReaderStream::new(file)).into_response();
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_str(
                mime_guess::from_path(&topic)
                    .first_or_octet_stream()
                    .as_ref(),
            )
            .unwrap(),
        );
        response
            .headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
        return Ok(response);
    }
    let p = Params::new(request.headers().clone(), request.uri());
    topic_valid(&topic)?;
    if p.boolean(&["x-unifiedpush", "unifiedpush", "up"], false) {
        return Ok(Json(json!({"unifiedpush":{"version":1}})).into_response());
    }
    Ok(root(State(app)).await)
}
async fn metrics(State(app): State<Arc<App>>, request: Request) -> Result<Response> {
    let (_, identity, _) = app.context(context_parts(&request), true).await?;
    if !identity.admin {
        return Err(Error::new(
            StatusCode::FORBIDDEN,
            "admin credentials required",
        ));
    }
    Ok(([(header::CONTENT_TYPE,"text/plain; version=0.0.4")], format!("# TYPE ntfy_messages_published_total counter\nntfy_messages_published_total {}\n# TYPE ntfy_subscriptions gauge\nntfy_subscriptions {}\n# TYPE ntfy_active_topics gauge\nntfy_active_topics {}\n# TYPE ntfy_slow_subscribers_total counter\nntfy_slow_subscribers_total {}\n", app.broker.published.load(Ordering::Relaxed),app.broker.connections(),app.broker.topics(),app.broker.lagged.load(Ordering::Relaxed))).into_response())
}
async fn topic_auth(
    State(app): State<Arc<App>>,
    Path(topic): Path<String>,
    request: Request,
) -> Result<Json<Value>> {
    let (_, identity, _) = app.context(context_parts(&request), true).await?;
    for topic in topics(&topic)? {
        app.auth.authorize(&identity, &topic, false)?;
    }
    Ok(Json(json!({"success":true})))
}

async fn publish_json(State(app): State<Arc<App>>, request: Request) -> Result<Response> {
    let (mut p, identity, _) = app.context(context_parts(&request), true).await?;
    let body = read_body(request.into_body(), 64 * 1024).await?;
    let object: Value = serde_json::from_slice(&body)?;
    let object = object
        .as_object()
        .ok_or_else(|| Error::bad("expected a JSON object"))?;
    let topic = object
        .get("topic")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::bad("topic required"))?
        .to_string();
    let mut message = String::new();
    for (name, value) in object {
        match name.as_str() {
            "topic" => {}
            "message" => {
                message = value
                    .as_str()
                    .ok_or_else(|| Error::bad("message must be a string"))?
                    .to_owned();
            }
            "tags" => {
                let tags: Vec<String> =
                    serde_json::from_value(value.clone()).map_err(Error::from)?;
                p.query.insert(name.clone(), tags.join(","));
            }
            "actions" => {
                p.query.insert(name.clone(), value.to_string());
            }
            "title" | "priority" | "click" | "icon" | "attach" | "filename" | "delay"
            | "markdown" | "cache" | "firebase" | "sequence_id" => {
                p.query.insert(
                    name.replace('_', "-"),
                    value
                        .as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| value.to_string()),
                );
            }
            _ => return Err(Error::bad(format!("unsupported publish field: {name}"))),
        }
    }
    publish(app, topic, p, identity, Body::from(message)).await
}
async fn publish_topic(
    State(app): State<Arc<App>>,
    Path(topic): Path<String>,
    request: Request,
) -> Result<Response> {
    let (p, identity, _) = app.context(context_parts(&request), true).await?;
    publish(app, topic, p, identity, request.into_body()).await
}
async fn publish(
    app: Arc<App>,
    topic: String,
    p: Params,
    identity: Identity,
    body: Body,
) -> Result<Response> {
    topic_valid(&topic)?;
    app.auth.authorize(&identity, &topic, true)?;
    for names in [
        &["x-email", "email", "mail", "e-mail", "x-e-mail", "e"][..],
        &["x-call", "call"],
        &["x-template", "template", "tpl"],
        &["x-sequence-id", "sequence-id", "sid"],
        &["x-timezone", "timezone"],
    ] {
        if p.get(names).is_some() {
            return Err(Error::bad(format!(
                "{} is not supported by this server",
                names[0]
            )));
        }
    }
    let unified = p.boolean(&["x-unifiedpush", "unifiedpush", "up"], false)
        || p.get(&["content-encoding"]).as_deref() == Some("aes128gcm");
    let cache = p.boolean(&["x-cache", "cache"], true);
    let mut message = Message::event(topic, "message");
    message.title = p.get(&["x-title", "title", "t"]).unwrap_or_default();
    message.priority = priority(
        &p.get(&["x-priority", "priority", "prio", "p"])
            .unwrap_or_default(),
    )?;
    message.tags = comma(&p.get(&["x-tags", "tags", "tag", "ta"]).unwrap_or_default());
    message.click = p.get(&["x-click", "click"]).unwrap_or_default();
    message.icon = p.get(&["x-icon", "icon"]).unwrap_or_default();
    if !message.icon.is_empty() {
        http_url(&message.icon)?;
    }
    if let Some(value) = p.get(&["x-actions", "actions", "action"]) {
        message.actions = actions(&value)?;
    }
    if p.boolean(&["x-markdown", "markdown", "md"], false)
        || p.get(&["content-type"])
            .is_some_and(|s| s.starts_with("text/markdown"))
    {
        message.content_type = "text/markdown".into();
    }
    if let Some(delay) = p.get(&["x-delay", "delay", "x-at", "at", "x-in", "in"]) {
        if !cache || unified {
            return Err(Error::bad(
                "delay requires caching and a regular notification",
            ));
        }
        message.time = delay
            .parse::<i64>()
            .or_else(|_| {
                humantime::parse_duration(&delay)
                    .map(|d| now().saturating_add(d.as_secs().min(i64::MAX as u64) as i64))
            })
            .map_err(|_| Error::bad("delay must be a duration or Unix timestamp"))?;
        if message.time <= now()
            || message.time
                > now() + Config::duration(&app.config.message_delay_max).as_secs() as i64
        {
            return Err(Error::bad("delay out of range"));
        }
    }
    message.expires = message.time + Config::duration(&app.config.cache_duration).as_secs() as i64;
    let mut upload = None;
    if unified {
        let body = read_body(body, app.config.message_size_limit).await?;
        set_unified_payload(&mut message, &body);
    } else {
        let external = p.get(&["x-attach", "attach", "a"]);
        let filename = p.get(&["x-filename", "filename", "file", "f"]);
        if let Some(url) = external {
            let parsed = http_url(&url)?;
            message.attachment = Some(Attachment {
                name: filename.unwrap_or_else(|| {
                    parsed
                        .path_segments()
                        .and_then(|mut p| p.next_back())
                        .filter(|p| !p.is_empty())
                        .unwrap_or("attachment")
                        .into()
                }),
                url,
                ..Attachment::default()
            });
            let bytes = read_body(body, app.config.message_size_limit).await?;
            message.message = String::from_utf8(bytes.to_vec())
                .map_err(|_| Error::bad("message must be UTF-8"))?
                .trim()
                .to_owned();
        } else {
            let mut stream = body.into_data_stream();
            let mut prefix = Vec::new();
            let mut pending = None;
            while prefix.len() <= app.config.message_size_limit {
                match stream.next().await {
                    Some(Ok(bytes)) => {
                        if prefix.len() + bytes.len() > app.config.message_size_limit {
                            pending = Some(bytes);
                            break;
                        }
                        prefix.extend_from_slice(&bytes);
                    }
                    Some(Err(e)) => return Err(Error::bad(e.to_string())),
                    None => break,
                }
            }
            if filename.is_some() || pending.is_some() || std::str::from_utf8(&prefix).is_err() {
                if !cache {
                    return Err(Error::bad("uploaded attachments require caching"));
                }
                if app.config.base_url.is_empty() {
                    return Err(Error::bad(
                        "base-url must be configured for attachment uploads",
                    ));
                }
                let mut file = Upload::new(&app, &message.id).await?;
                file.write(&prefix).await?;
                if let Some(bytes) = pending {
                    file.write(&bytes).await?;
                }
                while let Some(bytes) = stream.next().await {
                    file.write(&bytes.map_err(|e| Error::bad(e.to_string()))?)
                        .await?;
                }
                let name = filename.unwrap_or_else(|| "attachment".into());
                let name = name
                    .rsplit(['/', '\\'])
                    .next()
                    .unwrap_or("attachment")
                    .replace(['\r', '\n', '"'], "_");
                let content_type = mime_guess::from_path(&name)
                    .first_or_octet_stream()
                    .to_string();
                let expires = (now()
                    + Config::duration(&app.config.attachment_expiry_duration).as_secs() as i64)
                    .min(message.expires);
                if expires <= message.time {
                    return Err(Error::bad(
                        "attachment would expire before delayed delivery",
                    ));
                }
                message.attachment = Some(Attachment {
                    name: name.clone(),
                    content_type,
                    size: file.size as i64,
                    expires,
                    url: format!(
                        "{}/file/{}",
                        app.config.base_url.trim_end_matches('/'),
                        message.id
                    ),
                });
                message.message = format!("You received a file: {name}");
                file.finish().await?;
                upload = Some(file);
            } else {
                message.message = String::from_utf8(prefix).unwrap().trim().to_owned();
            }
        }
        if let Some(value) = p.get(&["x-message", "message", "m"]) {
            message.message = value.replace("\\n", "\n");
        }
        if message.message.is_empty() {
            message.message = "triggered".into();
        }
    }
    validate_message(&message, app.config.message_size_limit)?;
    let envelope = app.store.publish(message, cache, None).await?;
    if let Some(file) = upload.as_mut() {
        file.commit();
    }
    Ok((
        [(header::CONTENT_TYPE, "application/json")],
        envelope.json.clone(),
    )
        .into_response())
}

fn validate_message(m: &Message, limit: usize) -> Result<()> {
    let message_limit = if m.encoding == "base64" {
        limit.div_ceil(3) * 4
    } else {
        limit
    };
    if m.message.len() > message_limit
        || m.title.len() > 1024
        || m.tags.iter().map(String::len).sum::<usize>() > 1024
        || m.click.len() > 2048
        || m.icon.len() > 2048
    {
        return Err(Error::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "message or metadata too large",
        ));
    }
    Ok(())
}
fn http_url(value: &str) -> Result<url::Url> {
    let url = url::Url::parse(value).map_err(|_| Error::bad("invalid URL"))?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err(Error::bad("URL must use http or https"));
    }
    Ok(url)
}
async fn read_body(body: Body, limit: usize) -> Result<Bytes> {
    to_bytes(body, limit).await.map_err(|_| {
        Error::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "request body too large or unreadable",
        )
    })
}
fn set_unified_payload(m: &mut Message, payload: &[u8]) {
    match std::str::from_utf8(payload) {
        Ok(text) => m.message = text.into(),
        Err(_) => {
            m.message = STANDARD.encode(payload);
            m.encoding = "base64".into();
        }
    }
}

struct Upload {
    path: std::path::PathBuf,
    file: tokio::fs::File,
    size: u64,
    reserved: u64,
    bytes: Arc<AtomicU64>,
    committed: bool,
}
impl Upload {
    async fn new(app: &App, id: &str) -> Result<Self> {
        let reserved = app.config.attachment_file_size_limit as u64;
        app.attachment_bytes
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
                n.checked_add(reserved)
                    .filter(|n| *n <= app.config.attachment_total_size_limit)
            })
            .map_err(|_| Error::new(StatusCode::INSUFFICIENT_STORAGE, "attachment storage full"))?;
        let path = app.config.attachment_cache_dir.join(id);
        // Do not suspend between quota reservation and construction of the RAII guard.
        match std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)
        {
            Ok(file) => Ok(Self {
                path,
                file: tokio::fs::File::from_std(file),
                size: 0,
                reserved,
                bytes: app.attachment_bytes.clone(),
                committed: false,
            }),
            Err(e) => {
                app.attachment_bytes.fetch_sub(reserved, Ordering::Relaxed);
                Err(e.into())
            }
        }
    }
    async fn write(&mut self, bytes: &[u8]) -> Result<()> {
        if self.size + bytes.len() as u64 > self.reserved {
            return Err(Error::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                "attachment exceeds file size limit",
            ));
        }
        self.file.write_all(bytes).await?;
        self.size += bytes.len() as u64;
        Ok(())
    }
    async fn finish(&mut self) -> Result<()> {
        self.file.flush().await?;
        self.file.sync_all().await?;
        Ok(())
    }
    fn commit(&mut self) {
        self.bytes
            .fetch_sub(self.reserved - self.size, Ordering::Relaxed);
        self.committed = true;
    }
}
impl Drop for Upload {
    fn drop(&mut self) {
        if !self.committed {
            let _ = std::fs::remove_file(&self.path);
            self.bytes.fetch_sub(self.reserved, Ordering::Relaxed);
        }
    }
}
async fn attachment(
    State(app): State<Arc<App>>,
    Path(id): Path<String>,
    request: Request,
) -> Result<Response> {
    if id.len() != 12 || !id.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return Err(Error::new(StatusCode::NOT_FOUND, "attachment not found"));
    }
    let (_, identity, _) = app.context(context_parts(&request), true).await?;
    let message = app
        .store
        .attachment(id.clone())
        .await?
        .ok_or_else(|| Error::new(StatusCode::NOT_FOUND, "attachment not found"))?;
    app.auth.authorize(&identity, &message.topic, false)?;
    let file = message
        .attachment
        .filter(|a| a.expires > now())
        .ok_or_else(|| Error::new(StatusCode::NOT_FOUND, "attachment expired"))?;
    let reader = tokio::fs::File::open(app.config.attachment_cache_dir.join(id))
        .await
        .map_err(|_| Error::new(StatusCode::NOT_FOUND, "attachment not found"))?;
    let mut response = Body::from_stream(ReaderStream::new(reader)).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(&file.content_type)
            .unwrap_or(HeaderValue::from_static("application/octet-stream")),
    );
    response.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&format!("attachment; filename=\"{}\"", file.name))
            .unwrap_or(HeaderValue::from_static("attachment")),
    );
    response.headers_mut().insert(
        header::CONTENT_LENGTH,
        HeaderValue::from_str(&file.size.to_string()).unwrap(),
    );
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
    Ok(response)
}

fn topics(value: &str) -> Result<Vec<String>> {
    let mut seen = HashSet::new();
    let mut result = Vec::new();
    for topic in value.split(',') {
        topic_valid(topic)?;
        if seen.insert(topic) {
            result.push(topic.to_owned());
        }
    }
    if result.len() > 32 {
        return Err(Error::bad("at most 32 topics per subscription"));
    }
    Ok(result)
}
async fn subscribe_json(
    State(app): State<Arc<App>>,
    Path(topic): Path<String>,
    request: Request,
) -> Result<Response> {
    subscribe(app, topic, request, "json", None).await
}
async fn subscribe_sse(
    State(app): State<Arc<App>>,
    Path(topic): Path<String>,
    request: Request,
) -> Result<Response> {
    subscribe(app, topic, request, "sse", None).await
}
async fn subscribe_raw(
    State(app): State<Arc<App>>,
    Path(topic): Path<String>,
    request: Request,
) -> Result<Response> {
    subscribe(app, topic, request, "raw", None).await
}
async fn subscribe_ws(
    State(app): State<Arc<App>>,
    Path(topic): Path<String>,
    ws: WebSocketUpgrade,
    request: Request,
) -> Result<Response> {
    subscribe(app, topic, request, "ws", Some(ws)).await
}

async fn subscribe(
    app: Arc<App>,
    topic: String,
    request: Request,
    format: &'static str,
    ws: Option<WebSocketUpgrade>,
) -> Result<Response> {
    let (p, identity, visitor) = app.context(context_parts(&request), true).await?;
    let names = topics(&topic)?;
    for name in &names {
        app.auth.authorize(&identity, name, false)?;
    }
    let poll = p.boolean(&["x-poll", "poll", "po"], false);
    let since = p.since(poll)?;
    let scheduled = p.boolean(&["x-scheduled", "scheduled", "sched"], false);
    let filter = Filter::parse(&p)?;
    let replay_permit = if matches!(since, Since::None) {
        None
    } else {
        Some(
            app.replays
                .clone()
                .try_acquire_owned()
                .map_err(|_| Error::busy())?,
        )
    };
    // Register before taking the snapshot. Cached live events through its watermark are duplicates.
    let mut subscription = if poll {
        None
    } else {
        Some(app.broker.subscribe(names.clone(), visitor)?)
    };
    let mut snapshot = app.store.snapshot(names.clone(), since, scheduled).await?;
    let truncated = poll && snapshot.truncated;
    let keepalive = Config::duration(&app.config.keepalive_interval);
    let stream = async_stream::stream! {
        let _app = app.clone();
        if !poll { yield Ok::<Bytes, Infallible>(Envelope::new(0, false, Message::event(&topic,"open")).wire(format)); }
        let watermark = snapshot.watermark;
        loop {
            let last = snapshot.messages.last().map(|m| m.seq);
            for envelope in snapshot.messages { if filter.matches(&envelope.message) { yield Ok(envelope.wire(format)); } }
            if poll || !snapshot.truncated || app.shutdown.is_cancelled() { break; }
            let Some(last) = last else { return; };
            // Continue a bounded replay internally, so Android clients can catch up
            // without implementing server-specific pagination. Never cross the snapshot.
            match app.store.snapshot_until(names.clone(), Since::Sequence(last), scheduled, Some(watermark)).await {
                Ok(page) => snapshot = page,
                Err(_) => return,
            }
        }
        drop(replay_permit);
        if let Some(ref mut subscription) = subscription {
            let mut interval = tokio::time::interval_at(tokio::time::Instant::now()+keepalive, keepalive);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    _ = app.shutdown.cancelled() => break,
                    _ = interval.tick() => { yield Ok(Envelope::new(0, false, Message::event(&topic,"keepalive")).wire(format)); },
                    event = subscription.streams.next() => match event {
                        Some(Ok(envelope)) => {
                            if envelope.cached && envelope.seq <= watermark { continue; }
                            if filter.matches(&envelope.message) { yield Ok(envelope.wire(format)); }
                        },
                        Some(Err(_)) => {
                            app.broker.lagged.fetch_add(1, Ordering::Relaxed);
                            // Disconnect; clients can resume their last ID from durable history.
                            break;
                        },
                        None => break,
                    }
                }
            }
        }
    };
    if let Some(ws) = ws {
        return Ok(ws
            .max_message_size(4096)
            .max_frame_size(4096)
            .on_upgrade(move |socket| websocket(socket, stream)));
    }
    let mut response = Body::from_stream(stream).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(match format {
            "sse" => "text/event-stream; charset=utf-8",
            "raw" => "text/plain; charset=utf-8",
            _ => "application/x-ndjson; charset=utf-8",
        }),
    );
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-cache, no-transform"),
    );
    response
        .headers_mut()
        .insert("x-accel-buffering", HeaderValue::from_static("no"));
    if truncated {
        response
            .headers_mut()
            .insert("x-messages-truncated", HeaderValue::from_static("1"));
    }
    Ok(response)
}
async fn websocket(
    socket: WebSocket,
    stream: impl futures_util::Stream<Item = std::result::Result<Bytes, Infallible>> + Send + 'static,
) {
    let (mut sender, mut receiver) = socket.split();
    tokio::pin!(stream);
    loop {
        tokio::select! {
            message = stream.next() => match message {
                Some(Ok(bytes)) => {
                    let message = WsMessage::Text(String::from_utf8(bytes.to_vec()).unwrap().into());
                    if !matches!(tokio::time::timeout(Duration::from_secs(10),sender.send(message)).await, Ok(Ok(()))) { break; }
                },
                _ => break,
            },
            incoming = receiver.next() => match incoming {
                Some(Ok(WsMessage::Ping(data))) => { if !matches!(tokio::time::timeout(Duration::from_secs(10), sender.send(WsMessage::Pong(data))).await, Ok(Ok(()))) { break; } },
                Some(Ok(WsMessage::Close(_))) | Some(Err(_)) | None => break,
                _ => {},
            }
        }
    }
    let _ = tokio::time::timeout(Duration::from_secs(1), sender.close()).await;
}

async fn matrix_discovery() -> Json<Value> {
    Json(json!({"unifiedpush":{"gateway":"matrix"}}))
}
async fn matrix_notify(State(app): State<Arc<App>>, request: Request) -> Result<Json<Value>> {
    if app.config.base_url.is_empty() {
        return Err(Error::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "Matrix gateway requires base-url",
        ));
    }
    let base = http_url(&app.config.base_url)?;
    // Matrix gateways are unauthenticated; per-topic ACLs still apply to every pushkey.
    let body = read_body(request.into_body(), app.config.message_size_limit).await?;
    let object: Value = serde_json::from_slice(&body)?;
    let notification = object
        .get("notification")
        .and_then(Value::as_object)
        .ok_or_else(|| Error::bad("notification object required"))?;
    let devices = notification
        .get("devices")
        .and_then(Value::as_array)
        .ok_or_else(|| Error::bad("notification.devices array required"))?;
    if devices.is_empty() || devices.len() > 64 {
        return Err(Error::bad("expected between 1 and 64 devices"));
    }
    let mut rejected = Vec::new();
    let mut delivered = HashSet::new();
    let event_id = notification
        .get("event_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    for device in devices {
        let key = device
            .get("pushkey")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| Error::bad("device pushkey required"))?;
        let parsed = url::Url::parse(key);
        let valid = parsed.as_ref().is_ok_and(|u| {
            u.origin() == base.origin()
                && u.username().is_empty()
                && u.password().is_none()
                && u.fragment().is_none()
                && topic_valid(u.path().trim_start_matches('/')).is_ok()
                && u.path().matches('/').count() == 1
        });
        if !valid {
            rejected.push(key.to_owned());
            continue;
        }
        let url = parsed.unwrap();
        let topic = url.path().trim_start_matches('/');
        let query = url.query_pairs().into_owned().collect();
        let identity = app.auth.authenticate(&HeaderMap::new(), &query).await?;
        app.auth.authorize(&identity, topic, true)?;
        if !delivered.insert(topic.to_owned()) {
            continue;
        }
        let mut message = Message::event(topic, "message");
        message.expires = now() + Config::duration(&app.config.cache_duration).as_secs() as i64;
        set_unified_payload(&mut message, &body);
        let dedup = event_id.map(|id| token_hash(&format!("{topic}\0{id}")));
        app.store.publish(message, true, dedup).await?;
    }
    Ok(Json(json!({"rejected":rejected})))
}
