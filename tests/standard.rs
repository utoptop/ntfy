use axum::{
    Router,
    body::{Body, to_bytes},
    http::{HeaderMap, Request},
    response::Response,
    routing::post,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use ntfy_rs::{config::Config, http::App, model::now};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use tower::ServiceExt;
fn config(dir: &tempfile::TempDir) -> Config {
    Config {
        cache_file: dir.path().join("cache.db"),
        attachment_cache_dir: dir.path().join("files"),
        visitor_request_limit_burst: 10000,
        ..Config::default()
    }
}
async fn request(
    app: &Arc<App>,
    method: &str,
    path: &str,
    body: impl Into<Body>,
    headers: &[(&str, &str)],
) -> Response {
    let mut r = Request::builder().method(method).uri(path);
    for (k, v) in headers {
        r = r.header(*k, *v);
    }
    app.router()
        .oneshot(r.body(body.into()).unwrap())
        .await
        .unwrap()
}
async fn value(response: Response) -> Value {
    serde_json::from_slice(&to_bytes(response.into_body(), 1048576).await.unwrap()).unwrap()
}
async fn poll(app: &Arc<App>, path: &str) -> Vec<Value> {
    let response = request(app, "GET", path, Body::empty(), &[]).await;
    assert_eq!(response.status(), 200);
    let bytes = to_bytes(response.into_body(), 1048576).await.unwrap();
    std::str::from_utf8(&bytes)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}
#[tokio::test]
async fn declarative_standard_auth_is_reconciled_and_cli_users_survive() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = config(&dir);
    let hash = bcrypt::hash("secret", 4).unwrap();
    let yaml = format!(
        "auth-users:\n - 'alice:{hash}:user'\nauth-access:\n - 'alice:private_*:ro'\n - 'everyone:*:deny'\nauth-tokens:\n - 'alice:tk_12345678901234567890123456789:phone'\nattachment-file-size-limit: 15M\nmessage-size-limit: 4K\n"
    );
    let parsed: Config = serde_yaml::from_str(&yaml).unwrap();
    parsed.validate().unwrap();
    assert_eq!(parsed.attachment_file_size_limit, 15 * 1024 * 1024);
    assert_eq!(parsed.message_size_limit, 4096);
    c.auth_file = dir.path().join("auth.db").display().to_string();
    c.enable_login = true;
    c.auth_users = parsed.auth_users;
    c.auth_access = parsed.auth_access;
    c.auth_tokens = parsed.auth_tokens;
    let app = App::new(c.clone()).unwrap();
    let auth = "Bearer tk_12345678901234567890123456789";
    assert_eq!(
        request(
            &app,
            "GET",
            "/private_topic/json?poll=1",
            Body::empty(),
            &[("authorization", auth)]
        )
        .await
        .status(),
        200
    );
    assert_eq!(
        request(
            &app,
            "POST",
            "/private_topic",
            "denied",
            &[("authorization", auth)]
        )
        .await
        .status(),
        403
    );
    let account = value(
        request(
            &app,
            "GET",
            "/v1/account",
            Body::empty(),
            &[("authorization", auth)],
        )
        .await,
    )
    .await;
    assert_eq!(account["provisioned"], true);
    assert_eq!(account["tokens"][0]["provisioned"], true);
    assert_eq!(
        request(
            &app,
            "DELETE",
            "/v1/account/token",
            Body::empty(),
            &[("authorization", auth)]
        )
        .await
        .status(),
        409
    );
    let db = ntfy_rs::auth_db::AuthDb::open(&c).unwrap();
    db.add_user("manual", &hash, "user").await.unwrap();
    drop(db);
    drop(app);
    c.auth_users.clear();
    c.auth_access.clear();
    c.auth_tokens.clear();
    let app = App::new(c).unwrap();
    assert_eq!(
        request(
            &app,
            "GET",
            "/v1/account",
            Body::empty(),
            &[("authorization", auth)]
        )
        .await
        .status(),
        401
    );
    let basic = format!("Basic {}", STANDARD.encode("manual:secret"));
    assert_eq!(
        request(
            &app,
            "POST",
            "/v1/account/login",
            Body::empty(),
            &[("authorization", &basic)]
        )
        .await
        .status(),
        200
    );
}
#[tokio::test]
async fn go_cache_import_preserves_replay_schedules_and_backup() {
    let dir = tempfile::tempdir().unwrap();
    let c = config(&dir);
    let db = rusqlite::Connection::open(&c.cache_file).unwrap();
    db.execute_batch(include_str!("fixtures/go-cache-v17.sql"))
        .unwrap();
    let insert = "INSERT INTO messages(mid,sequence_id,time,event,expires,topic,message,title,priority,tags,click,icon,actions,attachment_name,attachment_type,attachment_size,attachment_expires,attachment_url,attachment_deleted,sender,user,content_type,encoding,published) VALUES(?1,'job',?2,'message',?3,'migrated','payload','title',4,'warning,robot','https://example.org','','[]','hello.txt','text/plain',7,?3,'https://example.org/file/test.txt',0,'127.0.0.1','','text/markdown','',?4)";
    db.execute(
        insert,
        rusqlite::params!["legacy000001", now() - 10, now() + 7200, 1],
    )
    .unwrap();
    db.execute(
        insert,
        rusqlite::params!["legacy000002", now() + 3600, now() + 7200, 0],
    )
    .unwrap();
    db.execute(
        "UPDATE messages SET attachment_deleted=1 WHERE mid='legacy000002'",
        [],
    )
    .unwrap();
    drop(db);
    let app = App::new(c.clone()).unwrap();
    let rows = poll(&app, "/migrated/json?poll=1").await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["id"], "legacy000001");
    assert_eq!(rows[0]["sequence_id"], "job");
    assert_eq!(rows[0]["tags"], json!(["warning", "robot"]));
    assert_eq!(rows[0]["attachment"]["name"], "hello.txt");
    let rows = poll(&app, "/migrated/json?poll=1&scheduled=1").await;
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[1]["attachment"]["deleted"], true);
    let backup =
        rusqlite::Connection::open(c.cache_file.with_extension("db.pre-rust-go-v17.bak")).unwrap();
    assert_eq!(
        backup
            .query_row("SELECT version FROM schemaVersion", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        17
    );
    assert_eq!(
        backup
            .query_row("SELECT COUNT(*) FROM messages", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        2
    );
    drop(app);
    let app = App::new(c).unwrap();
    assert_eq!(poll(&app, "/migrated/json?poll=1").await.len(), 1);
}
#[tokio::test]
async fn failed_cache_import_is_atomic_and_foreign_database_is_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let c = config(&dir);
    let db = rusqlite::Connection::open(&c.cache_file).unwrap();
    db.execute_batch(
        "CREATE TABLE unrelated(secret TEXT);INSERT INTO unrelated VALUES('preserve');",
    )
    .unwrap();
    drop(db);
    assert!(App::new(c.clone()).is_err());
    let db = rusqlite::Connection::open(&c.cache_file).unwrap();
    assert_eq!(
        db.query_row("SELECT secret FROM unrelated", [], |r| r
            .get::<_, String>(0))
            .unwrap(),
        "preserve"
    );
}
#[tokio::test]
async fn message_sequences_replace_schedules_and_emit_delete_clear_events() {
    let dir = tempfile::tempdir().unwrap();
    let app = App::new(config(&dir)).unwrap();
    let first = value(request(&app, "POST", "/jobs/job", "first", &[("delay", "1h")]).await).await;
    assert_eq!(first["sequence_id"], "job");
    let second = value(
        request(
            &app,
            "PUT",
            "/jobs/job",
            "second",
            &[("delay", "2h"), ("sequence-id", "conflicting")],
        )
        .await,
    )
    .await;
    assert_eq!(second["sequence_id"], "job");
    let rows = poll(&app, "/jobs/json?poll=1&scheduled=1").await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["message"], "second");
    assert_eq!(
        request(&app, "DELETE", "/jobs/job", Body::empty(), &[])
            .await
            .status(),
        200
    );
    let rows = poll(&app, "/jobs/json?poll=1&scheduled=1").await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["event"], "message_delete");
    assert_eq!(
        request(&app, "PUT", "/jobs/job/read", Body::empty(), &[])
            .await
            .status(),
        200
    );
    let rows = poll(&app, "/jobs/json?poll=1").await;
    assert_eq!(rows[1]["event"], "message_clear");
    let message = value(
        request(
            &app,
            "POST",
            "/",
            "{\"topic\":\"jobs\",\"sequence_id\":\"json-job\",\"message\":\"hi\"}",
            &[],
        )
        .await,
    )
    .await;
    assert_eq!(message["sequence_id"], "json-job");
}
#[tokio::test]
async fn upstream_wakeup_contains_only_id_and_skips_unifiedpush() {
    let (tx, mut rx) = tokio::sync::mpsc::channel(10);
    let mock = Router::new().route(
        "/{topic}",
        post(
            move |axum::extract::Path(topic): axum::extract::Path<String>,
                  headers: HeaderMap,
                  body: axum::body::Bytes| {
                let tx = tx.clone();
                async move {
                    tx.send((topic, headers, body)).await.unwrap();
                    "{}"
                }
            },
        ),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(async move { axum::serve(listener, mock).await.unwrap() });
    let dir = tempfile::tempdir().unwrap();
    let mut c = config(&dir);
    c.base_url = "https://ntfy.example.com".into();
    c.upstream_base_url = format!("http://{addr}");
    c.upstream_access_token = "test-upstream-token".into();
    let app = App::new(c).unwrap();
    let message = value(
        request(
            &app,
            "POST",
            "/alerts",
            "private body",
            &[("title", "private title")],
        )
        .await,
    )
    .await;
    let (topic, headers, body) = tokio::time::timeout(std::time::Duration::from_secs(3), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        topic,
        format!("{:x}", Sha256::digest("https://ntfy.example.com/alerts"))
    );
    assert!(body.is_empty());
    assert_eq!(
        headers["x-poll-id"].to_str().unwrap(),
        message["id"].as_str().unwrap()
    );
    assert_eq!(headers["authorization"], "Bearer test-upstream-token");
    assert!(!headers.contains_key("title"));
    assert_eq!(
        request(&app, "POST", "/alerts?up=1", "encrypted body", &[])
            .await
            .status(),
        200
    );
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(150), rx.recv())
            .await
            .is_err()
    );
    let poll = value(
        request(
            &app,
            "POST",
            "/poll",
            "discarded",
            &[
                ("x-poll-id", "abcdefghijkl"),
                ("filename", "should-not-upload.bin"),
            ],
        )
        .await,
    )
    .await;
    assert_eq!(poll["event"], "poll_request");
    assert_eq!(poll["poll_id"], "abcdefghijkl");
    assert!(poll.get("attachment").is_none());
    assert!(poll.get("expires").is_none());
    assert!(self::poll(&app, "/poll/json?poll=1").await.is_empty());
    task.abort();
}

#[tokio::test]
async fn webpush_registration_persists_and_checks_topic_permissions() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = config(&dir);
    let (private, public) = ntfy_rs::webpush::keys();
    c.web_push_private_key = private;
    c.web_push_public_key = public;
    c.web_push_file = dir.path().join("webpush.db").display().to_string();
    c.web_push_email_address = "admin@example.com".into();
    c.base_url = "https://ntfy.example.com".into();
    c.auth_default_access = "deny-all".into();
    c.auth_access = vec![ntfy_rs::config::Access {
        user: "*".into(),
        topic: "public".into(),
        permission: "read-only".into(),
    }];
    let app = App::new(c.clone()).unwrap();
    let (browser, auth) = ece::generate_keypair_and_auth_secret().unwrap();
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let mut registration = json!({"endpoint":"https://updates.push.services.mozilla.com/wpush/v2/test","p256dh":URL_SAFE_NO_PAD.encode(browser.pub_as_raw().unwrap()),"auth":URL_SAFE_NO_PAD.encode(auth),"topics":["private"]});
    assert_eq!(
        request(&app, "POST", "/v1/webpush", registration.to_string(), &[])
            .await
            .status(),
        401
    );
    registration["topics"] = json!(["public"]);
    assert_eq!(
        request(&app, "POST", "/v1/webpush", registration.to_string(), &[])
            .await
            .status(),
        200
    );
    let db = rusqlite::Connection::open(&c.web_push_file).unwrap();
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM subscription_topic WHERE topic='public'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    drop(app);
    let app = App::new(c).unwrap();
    assert_eq!(
        value(request(&app, "GET", "/v1/config", Body::empty(), &[]).await).await["enable_web_push"],
        true
    );
    assert_eq!(
        request(&app, "DELETE", "/v1/webpush", registration.to_string(), &[])
            .await
            .status(),
        200
    );
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM subscription_topic", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
}
#[tokio::test]
async fn trusted_proxy_networks_strip_only_known_rightmost_hops() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = config(&dir);
    c.behind_proxy = true;
    c.proxy_trusted_hosts = "10.0.0.0/8,2001:db8:1234::/48".into();
    c.visitor_request_limit_burst = 1;
    let app = App::new(c).unwrap();
    for chain in [
        "192.0.2.1, 10.1.1.1",
        "192.0.2.2, 10.2.2.2",
        "192.0.2.3, 2001:db8:1234::1",
    ] {
        assert_eq!(
            request(&app, "POST", "/proxy", "one", &[("x-forwarded-for", chain)])
                .await
                .status(),
            200
        );
    }
    assert_eq!(
        request(
            &app,
            "POST",
            "/proxy",
            "two",
            &[("x-forwarded-for", "192.0.2.1, 10.9.9.9")]
        )
        .await
        .status(),
        429
    );
}

#[test]
fn standard_configuration_cli_overrides_and_empty_optional_fields() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("server.yml");
    std::fs::write(&path,"listen_http: ':80'\nbehind_proxy: true\nauth_file:\nenable_login: true\nmessage_size_limit: 4K\n").unwrap();
    let options = std::collections::HashMap::from([
        (
            "auth-file".into(),
            dir.path().join("auth.db").display().to_string(),
        ),
        ("listen-http".into(), ":8081".into()),
    ]);
    let config = Config::load_with_overrides(path.to_str(), &options).unwrap();
    assert_eq!(config.address().unwrap().port(), 8081);
    assert!(config.behind_proxy);
    assert_eq!(config.message_size_limit, 4096);
    assert!(config.enable_login);
}
