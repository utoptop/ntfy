use axum::{
    body::{Body, to_bytes},
    http::Request,
    response::Response,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use ntfy_rs::{auth_db::AuthDb, config::Config, http::App, model::now};
use serde_json::{Value, json};
use std::{process::Command, sync::Arc};
use tower::ServiceExt;
fn setup() -> (tempfile::TempDir, Arc<App>) {
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();
    let dir = tempfile::tempdir().unwrap();
    let c = Config {
        auth_file: dir.path().join("auth.db").display().to_string(),
        cache_file: dir.path().join("cache.db"),
        attachment_cache_dir: dir.path().join("files"),
        enable_login: true,
        enable_signup: true,
        auth_default_access: "deny-all".into(),
        visitor_request_limit_burst: 10000,
        ..Config::default()
    };
    let app = App::new(c).unwrap();
    (dir, app)
}
fn basic(name: &str, password: &str) -> String {
    format!("Basic {}", STANDARD.encode(format!("{name}:{password}")))
}

#[tokio::test]
async fn independent_account_connections_can_create_tokens_concurrently() {
    let (_dir, app) = setup();
    let first = AuthDb::open(&app.config).unwrap();
    first
        .add_user("concurrent", &bcrypt::hash("secret", 4).unwrap(), "user")
        .await
        .unwrap();
    let id = first.user("concurrent").await.unwrap().unwrap().id;
    let second = AuthDb::open(&app.config).unwrap();
    let create = |db: AuthDb, id: String| async move {
        for _ in 0..40 {
            db.create_token(&id, "parallel", now() + 3600, "127.0.0.1")
                .await
                .unwrap();
        }
    };
    tokio::join!(
        create(first.clone(), id.clone()),
        create(second, id.clone())
    );
    assert_eq!(first.tokens(&id).await.unwrap().len(), 60);
}
async fn req(
    app: &Arc<App>,
    method: &str,
    path: &str,
    value: Option<Value>,
    auth: Option<&str>,
) -> Response {
    let mut request = Request::builder().method(method).uri(path);
    if let Some(auth) = auth {
        request = request.header("Authorization", auth);
    }
    app.router()
        .oneshot(
            request
                .body(
                    value
                        .map(|v| Body::from(v.to_string()))
                        .unwrap_or_else(Body::empty),
                )
                .unwrap(),
        )
        .await
        .unwrap()
}
async fn json(response: Response) -> Value {
    serde_json::from_slice(&to_bytes(response.into_body(), 1048576).await.unwrap()).unwrap()
}
async fn signup(app: &Arc<App>, name: &str) {
    assert_eq!(
        req(
            app,
            "POST",
            "/v1/account",
            Some(json!({"username":name,"password":"secret"})),
            None
        )
        .await
        .status(),
        200
    );
}
async fn login(app: &Arc<App>, name: &str) -> String {
    let response = req(
        app,
        "POST",
        "/v1/account/login",
        None,
        Some(&basic(name, "secret")),
    )
    .await;
    assert_eq!(response.status(), 200);
    let data = json(response).await;
    assert_eq!(data["username"], name);
    data["token"].as_str().unwrap().to_owned()
}
#[tokio::test]
async fn account_login_tokens_settings_and_restart() {
    let (_dir, app) = setup();
    signup(&app, "alice").await;
    let token = login(&app, "alice").await;
    assert_eq!(token.len(), 32);
    assert!(token.starts_with("tk_"));
    let bearer = format!("Bearer {token}");
    assert_eq!(
        req(
            &app,
            "GET",
            "/v1/account",
            None,
            Some(&basic("alice", "wrong"))
        )
        .await
        .status(),
        401
    );
    let account = json(req(&app, "GET", "/v1/account", None, Some(&bearer)).await).await;
    assert_eq!(account["username"], "alice");
    assert_eq!(account["tokens"][0]["token"], token);
    let sync = account["sync_topic"].as_str().unwrap();
    assert_eq!(
        req(&app, "GET", &format!("/{sync}/json?poll=1"), None, None)
            .await
            .status(),
        401
    );
    assert_eq!(
        req(
            &app,
            "GET",
            &format!("/{sync}/json?poll=1"),
            None,
            Some(&bearer)
        )
        .await
        .status(),
        200
    );
    assert_eq!(
        req(&app, "POST", &format!("/{sync}"), None, Some(&bearer))
            .await
            .status(),
        403
    );
    assert_eq!(
        req(
            &app,
            "PATCH",
            "/v1/account/settings",
            Some(json!({"language":"zh","notification":{"sound":"ding","min_priority":2}})),
            Some(&bearer)
        )
        .await
        .status(),
        200
    );
    assert_eq!(
        req(
            &app,
            "PATCH",
            "/v1/account/settings",
            Some(json!({"notification":{"delete_after":86400}})),
            Some(&bearer)
        )
        .await
        .status(),
        200
    );
    assert_eq!(
        req(
            &app,
            "POST",
            "/v1/account/subscription",
            Some(json!({"base_url":"https://ntfy.example.com","topic":"alerts"})),
            Some(&bearer)
        )
        .await
        .status(),
        200
    );
    assert_eq!(req(&app,"PATCH","/v1/account/subscription",Some(json!({"base_url":"https://ntfy.example.com","topic":"alerts","display_name":"Alerts"})),Some(&bearer)).await.status(),200);
    let config = (*app.config).clone();
    drop(app);
    let app = App::new(config).unwrap();
    let account = json(req(&app, "GET", "/v1/account", None, Some(&bearer)).await).await;
    assert_eq!(account["language"], "zh");
    assert_eq!(account["notification"]["sound"], "ding");
    assert_eq!(account["notification"]["delete_after"], 86400);
    assert_eq!(account["subscriptions"][0]["display_name"], "Alerts");
    assert_eq!(
        req(&app, "DELETE", "/v1/account/token", None, Some(&bearer))
            .await
            .status(),
        200
    );
    assert_eq!(
        req(&app, "GET", "/v1/account", None, Some(&bearer))
            .await
            .status(),
        401
    );
}
#[tokio::test]
async fn tokens_cannot_cross_accounts_and_expiration_is_enforced() {
    let (_dir, app) = setup();
    signup(&app, "alice").await;
    signup(&app, "bob").await;
    let alice = login(&app, "alice").await;
    let bob = login(&app, "bob").await;
    let bob_auth = format!("Bearer {bob}");
    assert_eq!(
        req(
            &app,
            "PATCH",
            "/v1/account/token",
            Some(json!({"token":alice,"label":"stolen"})),
            Some(&bob_auth)
        )
        .await
        .status(),
        404
    );
    let expired = json(
        req(
            &app,
            "POST",
            "/v1/account/token",
            Some(json!({"expires":now()-10,"label":"expired"})),
            Some(&bob_auth),
        )
        .await,
    )
    .await;
    assert_eq!(
        req(
            &app,
            "GET",
            "/v1/account",
            None,
            Some(&format!("Bearer {}", expired["token"].as_str().unwrap()))
        )
        .await
        .status(),
        401
    );
    assert_eq!(
        req(
            &app,
            "POST",
            "/v1/account/password",
            Some(json!({"password":"wrong","new_password":"newpass"})),
            Some(&bob_auth)
        )
        .await
        .status(),
        400
    );
    assert_eq!(
        req(
            &app,
            "POST",
            "/v1/account/password",
            Some(json!({"password":"secret","new_password":"newpass"})),
            Some(&bob_auth)
        )
        .await
        .status(),
        200
    );
    assert_eq!(
        req(
            &app,
            "GET",
            "/v1/account",
            None,
            Some(&basic("bob", "secret"))
        )
        .await
        .status(),
        401
    );
    assert_eq!(
        req(
            &app,
            "GET",
            "/v1/account",
            None,
            Some(&basic("bob", "newpass"))
        )
        .await
        .status(),
        200
    );
    assert_eq!(
        req(
            &app,
            "DELETE",
            "/v1/account",
            Some(json!({"password":"newpass"})),
            Some(&bob_auth)
        )
        .await
        .status(),
        200
    );
    assert_eq!(
        req(&app, "GET", "/v1/account", None, Some(&bob_auth))
            .await
            .status(),
        401
    );
    assert_eq!(
        req(
            &app,
            "GET",
            "/v1/account",
            None,
            Some(&format!("Bearer {alice}"))
        )
        .await
        .status(),
        200
    );
}
#[tokio::test]
async fn legacy_go_auth_database_migrates_with_backup_and_keeps_acl() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("auth.db");
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch("CREATE TABLE user(user TEXT PRIMARY KEY,pass TEXT NOT NULL,role TEXT NOT NULL);CREATE TABLE access(user TEXT,topic TEXT,read INT,write INT);CREATE TABLE schemaVersion(id INT PRIMARY KEY,version INT NOT NULL);INSERT INTO schemaVersion VALUES(1,1);").unwrap();
    let hash = bcrypt::hash("legacy", 4).unwrap();
    db.execute("INSERT INTO user VALUES('reader',?1,'user')", [hash])
        .unwrap();
    db.execute("INSERT INTO access VALUES('reader','private_%',1,0)", [])
        .unwrap();
    drop(db);
    let config = Config {
        auth_file: path.display().to_string(),
        enable_login: true,
        cache_file: dir.path().join("cache.db"),
        attachment_cache_dir: dir.path().join("files"),
        auth_default_access: "deny-all".into(),
        ..Config::default()
    };
    let app = App::new(config).unwrap();
    let auth = basic("reader", "legacy");
    assert_eq!(
        req(&app, "GET", "/private_topic/json?poll=1", None, Some(&auth))
            .await
            .status(),
        200
    );
    assert_eq!(
        req(&app, "POST", "/private_topic", None, Some(&auth))
            .await
            .status(),
        403
    );
    assert_eq!(
        req(&app, "GET", "/privateXtopic/json?poll=1", None, Some(&auth))
            .await
            .status(),
        403
    );
    assert_eq!(
        req(&app, "POST", "/v1/account/login", None, Some(&auth))
            .await
            .status(),
        200
    );
    // Version 4->5 escapes underscores; a pre-v5 pattern stores a literal underscore.
    let current = rusqlite::Connection::open(&path).unwrap();
    assert_eq!(
        current
            .query_row("SELECT version FROM schemaVersion", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        11
    );
    let backup = path.with_extension("db.pre-rust-v1.bak");
    let old = rusqlite::Connection::open(&backup).unwrap();
    assert_eq!(
        old.query_row("SELECT version FROM schemaVersion", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(backup).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
#[tokio::test]
async fn standard_cli_creates_users_acl_and_tokens_visible_to_running_server() {
    let (dir, app) = setup();
    let binary = env!("CARGO_BIN_EXE_ntfy");
    let file = dir.path().join("auth.db");
    let output = Command::new(binary)
        .args(["user", "add", "--role=admin", "admin"])
        .env("NTFY_AUTH_FILE", &file)
        .env("NTFY_PASSWORD", "adminpass")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        req(
            &app,
            "POST",
            "/v1/account/login",
            None,
            Some(&basic("admin", "adminpass"))
        )
        .await
        .status(),
        200
    );
    let db = AuthDb::open(&app.config).unwrap();
    db.add_user("reader", &bcrypt::hash("secret", 4).unwrap(), "user")
        .await
        .unwrap();
    let output = Command::new(binary)
        .args(["access", "reader", "private_*", "read-only"])
        .env("NTFY_AUTH_FILE", &file)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        req(
            &app,
            "GET",
            "/private_topic/json?poll=1",
            None,
            Some(&basic("reader", "secret"))
        )
        .await
        .status(),
        200
    );
    assert_eq!(
        req(
            &app,
            "POST",
            "/private_topic",
            Some(json!("denied")),
            Some(&basic("reader", "secret"))
        )
        .await
        .status(),
        403
    );
    let output = Command::new(binary)
        .args(["token", "add", "--label=test", "reader"])
        .env("NTFY_AUTH_FILE", &file)
        .output()
        .unwrap();
    assert!(output.status.success());
    let output = String::from_utf8(output.stdout).unwrap();
    let token = output
        .split_whitespace()
        .find(|s| s.starts_with("tk_"))
        .unwrap();
    assert_eq!(
        req(
            &app,
            "GET",
            "/v1/account",
            None,
            Some(&format!("Bearer {token}"))
        )
        .await
        .status(),
        200
    );
}

#[tokio::test]
async fn revoking_a_token_stops_an_existing_stream_before_next_message() {
    use http_body_util::BodyExt;
    let (_dir, app) = setup();
    signup(&app, "reader").await;
    let db = AuthDb::open(&app.config).unwrap();
    db.set_access("reader", "private", "read-write")
        .await
        .unwrap();
    let token = login(&app, "reader").await;
    let auth = format!("Bearer {token}");
    let response = req(&app, "GET", "/private/json", None, Some(&auth)).await;
    assert_eq!(response.status(), 200);
    let mut body = response.into_body();
    assert!(body.frame().await.unwrap().is_ok());
    assert_eq!(
        req(&app, "DELETE", "/v1/account/token", None, Some(&auth))
            .await
            .status(),
        200
    );
    assert_eq!(
        req(
            &app,
            "POST",
            "/private",
            Some(json!("must not arrive")),
            Some(&basic("reader", "secret"))
        )
        .await
        .status(),
        200
    );
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(1), body.frame())
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn administrative_http_api_limits_sensitive_operations_to_local_cli() {
    let (_dir, app) = setup();
    let db = AuthDb::open(&app.config).unwrap();
    db.add_user("admin", &bcrypt::hash("adminpass", 4).unwrap(), "admin")
        .await
        .unwrap();
    let admin = basic("admin", "adminpass");
    assert_eq!(
        req(&app, "GET", "/v1/users", None, None).await.status(),
        403
    );
    assert_eq!(
        req(
            &app,
            "POST",
            "/v1/users",
            Some(json!({"username":"managed","password":"secret"})),
            Some(&admin)
        )
        .await
        .status(),
        200
    );
    assert_eq!(
        req(
            &app,
            "PUT",
            "/v1/users/access",
            Some(json!({"username":"managed","topic":"private_*","permission":"ro"})),
            Some(&admin)
        )
        .await
        .status(),
        200
    );
    let users = json(
        req(
            &app,
            "GET",
            "/v1/users?username=managed",
            None,
            Some(&admin),
        )
        .await,
    )
    .await;
    assert_eq!(users[0]["grants"][0]["permission"], "read-only");
    assert!(users[0].get("hash").is_none());
    assert_eq!(
        req(
            &app,
            "POST",
            "/v1/users",
            Some(json!({"username":"second_admin","password":"secret","role":"admin"})),
            Some(&admin)
        )
        .await
        .status(),
        403
    );
    assert_eq!(
        req(
            &app,
            "GET",
            "/v1/users/tokens?username=admin",
            None,
            Some(&admin)
        )
        .await
        .status(),
        403
    );
    assert_eq!(
        req(&app, "GET", "/v1/users/tokens", None, Some(&admin))
            .await
            .status(),
        403
    );
    assert_eq!(
        req(
            &app,
            "DELETE",
            "/v1/users/access",
            Some(json!({})),
            Some(&admin)
        )
        .await
        .status(),
        403
    );
    let token = json(
        req(
            &app,
            "POST",
            "/v1/users/tokens",
            Some(json!({"username":"managed","label":"automation"})),
            Some(&admin),
        )
        .await,
    )
    .await;
    let bearer = format!("Bearer {}", token["token"].as_str().unwrap());
    assert_eq!(
        req(&app, "GET", "/private_one/json?poll=1", None, Some(&bearer))
            .await
            .status(),
        200
    );
    assert_eq!(
        req(
            &app,
            "DELETE",
            "/v1/users",
            Some(json!({"username":"managed"})),
            Some(&admin)
        )
        .await
        .status(),
        200
    );
    assert_eq!(
        req(&app, "GET", "/v1/account", None, Some(&bearer))
            .await
            .status(),
        401
    );
}

#[tokio::test]
async fn topic_reservation_roundtrip_and_delete_messages() {
    let (dir, old) = setup();
    let mut config = (*old.config).clone();
    drop(old);
    config.enable_reservations = true;
    let app = App::new(config).unwrap();
    let db = AuthDb::open(&app.config).unwrap();
    db.add_user("admin", &bcrypt::hash("secret", 4).unwrap(), "admin")
        .await
        .unwrap();
    let auth = basic("admin", "secret");
    assert_eq!(
        req(
            &app,
            "POST",
            "/v1/account/reservation",
            Some(json!({"topic":"owned_topic","everyone":"deny-all"})),
            Some(&auth)
        )
        .await
        .status(),
        200
    );
    let account = json(req(&app, "GET", "/v1/account", None, Some(&auth)).await).await;
    assert_eq!(account["reservations"][0]["topic"], "owned_topic");
    assert_eq!(account["stats"]["reservations"], 1);
    assert_eq!(
        req(
            &app,
            "POST",
            "/owned_topic",
            Some(json!("private")),
            Some(&auth)
        )
        .await
        .status(),
        200
    );
    assert_eq!(
        req(&app, "GET", "/owned_topic/json?poll=1", None, None)
            .await
            .status(),
        401
    );
    assert_eq!(
        req(
            &app,
            "DELETE",
            "/v1/account/reservation/owned_topic?delete-messages=true",
            None,
            Some(&auth)
        )
        .await
        .status(),
        200
    );
    let response = req(&app, "GET", "/owned_topic/json?poll=1", None, Some(&auth)).await;
    assert!(
        to_bytes(response.into_body(), 65536)
            .await
            .unwrap()
            .is_empty()
    );
    drop(app);
    drop(dir);
}
