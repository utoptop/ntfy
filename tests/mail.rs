use axum::{
    body::{Body, to_bytes},
    http::Request,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use mailparse::MailHeaderMap;
use ntfy_rs::{auth_db::AuthDb, config::Config, http::App};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
    sync::mpsc,
};
use tower::ServiceExt;
async fn req(
    app: &Arc<App>,
    method: &str,
    path: &str,
    value: Value,
    auth: Option<&str>,
) -> (u16, Value) {
    let mut request = Request::builder().method(method).uri(path);
    if let Some(auth) = auth {
        request = request.header("authorization", auth);
    }
    let response = app
        .router()
        .oneshot(request.body(Body::from(value.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status().as_u16();
    let bytes = to_bytes(response.into_body(), 1048576).await.unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}
async fn mock_smtp() -> (String, mpsc::Receiver<Vec<u8>>, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap().to_string();
    let (tx, rx) = mpsc::channel(32);
    let task = tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let tx = tx.clone();
            tokio::spawn(async move {
                let mut stream = BufReader::new(stream);
                stream
                    .get_mut()
                    .write_all(b"220 mock ESMTP\r\n")
                    .await
                    .unwrap();
                loop {
                    let mut line = String::new();
                    if stream.read_line(&mut line).await.unwrap() == 0 {
                        break;
                    }
                    let reply: &[u8] = if line.starts_with("EHLO") {
                        b"250-mock\r\n250 8BITMIME\r\n".as_slice()
                    } else if line.starts_with("DATA") {
                        stream.get_mut().write_all(b"354 send\r\n").await.unwrap();
                        let mut data = Vec::new();
                        loop {
                            let mut part = Vec::new();
                            stream.read_until(b'\n', &mut part).await.unwrap();
                            if part == b".\r\n" {
                                break;
                            }
                            data.extend_from_slice(&part);
                        }
                        tx.send(data).await.unwrap();
                        b"250 accepted\r\n"
                    } else if line.starts_with("QUIT") {
                        stream.get_mut().write_all(b"221 bye\r\n").await.unwrap();
                        break;
                    } else {
                        b"250 OK\r\n"
                    };
                    stream.get_mut().write_all(reply).await.unwrap();
                }
            });
        }
    });
    (address, rx, task)
}
fn setup(dir: &tempfile::TempDir) -> Config {
    Config {
        cache_file: dir.path().join("cache.db"),
        auth_file: dir.path().join("auth.db").display().to_string(),
        attachment_cache_dir: dir.path().join("files"),
        base_url: "https://ntfy.example.com".into(),
        enable_login: true,
        enable_signup: true,
        visitor_request_limit_burst: 10000,
        ..Config::default()
    }
}
async fn mail(rx: &mut mpsc::Receiver<Vec<u8>>) -> Vec<u8> {
    tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
        .await
        .unwrap()
        .unwrap()
}
fn link_token(raw: &[u8], path: &str) -> String {
    let mail = mailparse::parse_mail(raw).unwrap();
    let body = mail.get_body().unwrap();
    body.lines()
        .find_map(|line| {
            line.trim()
                .strip_prefix(&format!("https://ntfy.example.com{path}"))
        })
        .unwrap()
        .to_owned()
}
#[tokio::test]
async fn verification_and_password_reset_links_are_single_use_and_kind_bound() {
    let (address, mut rx, task) = mock_smtp().await;
    let dir = tempfile::tempdir().unwrap();
    let mut c = setup(&dir);
    c.smtp_sender_addr = address;
    c.smtp_sender_from = "ntfy@example.com".into();
    let app = App::new(c).unwrap();
    assert_eq!(
        req(
            &app,
            "POST",
            "/v1/account",
            json!({"username":"alice","password":"secret","email":"alice@example.com"}),
            None
        )
        .await
        .0,
        200
    );
    let raw = mail(&mut rx).await;
    let token = link_token(&raw, "/account/email/verify/");
    let db = rusqlite::Connection::open(&app.config.auth_file).unwrap();
    let stored: String = db
        .query_row("SELECT token_hash FROM user_magic_link", [], |r| r.get(0))
        .unwrap();
    assert_ne!(stored, token);
    assert_eq!(stored, ntfy_rs::auth::token_hash(&token));
    assert_eq!(
        req(
            &app,
            "POST",
            "/v1/account/password/reset",
            json!({"token":token,"password":"bad"}),
            None
        )
        .await
        .0,
        400
    );
    assert_eq!(
        req(
            &app,
            "POST",
            "/v1/account/email/verify",
            json!({"token":token}),
            None
        )
        .await
        .0,
        200
    );
    assert_eq!(
        req(
            &app,
            "POST",
            "/v1/account/email/verify",
            json!({"token":token}),
            None
        )
        .await
        .0,
        400
    );
    let auth = format!("Basic {}", STANDARD.encode("alice:secret"));
    let (_, account) = req(&app, "GET", "/v1/account", json!({}), Some(&auth)).await;
    assert_eq!(account["emails"][0]["address"], "alice@example.com");
    assert_eq!(account["emails"][0]["primary"], true);
    let unknown = req(
        &app,
        "POST",
        "/v1/account/password/reset/request",
        json!({"identifier":"unknown"}),
        None,
    )
    .await;
    let known = req(
        &app,
        "POST",
        "/v1/account/password/reset/request",
        json!({"identifier":"alice@example.com"}),
        None,
    )
    .await;
    assert_eq!(known, unknown);
    let token = link_token(&mail(&mut rx).await, "/account/password/reset/");
    assert_eq!(
        req(
            &app,
            "POST",
            "/v1/account/email/verify",
            json!({"token":token}),
            None
        )
        .await
        .0,
        400
    );
    let (a, b) = tokio::join!(
        req(
            &app,
            "POST",
            "/v1/account/password/reset",
            json!({"token":token,"password":"changed"}),
            None
        ),
        req(
            &app,
            "POST",
            "/v1/account/password/reset",
            json!({"token":token,"password":"changed"}),
            None
        )
    );
    let mut statuses = [a.0, b.0];
    statuses.sort();
    assert_eq!(statuses, [200, 400]);
    assert_eq!(
        req(&app, "GET", "/v1/account", json!({}), Some(&auth))
            .await
            .0,
        401
    );
    let auth = format!("Basic {}", STANDARD.encode("alice:changed"));
    assert_eq!(
        req(&app, "GET", "/v1/account", json!({}), Some(&auth))
            .await
            .0,
        200
    );
    task.abort();
}
#[tokio::test]
async fn publishing_to_email_uses_primary_and_enforces_verified_recipients() {
    let (address, mut rx, task) = mock_smtp().await;
    let dir = tempfile::tempdir().unwrap();
    let mut c = setup(&dir);
    c.smtp_sender_addr = address;
    c.smtp_sender_from = "ntfy@example.com".into();
    c.smtp_sender_verify = true;
    let app = App::new(c).unwrap();
    assert_eq!(
        req(
            &app,
            "POST",
            "/v1/account",
            json!({"username":"reader","password":"secret","email":"reader@example.com"}),
            None
        )
        .await
        .0,
        200
    );
    let token = link_token(&mail(&mut rx).await, "/account/email/verify/");
    assert_eq!(
        req(
            &app,
            "POST",
            "/v1/account/email/verify",
            json!({"token":token}),
            None
        )
        .await
        .0,
        200
    );
    let auth = format!("Basic {}", STANDARD.encode("reader:secret"));
    assert_eq!(
        req(
            &app,
            "POST",
            "/",
            json!({"topic":"email","title":"Test mail","message":"private body","email":"yes"}),
            Some(&auth)
        )
        .await
        .0,
        200
    );
    let raw = mail(&mut rx).await;
    let mail = mailparse::parse_mail(&raw).unwrap();
    assert_eq!(
        mail.headers.get_first_value("Subject").unwrap(),
        "Test mail"
    );
    assert!(mail.get_body().unwrap().contains("private body"));
    assert_eq!(
        req(
            &app,
            "POST",
            "/",
            json!({"topic":"email","message":"denied","email":"other@example.com"}),
            Some(&auth)
        )
        .await
        .0,
        403
    );
    task.abort();
}
async fn command(stream: &mut BufReader<TcpStream>, command: &str) -> String {
    if !command.is_empty() {
        stream
            .get_mut()
            .write_all(command.as_bytes())
            .await
            .unwrap();
    }
    let mut response = String::new();
    loop {
        let mut line = String::new();
        stream.read_line(&mut line).await.unwrap();
        let last = line.as_bytes().get(3) == Some(&b' ');
        response.push_str(&line);
        if last {
            break;
        }
    }
    response
}
#[tokio::test]
async fn smtp_ingress_decodes_mime_enforces_acl_and_resets_transaction_credentials() {
    let dir = tempfile::tempdir().unwrap();
    let mut c = setup(&dir);
    c.smtp_server_domain = "mail.example.com".into();
    c.smtp_server_addr_prefix = "notify-".into();
    c.auth_default_access = "deny-all".into();
    let app = App::new(c).unwrap();
    let db = AuthDb::open(&app.config).unwrap();
    db.add_user("reader", &bcrypt::hash("secret", 4).unwrap(), "user")
        .await
        .unwrap();
    db.set_access("reader", "mail", "read-write").await.unwrap();
    let user = db.user("reader").await.unwrap().unwrap();
    let token = db
        .create_token(&user.id, "smtp", 0, "127.0.0.1")
        .await
        .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = ntfy_rs::smtp::serve(app.clone(), listener);
    let mut stream = BufReader::new(TcpStream::connect(addr).await.unwrap());
    assert!(command(&mut stream, "").await.starts_with("220"));
    assert!(
        command(&mut stream, "EHLO client\r\n")
            .await
            .starts_with("250")
    );
    assert!(
        command(&mut stream, "MAIL FROM:<sender@example.com>\r\n")
            .await
            .starts_with("250")
    );
    assert!(
        command(&mut stream, "RCPT TO:<notify-mail@evil.example>\r\n")
            .await
            .starts_with("550")
    );
    assert!(
        command(
            &mut stream,
            &format!("RCPT TO:<notify-mail+{}@mail.example.com>\r\n", token.token)
        )
        .await
        .starts_with("250")
    );
    assert!(command(&mut stream, "DATA\r\n").await.starts_with("354"));
    assert!(command(&mut stream,"Subject: =?UTF-8?B?5rWL6K+V?=\r\nContent-Type: text/plain; charset=UTF-8\r\nContent-Transfer-Encoding: base64\r\n\r\naGVsbG8=\r\n.\r\n").await.starts_with("250"));
    assert!(
        command(&mut stream, "MAIL FROM:<sender@example.com>\r\n")
            .await
            .starts_with("250")
    );
    assert!(
        command(&mut stream, "RCPT TO:<notify-mail@mail.example.com>\r\n")
            .await
            .starts_with("250")
    );
    assert!(command(&mut stream, "DATA\r\n").await.starts_with("354"));
    assert!(
        command(&mut stream, "Subject: must fail\r\n\r\nno auth\r\n.\r\n")
            .await
            .starts_with("550")
    );
    let auth = format!("Basic {}", STANDARD.encode("reader:secret"));
    let request = Request::builder()
        .uri("/mail/json?poll=1")
        .header("authorization", auth)
        .body(Body::empty())
        .unwrap();
    let response = app.router().oneshot(request).await.unwrap();
    let bytes = to_bytes(response.into_body(), 65536).await.unwrap();
    let messages: Vec<Value> = std::str::from_utf8(&bytes)
        .unwrap()
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["message"], "hello");
    assert_eq!(messages[0]["title"], "测试");
    app.shutdown.cancel();
    task.await.unwrap();
}
