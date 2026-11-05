use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
    response::Response,
};
use base64::{
    Engine,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use futures_util::{SinkExt, StreamExt};
use http_body_util::BodyExt;
use ntfy_rs::{
    config::{Access, Config, User},
    http::App,
    model::{Message, now},
    store::Since,
};
use serde_json::{Value, json};
use std::sync::Arc;
use tower::ServiceExt;

fn setup(change: impl FnOnce(&mut Config)) -> (tempfile::TempDir, Arc<App>) {
    let dir = tempfile::tempdir().unwrap();
    let mut config = Config {
        cache_file: dir.path().join("cache.db"),
        attachment_cache_dir: dir.path().join("attachments"),
        base_url: "https://ntfy.example.com".into(),
        visitor_request_limit_burst: 100_000,
        visitor_subscription_limit: 1000,
        ..Config::default()
    };
    change(&mut config);
    let app = App::new(config).unwrap();
    (dir, app)
}
async fn req(
    app: &Arc<App>,
    method: &str,
    path: &str,
    body: impl Into<Body>,
    headers: &[(&str, &str)],
) -> Response {
    let mut builder = Request::builder().method(method).uri(path);
    for (k, v) in headers {
        builder = builder.header(*k, *v);
    }
    app.router()
        .oneshot(builder.body(body.into()).unwrap())
        .await
        .unwrap()
}
async fn bytes(response: Response) -> bytes::Bytes {
    to_bytes(response.into_body(), 20 * 1024 * 1024)
        .await
        .unwrap()
}
async fn value(response: Response) -> Value {
    serde_json::from_slice(&bytes(response).await).unwrap()
}
async fn poll(app: &Arc<App>, path: &str) -> Vec<Value> {
    let response = req(app, "GET", path, Body::empty(), &[]).await;
    assert_eq!(response.status(), StatusCode::OK);
    String::from_utf8(bytes(response).await.to_vec())
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}
async fn frame(body: &mut Body) -> Value {
    let frame = tokio::time::timeout(std::time::Duration::from_secs(3), body.frame())
        .await
        .unwrap()
        .unwrap()
        .unwrap()
        .into_data()
        .unwrap();
    serde_json::from_slice(&frame).unwrap()
}

#[tokio::test]
async fn publish_aliases_json_and_replay() {
    let (_dir, app) = setup(|_| {});
    let response = req(
        &app,
        "POST",
        "/alerts",
        "hello\nworld",
        &[
            ("Title", "Backup"),
            ("Priority", "urgent"),
            ("Tags", "warning,server"),
        ],
    )
    .await;
    assert_eq!(response.status(), 200);
    let first = value(response).await;
    assert_eq!(first["priority"], 5);
    assert_eq!(first["title"], "Backup");
    assert_eq!(first["id"].as_str().unwrap().len(), 12);
    let second = value(
        req(
            &app,
            "PUT",
            "/",
            json!({"topic":"alerts","message":"你好","markdown":true,"priority":2}).to_string(),
            &[],
        )
        .await,
    )
    .await;
    assert_eq!(second["content_type"], "text/markdown");
    let history = poll(&app, "/alerts/json?poll=1").await;
    assert_eq!(history.len(), 2);
    assert_eq!(history[0]["id"], first["id"]);
    let later = poll(
        &app,
        &format!(
            "/alerts/json?poll=1&since={}",
            first["id"].as_str().unwrap()
        ),
    )
    .await;
    assert_eq!(later.len(), 1);
    assert_eq!(later[0]["id"], second["id"]);
    assert_eq!(
        poll(&app, "/alerts/json?poll=1&since=latest").await[0]["id"],
        second["id"]
    );
}

#[tokio::test]
async fn unifiedpush_preserves_binary_whitespace_and_content_encoding() {
    let (_dir, app) = setup(|_| {});
    assert_eq!(
        value(req(&app, "GET", "/upTest?up=1", Body::empty(), &[]).await).await["unifiedpush"]["version"],
        1
    );
    let binary = vec![0, 255, 1, 128, 10];
    let result = value(req(&app, "POST", "/upTest?up=1", binary.clone(), &[]).await).await;
    assert_eq!(result["encoding"], "base64");
    assert_eq!(
        STANDARD
            .decode(result["message"].as_str().unwrap())
            .unwrap(),
        binary
    );
    let raw = "\n  {\"a\":1} \n\0";
    let result = value(
        req(
            &app,
            "PUT",
            "/upTest",
            raw,
            &[("Content-Encoding", "aes128gcm")],
        )
        .await,
    )
    .await;
    assert_eq!(result["message"], raw);
    assert!(result.get("encoding").is_none());
    assert_eq!(
        req(&app, "POST", "/upTest?up=1", vec![0; 4097], &[])
            .await
            .status(),
        413
    );
    assert_eq!(poll(&app, "/upTest/json?poll=1").await.len(), 2);
}

#[tokio::test]
async fn live_stream_fanout_cache_disabled_and_cleanup() {
    let (_dir, app) = setup(|_| {});
    let mut a = req(&app, "GET", "/live/json", Body::empty(), &[])
        .await
        .into_body();
    let mut b = req(&app, "GET", "/live/json", Body::empty(), &[])
        .await
        .into_body();
    assert_eq!(frame(&mut a).await["event"], "open");
    assert_eq!(frame(&mut b).await["event"], "open");
    let published = value(req(&app, "POST", "/live?cache=no", "ephemeral", &[]).await).await;
    assert_eq!(frame(&mut a).await["id"], published["id"]);
    assert_eq!(frame(&mut b).await["id"], published["id"]);
    assert!(poll(&app, "/live/json?poll=1").await.is_empty());
    drop(a);
    drop(b);
    assert_eq!(app.broker.connections(), 0);
    assert_eq!(app.broker.topics(), 0);
}

#[tokio::test]
async fn sse_raw_and_filter_formats() {
    let (_dir, app) = setup(|_| {});
    req(
        &app,
        "POST",
        "/format",
        "line1\nline2",
        &[("Priority", "high"), ("Tags", "a,b")],
    )
    .await;
    let raw = bytes(req(&app, "GET", "/format/raw?poll=1", Body::empty(), &[]).await).await;
    assert_eq!(raw, "line1 line2\n");
    let sse = bytes(req(&app, "GET", "/format/sse?poll=1", Body::empty(), &[]).await).await;
    assert!(sse.starts_with(b"data: {"));
    assert!(!sse.windows(14).any(|w| w == b"event: message"));
    assert!(sse.ends_with(b"\n\n"));
    assert_eq!(
        poll(&app, "/format/json?poll=1&priority=high&tags=a,b")
            .await
            .len(),
        1
    );
    assert!(
        poll(&app, "/format/json?poll=1&priority=low")
            .await
            .is_empty()
    );
    assert!(poll(&app, "/format/json?poll=1&tags=a,c").await.is_empty());
}

#[tokio::test]
async fn replay_is_globally_ordered_and_paginates_without_loss() {
    let (_dir, app) = setup(|c| c.replay_limit = 2);
    let mut ids = Vec::new();
    for topic in ["z", "a", "z", "a", "z"] {
        ids.push(
            value(req(&app, "POST", &format!("/{topic}"), "x", &[]).await).await["id"]
                .as_str()
                .unwrap()
                .to_owned(),
        );
    }
    let response = req(&app, "GET", "/a,z/json?poll=1", Body::empty(), &[]).await;
    assert_eq!(response.headers()["x-messages-truncated"], "1");
    let rows: Vec<Value> = String::from_utf8(bytes(response).await.to_vec())
        .unwrap()
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect();
    assert_eq!(rows[0]["id"], ids[0]);
    assert_eq!(rows[1]["id"], ids[1]);
    let next = poll(&app, &format!("/a,z/json?poll=1&since={}", ids[1])).await;
    assert_eq!(next[0]["id"], ids[2]);
    assert_eq!(next[1]["id"], ids[3]);
    let mut stream = req(&app, "GET", "/a,z/json?since=all", Body::empty(), &[])
        .await
        .into_body();
    assert_eq!(frame(&mut stream).await["event"], "open");
    for id in ids {
        assert_eq!(frame(&mut stream).await["id"], id);
    }
    drop(stream);
    assert_eq!(app.broker.connections(), 0);
}

#[tokio::test]
async fn slow_subscriber_disconnects_and_can_recover() {
    let (_dir, app) = setup(|c| c.subscriber_buffer_size = 2);
    let mut stream = req(&app, "GET", "/slow/json", Body::empty(), &[])
        .await
        .into_body();
    frame(&mut stream).await;
    for _ in 0..8 {
        req(&app, "POST", "/slow", "buffer", &[]).await;
    }
    assert!(stream.frame().await.is_none());
    assert_eq!(poll(&app, "/slow/json?poll=1").await.len(), 8);
}

fn secure_config(config: &mut Config) {
    config.auth_default_access = "deny-all".into();
    config.auth_users = vec![User {
        name: "phone".into(),
        token_hashes: vec![ntfy_rs::auth::token_hash("phone-secret")],
        ..User::default()
    }];
    config.auth_access = vec![
        Access {
            user: "*".into(),
            topic: "up*".into(),
            permission: "write-only".into(),
        },
        Access {
            user: "phone".into(),
            topic: "up*".into(),
            permission: "read-write".into(),
        },
    ];
}

#[tokio::test]
async fn authentication_acl_bearer_basic_token_and_query() {
    let (_dir, app) = setup(secure_config);
    assert_eq!(
        req(&app, "POST", "/upPrivate?up=1", "data", &[])
            .await
            .status(),
        200
    );
    assert_eq!(
        req(&app, "GET", "/upPrivate/json?poll=1", Body::empty(), &[])
            .await
            .status(),
        401
    );
    assert_eq!(
        req(
            &app,
            "GET",
            "/upPrivate/json?poll=1",
            Body::empty(),
            &[("Authorization", "Bearer phone-secret")]
        )
        .await
        .status(),
        200
    );
    let basic = format!("Basic {}", STANDARD.encode(":phone-secret"));
    assert_eq!(
        req(
            &app,
            "GET",
            "/upPrivate/auth",
            Body::empty(),
            &[("Authorization", &basic)]
        )
        .await
        .status(),
        200
    );
    let auth = URL_SAFE_NO_PAD.encode("Bearer phone-secret");
    assert_eq!(
        req(
            &app,
            "GET",
            &format!("/upPrivate/auth?auth={auth}"),
            Body::empty(),
            &[]
        )
        .await
        .status(),
        200
    );
    assert_eq!(
        req(
            &app,
            "POST",
            "/private",
            "x",
            &[("Authorization", "Bearer phone-secret")]
        )
        .await
        .status(),
        403
    );
    assert_eq!(
        req(
            &app,
            "GET",
            "/upPrivate/auth",
            Body::empty(),
            &[("Authorization", "Bearer wrong")]
        )
        .await
        .status(),
        401
    );
    assert_eq!(
        req(
            &app,
            "GET",
            "/upPrivate,private/auth",
            Body::empty(),
            &[("Authorization", "Bearer phone-secret")]
        )
        .await
        .status(),
        403
    );
}

#[tokio::test]
async fn argon2_password_authentication() {
    use argon2::{
        Argon2, PasswordHasher,
        password_hash::{SaltString, rand_core::OsRng},
    };
    let hash = Argon2::default()
        .hash_password(b"correct", &SaltString::generate(&mut OsRng))
        .unwrap()
        .to_string();
    let (_dir, app) = setup(|c| {
        c.auth_default_access = "deny-all".into();
        c.auth_users = vec![User {
            name: "admin".into(),
            password_hash: hash,
            admin: true,
            ..User::default()
        }];
    });
    for (password, status) in [("correct", 200), ("wrong", 401)] {
        let auth = format!("Basic {}", STANDARD.encode(format!("admin:{password}")));
        assert_eq!(
            req(&app, "POST", "/secured", "x", &[("Authorization", &auth)])
                .await
                .status(),
            status
        );
    }
}

fn matrix(keys: &[&str], event: &str) -> String {
    json!({"notification":{"event_id":event,"room_id":"!room:example.org","counts":{"unread":1},"devices":keys.iter().map(|key|json!({"pushkey":key,"app_id":"io.element.android.x"})).collect::<Vec<_>>()}}).to_string()
}

#[tokio::test]
async fn matrix_gateway_discovery_multidevice_dedup_and_binary_chain() {
    let (_dir, app) = setup(|_| {});
    assert_eq!(
        value(req(&app, "GET", "/_matrix/push/v1/notify", Body::empty(), &[]).await).await,
        json!({"unifiedpush":{"gateway":"matrix"}})
    );
    let body = matrix(
        &[
            "https://ntfy.example.com/upPhone?up=1",
            "https://ntfy.example.com/upTablet?up=1",
        ],
        "$event",
    );
    let mut stream = req(&app, "GET", "/upPhone/json", Body::empty(), &[])
        .await
        .into_body();
    frame(&mut stream).await;
    for _ in 0..3 {
        let response = req(&app, "POST", "/_matrix/push/v1/notify", body.clone(), &[]).await;
        assert_eq!(response.status(), 200);
        assert_eq!(value(response).await, json!({"rejected":[]}));
    }
    assert_eq!(frame(&mut stream).await["message"], body);
    assert_eq!(poll(&app, "/upPhone/json?poll=1").await.len(), 1);
    assert_eq!(poll(&app, "/upTablet/json?poll=1").await.len(), 1);
}

#[tokio::test]
async fn matrix_rejects_foreign_and_malformed_keys_without_network_requests() {
    let (_dir, app) = setup(|_| {});
    let keys = [
        "https://evil.example/upPhone",
        "https://ntfy.example.com.evil.example/upPhone",
        "https://ntfy.example.com@evil.example/upPhone",
        "http://ntfy.example.com/upPhone",
        "https://ntfy.example.com/up%00secret",
        "https://ntfy.example.com/up%2Fsecret",
        "https://ntfy.example.com/upOne,upTwo",
        "https://ntfy.example.com/upPhone/json",
        "https://ntfy.example.com:444/upPhone",
        "https://ntfy.example.com//upPhone",
        "https://ntfy.example.com/upPhone#fragment",
    ];
    let response = req(
        &app,
        "POST",
        "/_matrix/push/v1/notify",
        matrix(&keys, "$bad"),
        &[],
    )
    .await;
    assert_eq!(response.status(), 200);
    assert_eq!(value(response).await["rejected"], json!(keys));
    assert!(poll(&app, "/upPhone/json?poll=1").await.is_empty());
}

#[tokio::test]
async fn matrix_acl_no_base_url_oversize_and_bad_json() {
    let (_dir, app) = setup(secure_config);
    assert_eq!(
        req(
            &app,
            "POST",
            "/_matrix/push/v1/notify",
            matrix(&["https://ntfy.example.com/upPhone?up=1"], "$one"),
            &[]
        )
        .await
        .status(),
        200
    );
    assert_eq!(
        req(
            &app,
            "POST",
            "/_matrix/push/v1/notify",
            matrix(&["https://ntfy.example.com/private"], "$two"),
            &[]
        )
        .await
        .status(),
        401
    );
    for body in [
        "null",
        "{}",
        "{\"notification\":{\"devices\":[]}}",
        "{\"notification\":{\"devices\":[{}]}}",
        "not-json",
    ] {
        assert_eq!(
            req(&app, "POST", "/_matrix/push/v1/notify", body, &[])
                .await
                .status(),
            400
        );
    }
    assert_eq!(
        req(&app, "POST", "/_matrix/push/v1/notify", vec![0; 4097], &[])
            .await
            .status(),
        413
    );
    let (_dir, app) = setup(|c| c.base_url.clear());
    assert_eq!(
        req(
            &app,
            "POST",
            "/_matrix/push/v1/notify",
            matrix(&["https://ntfy.example.com/upPhone"], "$event"),
            &[]
        )
        .await
        .status(),
        503
    );
}

#[tokio::test]
async fn durable_restart_and_matrix_dedup_survive_restart() {
    let (_dir, app) = setup(|_| {});
    let body = matrix(&["https://ntfy.example.com/upPhone?up=1"], "$persist");
    req(&app, "POST", "/_matrix/push/v1/notify", body.clone(), &[]).await;
    let config = (*app.config).clone();
    drop(app);
    let app = App::new(config).unwrap();
    assert_eq!(poll(&app, "/upPhone/json?poll=1").await.len(), 1);
    req(&app, "POST", "/_matrix/push/v1/notify", body, &[]).await;
    assert_eq!(poll(&app, "/upPhone/json?poll=1").await.len(), 1);
}

#[tokio::test]
async fn cache_eviction_expiry_and_delayed_delivery() {
    let (_dir, app) = setup(|c| c.cache_max_messages = 2);
    for _ in 0..5 {
        req(&app, "POST", "/evict", "x", &[]).await;
    }
    assert!(poll(&app, "/evict/json?poll=1").await.len() <= 2);
    let mut expired = Message::event("expired", "message");
    expired.expires = now() - 1;
    app.store.publish(expired, true, None).await.unwrap();
    assert!(poll(&app, "/expired/json?poll=1").await.is_empty());
    app.store.maintain().await.unwrap();
    let mut delayed = Message::event("future", "message");
    delayed.time = now() + 1;
    delayed.expires = now() + 100;
    delayed.message = "later".into();
    app.store.publish(delayed, true, None).await.unwrap();
    assert!(poll(&app, "/future/json?poll=1").await.is_empty());
    assert_eq!(poll(&app, "/future/json?poll=1&sched=1").await.len(), 1);
    let mut stream = req(&app, "GET", "/future/json", Body::empty(), &[])
        .await
        .into_body();
    frame(&mut stream).await;
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    app.store.maintain().await.unwrap();
    assert_eq!(frame(&mut stream).await["message"], "later");
}

#[tokio::test]
async fn attachments_stream_upload_download_limit_and_auth() {
    let (_dir, app) = setup(|c| c.attachment_file_size_limit = 8192);
    let payload = vec![255; 8000];
    let response = req(
        &app,
        "PUT",
        "/files",
        payload.clone(),
        &[("Filename", "sample.bin")],
    )
    .await;
    assert_eq!(response.status(), 200);
    let message = value(response).await;
    let path = format!("/file/{}", message["id"].as_str().unwrap());
    let response = req(&app, "GET", &path, Body::empty(), &[]).await;
    assert_eq!(response.status(), 200);
    assert_eq!(bytes(response).await.to_vec(), payload);
    assert_eq!(
        req(
            &app,
            "POST",
            "/files",
            vec![1; 8193],
            &[("Filename", "large.bin")]
        )
        .await
        .status(),
        413
    );
    let files = std::fs::read_dir(&app.config.attachment_cache_dir)
        .unwrap()
        .count();
    assert_eq!(files, 1);
    assert_eq!(
        req(&app, "GET", "/file/../../etc/passwd", Body::empty(), &[])
            .await
            .status(),
        404
    );
}

#[tokio::test]
async fn resource_limits_and_bad_parameters() {
    let (_dir, app) = setup(|c| {
        c.visitor_subscription_limit = 1;
        c.visitor_request_limit_burst = 10;
    });
    let stream = req(&app, "GET", "/one/json", Body::empty(), &[]).await;
    assert_eq!(stream.status(), 200);
    assert_eq!(
        req(&app, "GET", "/two/json", Body::empty(), &[])
            .await
            .status(),
        429
    );
    drop(stream);
    assert_eq!(
        req(
            &app,
            "GET",
            "/two/json?since=not-a-cursor",
            Body::empty(),
            &[]
        )
        .await
        .status(),
        400
    );
    assert_eq!(
        req(&app, "POST", "/one?priority=99", "x", &[])
            .await
            .status(),
        400
    );
    assert_eq!(
        req(&app, "POST", "/one?delay=1s&cache=no", "x", &[])
            .await
            .status(),
        400
    );
    assert_eq!(
        req(&app, "POST", "/one?email=x@example.org", "x", &[])
            .await
            .status(),
        400
    );
    assert_eq!(
        req(&app, "POST", "/one?sequence-id=bad%21", "x", &[])
            .await
            .status(),
        400
    );
    assert_eq!(req(&app, "POST", "/docs", "x", &[]).await.status(), 400);
    assert_eq!(req(&app, "POST", "/a,b", "x", &[]).await.status(), 400);
    assert_eq!(req(&app, "POST", "/one", "x", &[]).await.status(), 200);
    assert_eq!(req(&app, "POST", "/one", "x", &[]).await.status(), 429);
}

#[tokio::test]
async fn replay_live_boundary_no_missing_or_duplicate_messages() {
    let (_dir, app) = setup(|c| c.subscriber_buffer_size = 512);
    for _ in 0..20 {
        req(&app, "POST", "/boundary", "before", &[]).await;
    }
    let publisher = {
        let app = app.clone();
        tokio::spawn(async move {
            for _ in 0..100 {
                req(&app, "POST", "/boundary", "racing", &[]).await;
            }
        })
    };
    let mut stream = req(&app, "GET", "/boundary/json?since=all", Body::empty(), &[])
        .await
        .into_body();
    assert_eq!(frame(&mut stream).await["event"], "open");
    let mut ids = std::collections::HashSet::new();
    for _ in 0..120 {
        let m = frame(&mut stream).await;
        assert!(ids.insert(m["id"].as_str().unwrap().to_owned()));
    }
    publisher.await.unwrap();
    assert_eq!(ids.len(), 120);
}

#[tokio::test]
async fn websocket_real_socket_and_shutdown() {
    let (_dir, app) = setup(|_| {});
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let router = app.router();
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{address}/upPhone/ws"))
        .await
        .unwrap();
    let open = ws.next().await.unwrap().unwrap().into_text().unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&open).unwrap()["event"],
        "open"
    );
    req(&app, "POST", "/upPhone?up=1", "matrix", &[]).await;
    let message = ws.next().await.unwrap().unwrap().into_text().unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&message).unwrap()["message"],
        "matrix"
    );
    ws.send(tokio_tungstenite::tungstenite::Message::Ping(
        vec![1, 2, 3].into(),
    ))
    .await
    .unwrap();
    assert!(matches!(
        ws.next().await.unwrap().unwrap(),
        tokio_tungstenite::tungstenite::Message::Pong(_)
    ));
    app.shutdown.cancel();
    let closed = tokio::time::timeout(std::time::Duration::from_secs(3), ws.next())
        .await
        .unwrap();
    assert!(matches!(
        closed,
        Some(Ok(tokio_tungstenite::tungstenite::Message::Close(_))) | None
    ));
    server.abort();
}

#[tokio::test]
async fn actions_and_external_attachments() {
    let (_dir, app) = setup(|_| {});
    let response=req(&app,"POST","/action","hello",&[("Actions","view, Website, https://example.org, clear=true; http, Restart, https://example.org/restart, method=PUT"),("Attach","https://example.org/test.png")]).await;
    assert_eq!(response.status(), 200);
    let m = value(response).await;
    assert_eq!(m["actions"][0]["clear"], true);
    assert_eq!(m["actions"][1]["method"], "PUT");
    assert_eq!(m["attachment"]["name"], "test.png");
    assert_eq!(
        req(
            &app,
            "POST",
            "/action",
            "hello",
            &[("Attach", "file:///etc/passwd")]
        )
        .await
        .status(),
        400
    );
}

#[test]
fn strict_configuration_and_legacy_database_protection() {
    assert!(serde_yaml::from_str::<Config>("unsupported-option: true").is_err());
    assert!(
        serde_yaml::from_str::<Config>("upstream-base-url: https://ntfy.sh")
            .unwrap()
            .validate()
            .is_err()
    );
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("old.db");
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute("CREATE TABLE old_messages(id TEXT)", [])
        .unwrap();
    let c = Config {
        cache_file: path,
        attachment_cache_dir: dir.path().join("files"),
        ..Config::default()
    };
    assert!(App::new(c).is_err());
}

#[tokio::test]
async fn web_root_assets_and_spa_routes_are_served_without_path_traversal() {
    let (_dir, app) = setup(|c| {
        c.web_root = c.cache_file.parent().unwrap().join("web");
        std::fs::create_dir_all(&c.web_root).unwrap();
        std::fs::write(c.web_root.join("index.html"), "<html>ntfy</html>").unwrap();
        std::fs::write(
            c.web_root.join("sw.js"),
            "self.addEventListener('fetch',()=>{});",
        )
        .unwrap();
    });
    for path in ["/", "/app", "/settings", "/upPhone", "/sw.js"] {
        assert_eq!(
            req(&app, "GET", path, Body::empty(), &[]).await.status(),
            200,
            "{path}"
        );
    }
    for path in ["/missing.js", "/%2e%2e%2fsecret", "/static/../../secret"] {
        assert_eq!(
            req(&app, "GET", path, Body::empty(), &[]).await.status(),
            404,
            "{path}"
        );
    }
    let config = value(req(&app, "GET", "/v1/config", Body::empty(), &[]).await).await;
    assert_eq!(config["enable_web_push"], false);
    let account = value(req(&app, "GET", "/v1/account", Body::empty(), &[]).await).await;
    assert_eq!(account["limits"]["attachment_file_size"], 15 * 1024 * 1024);
}

#[tokio::test]
async fn unknown_cursor_does_not_leak_private_topic_and_ttl_applies() {
    let (_dir, app) = setup(|_| {});
    let secret = value(req(&app, "POST", "/secret", "a", &[]).await).await;
    req(&app, "POST", "/public", "b", &[]).await;
    let snapshot = app
        .store
        .snapshot(
            vec!["public".into()],
            Since::Id(secret["id"].as_str().unwrap().into()),
            false,
        )
        .await
        .unwrap();
    assert_eq!(snapshot.messages.len(), 1);
    assert_eq!(snapshot.messages[0].message.topic, "public");
}

#[test]
fn proxy_configuration_accepts_yaml_and_validates_header_names() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("server.yml");
    std::fs::write(&path, "listen-http: ':80'\nbehind-proxy: true\ncache-duration: 72h\nproxy-forwarded-header: X-Real-IP\n").unwrap();
    let config = Config::load(Some(path.to_str().unwrap())).unwrap();
    assert!(config.behind_proxy);
    assert_eq!(config.proxy_forwarded_header, "X-Real-IP");
    assert!(!Config::default().behind_proxy);
    let invalid = Config {
        proxy_forwarded_header: "bad header".into(),
        ..Config::default()
    };
    assert!(invalid.validate().is_err());
}

#[tokio::test]
async fn proxy_request_limits_use_the_last_hop_only_when_enabled() {
    for enabled in [false, true] {
        let (_dir, app) = setup(|c| {
            c.behind_proxy = enabled;
            c.visitor_request_limit_burst = 1;
            c.visitor_request_limit_replenish = "1h".into();
        });
        let first = req(
            &app,
            "POST",
            "/proxy",
            "one",
            &[("X-Forwarded-For", "198.51.100.1, 203.0.113.1")],
        )
        .await;
        assert_eq!(first.status(), 200);
        let second = req(
            &app,
            "POST",
            "/proxy",
            "two",
            &[("X-Forwarded-For", "198.51.100.1, 203.0.113.2")],
        )
        .await;
        assert_eq!(second.status(), if enabled { 200 } else { 429 });
        // Changing an earlier header or hop must not change the selected visitor.
        let forged = req(
            &app,
            "POST",
            "/proxy",
            "three",
            &[
                ("X-Forwarded-For", "198.51.100.99"),
                ("X-Forwarded-For", "198.51.100.88, 203.0.113.1"),
            ],
        )
        .await;
        assert_eq!(forged.status(), 429);
    }
}

#[tokio::test]
async fn proxy_ipv6_custom_headers_and_malformed_hops() {
    for (header, first, second) in [
        ("X-Forwarded-For", "198.51.100.1:1234", "198.51.100.1"),
        ("X-Forwarded-For", "[2001:db8::1]:443", "2001:db8::1"),
        ("X-Real-IP", "203.0.113.1", "203.0.113.1"),
        (
            "Forwarded",
            "for=198.51.100.1;proto=https, For=\"[2001:db8::1]:1234\";by=proxy",
            "for=\"[2001:db8::1]\"",
        ),
    ] {
        let (_dir, app) = setup(|c| {
            c.behind_proxy = true;
            c.proxy_forwarded_header = header.into();
            c.visitor_request_limit_burst = 1;
            c.visitor_request_limit_replenish = "1h".into();
        });
        assert_eq!(
            req(&app, "POST", "/proxy", "one", &[(header, first)])
                .await
                .status(),
            200
        );
        assert_eq!(
            req(&app, "POST", "/proxy", "two", &[(header, second)])
                .await
                .status(),
            429
        );
    }
    for (header, invalid) in [
        ("X-Forwarded-For", "198.51.100.1, garbage"),
        ("X-Forwarded-For", "198.51.100.1,"),
        ("X-Forwarded-For", "\"203.0.113.1"),
        ("Forwarded", "for=unknown"),
        ("Forwarded", "for=198.51.100.1;for=203.0.113.1"),
        ("Forwarded", "by=proxy"),
    ] {
        let (_dir, app) = setup(|c| {
            c.behind_proxy = true;
            c.proxy_forwarded_header = header.into();
            c.visitor_request_limit_burst = 1;
            c.visitor_request_limit_replenish = "1h".into();
        });
        let request = Request::builder()
            .method("POST")
            .uri("/proxy")
            .header(header, invalid)
            .extension(axum::extract::ConnectInfo(
                "192.0.2.10:1234".parse::<std::net::SocketAddr>().unwrap(),
            ))
            .body(Body::from("one"))
            .unwrap();
        assert_eq!(app.router().oneshot(request).await.unwrap().status(), 200);
        let request = Request::builder()
            .method("POST")
            .uri("/proxy")
            .extension(axum::extract::ConnectInfo(
                "192.0.2.10:9999".parse::<std::net::SocketAddr>().unwrap(),
            ))
            .body(Body::from("two"))
            .unwrap();
        assert_eq!(app.router().oneshot(request).await.unwrap().status(), 429);
    }
}

#[tokio::test]
async fn proxy_subscription_limits_are_per_forwarded_client() {
    let (_dir, app) = setup(|c| {
        c.behind_proxy = true;
        c.visitor_subscription_limit = 1;
    });
    let first = req(
        &app,
        "GET",
        "/one/json",
        Body::empty(),
        &[("X-Forwarded-For", "203.0.113.1")],
    )
    .await;
    let second = req(
        &app,
        "GET",
        "/one/json",
        Body::empty(),
        &[("X-Forwarded-For", "203.0.113.2")],
    )
    .await;
    assert_eq!(first.status(), 200);
    assert_eq!(second.status(), 200);
    assert_eq!(
        req(
            &app,
            "GET",
            "/two/json",
            Body::empty(),
            &[("X-Forwarded-For", "203.0.113.1")]
        )
        .await
        .status(),
        429
    );
    drop(first);
    assert_eq!(
        req(
            &app,
            "GET",
            "/two/json",
            Body::empty(),
            &[("X-Forwarded-For", "203.0.113.1")]
        )
        .await
        .status(),
        200
    );
}
