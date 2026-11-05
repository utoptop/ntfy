use crate::{
    auth::Identity,
    config::Config,
    error::{Error, Result},
    http::App,
    model::{Message, topic_valid},
};
use axum::{body::Body, extract::ConnectInfo, http::Request};
use base64::{Engine, engine::general_purpose::STANDARD};
use lettre::{
    AsyncSmtpTransport, AsyncTransport, Tokio1Executor,
    message::Mailbox,
    transport::smtp::{
        authentication::Credentials,
        client::{Tls, TlsParameters},
    },
};
use mailparse::MailHeaderMap;
use rusqlite::params;
use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
    sync::{Semaphore, mpsc},
    task::JoinSet,
};
use tower::ServiceExt;

pub fn address(value: &str) -> std::result::Result<SocketAddr, std::net::AddrParseError> {
    if value.starts_with(':') {
        format!("0.0.0.0{value}").parse()
    } else {
        value.parse()
    }
}
pub(crate) struct Mail {
    to: Mailbox,
    subject: String,
    body: String,
}
pub(crate) struct Sender {
    tx: mpsc::Sender<Mail>,
    buckets: Mutex<HashMap<String, (f64, Instant)>>,
    config: Arc<Config>,
}
impl Sender {
    pub fn new(config: Arc<Config>) -> Result<Option<Self>> {
        if config.smtp_sender_addr.is_empty() {
            return Ok(None);
        }
        let server = url::Url::parse(&format!("smtp://{}", config.smtp_sender_addr))
            .map_err(|_| Error::bad("invalid smtp-sender-addr"))?;
        let host = server
            .host_str()
            .filter(|_| {
                server.username().is_empty()
                    && server.password().is_none()
                    && server.path().is_empty()
            })
            .ok_or_else(|| Error::bad("invalid smtp-sender-addr"))?;
        let port = server
            .port()
            .ok_or_else(|| Error::bad("smtp-sender-addr requires a port"))?;
        let from: Mailbox = config
            .smtp_sender_from
            .parse()
            .map_err(|_| Error::bad("invalid smtp-sender-from"))?;
        let tls = TlsParameters::new(host.to_owned()).map_err(Error::internal)?;
        let mut transport = AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(host)
            .port(port)
            .timeout(Some(Duration::from_secs(10)))
            .tls(if port == 465 {
                Tls::Wrapper(tls)
            } else if config.smtp_sender_user.is_empty() {
                Tls::Opportunistic(tls)
            } else {
                Tls::Required(tls)
            });
        if !config.smtp_sender_user.is_empty() {
            transport = transport.credentials(Credentials::new(
                config.smtp_sender_user.clone(),
                config.smtp_sender_pass.clone(),
            ));
        }
        let transport = transport.build();
        let (tx, mut rx) = mpsc::channel::<Mail>(config.database_queue_size);
        tokio::runtime::Handle::try_current().map_err(Error::internal)?.spawn(async move {
            let mut tasks=JoinSet::new();
            loop {tokio::select!{
                _=tasks.join_next(),if !tasks.is_empty()=>{},
                job=rx.recv(),if tasks.len()<8=>{
                    let Some(job)=job else{break};let transport=transport.clone();let from=from.clone();
                    tasks.spawn(async move {let result=async {
                        let mail=lettre::Message::builder().from(from).to(job.to).subject(job.subject).body(job.body).map_err(Error::internal)?;
                        transport.send(mail).await.map_err(Error::internal)?;Ok::<(),Error>(())
                    }.await;if let Err(error)=result{tracing::warn!(%error,"SMTP delivery failed");}});
                }
            }}while tasks.join_next().await.is_some(){}
        });
        Ok(Some(Self {
            tx,
            buckets: Mutex::new(HashMap::new()),
            config,
        }))
    }
    pub fn reserve(&self, key: String) -> Result<mpsc::OwnedPermit<Mail>> {
        let permit = self
            .tx
            .clone()
            .try_reserve_owned()
            .map_err(|_| Error::busy())?;
        let mut buckets = self.buckets.lock().unwrap();
        if !buckets.contains_key(&key) && buckets.len() >= self.config.visitor_limit {
            let idle = Config::duration(&self.config.visitor_email_limit_replenish)
                .saturating_mul(self.config.visitor_email_limit_burst as u32);
            buckets.retain(|_, (_, updated)| updated.elapsed() < idle);
            if buckets.len() >= self.config.visitor_limit {
                return Err(Error::busy());
            }
        }
        let (tokens, updated) = buckets
            .entry(key)
            .or_insert((self.config.visitor_email_limit_burst as f64, Instant::now()));
        *tokens = (*tokens
            + updated.elapsed().as_secs_f64()
                / Config::duration(&self.config.visitor_email_limit_replenish).as_secs_f64())
        .min(self.config.visitor_email_limit_burst as f64);
        *updated = Instant::now();
        if *tokens < 1.0 {
            return Err(Error::new(
                axum::http::StatusCode::TOO_MANY_REQUESTS,
                "email rate limit reached",
            ));
        }
        *tokens -= 1.0;
        Ok(permit)
    }
    pub fn notification(&self, permit: mpsc::OwnedPermit<Mail>, to: Mailbox, message: &Message) {
        let subject = if message.title.is_empty() {
            format!("ntfy: {}", message.topic)
        } else {
            message.title.clone()
        };
        let mut body = format!(
            "{}\n\nTopic: {}/{}\n",
            message.message,
            self.config.base_url.trim_end_matches('/'),
            message.topic
        );
        if let Some(attachment) = &message.attachment {
            body.push_str(&format!("Attachment: {}\n", attachment.url));
        }
        if !message.click.is_empty() {
            body.push_str(&format!("Open: {}\n", message.click));
        }
        permit.send(Mail { to, subject, body });
    }
    pub fn send(
        &self,
        permit: mpsc::OwnedPermit<Mail>,
        to: Mailbox,
        subject: String,
        body: String,
    ) {
        permit.send(Mail { to, subject, body });
    }
}
pub(crate) async fn recipient(app: &App, identity: &Identity, value: &str) -> Result<Mailbox> {
    let mut value = value.to_owned();
    if matches!(value.as_str(), "yes" | "1" | "true") {
        let (Some(db), Some(id)) = (&app.auth.database, &identity.id) else {
            return Err(Error::bad(
                "email=yes requires a signed-in account with a primary email",
            ));
        };
        let id = id.clone();
        value = db
            .run(move |db| {
                db.query_row(
                    "SELECT email FROM user_email WHERE user_id=?1 ORDER BY is_primary DESC,email LIMIT 1",
                    [id],
                    |r| r.get(0),
                )
                .map_err(|_| Error::bad("account has no primary email"))
            })
            .await?;
    }
    let mailbox: Mailbox = value
        .parse()
        .map_err(|_| Error::bad("invalid recipient email address"))?;
    if app.config.smtp_sender_verify {
        let (Some(db), Some(id)) = (&app.auth.database, &identity.id) else {
            return Err(Error::new(
                axum::http::StatusCode::FORBIDDEN,
                "verified recipient required",
            ));
        };
        let (id, address) = (id.clone(), mailbox.email.to_string());
        let verified = db
            .run(move |db| {
                Ok(db.query_row(
                    "SELECT EXISTS(SELECT 1 FROM user_email WHERE user_id=?1 AND email=?2)",
                    params![id, address],
                    |r| r.get::<_, bool>(0),
                )?)
            })
            .await?;
        if !verified {
            return Err(Error::new(
                axum::http::StatusCode::FORBIDDEN,
                "recipient is not verified for this account",
            ));
        }
    }
    Ok(mailbox)
}

pub fn serve(app: Arc<App>, listener: TcpListener) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let slots = Arc::new(Semaphore::new(app.config.request_limit.min(32)));
        let mut tasks = JoinSet::new();
        loop {
            tokio::select! {
                _=app.shutdown.cancelled()=>break,
                _=tasks.join_next(),if !tasks.is_empty()=>{},
                connection=listener.accept()=>{
                    let Ok((mut socket,peer))=connection else{break};
                    let Ok(permit)=slots.clone().try_acquire_owned() else{let _=tokio::time::timeout(Duration::from_secs(1),socket.write_all(b"421 Too many connections\r\n")).await;continue};
                    let app=app.clone();tasks.spawn(async move{let _permit=permit;let _=tokio::time::timeout(Duration::from_secs(300),session(app,socket,peer)).await;});
                }
            }
        }
        tasks.abort_all();
        while tasks.join_next().await.is_some() {}
    })
}
async fn line(socket: &mut BufReader<TcpStream>) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut limited = socket.take(65537);
    tokio::time::timeout(
        Duration::from_secs(10),
        limited.read_until(b'\n', &mut bytes),
    )
    .await
    .map_err(|_| Error::bad("SMTP read timeout"))??;
    if bytes.len() > 65536 {
        return Err(Error::bad("SMTP line too long"));
    }
    Ok(bytes)
}
async fn reply(socket: &mut BufReader<TcpStream>, text: &str) -> Result<()> {
    tokio::time::timeout(
        Duration::from_secs(10),
        socket.get_mut().write_all(text.as_bytes()),
    )
    .await
    .map_err(|_| Error::bad("SMTP write timeout"))??;
    Ok(())
}
async fn session(app: Arc<App>, stream: TcpStream, peer: SocketAddr) -> Result<()> {
    let mut socket = BufReader::new(stream);
    reply(
        &mut socket,
        &format!("220 {} ESMTP ntfy\r\n", app.config.smtp_server_domain),
    )
    .await?;
    let mut greeted = false;
    let mut mail = false;
    let mut recipient = None::<(String, String)>;
    let mut credentials = String::new();
    loop {
        let bytes = line(&mut socket).await?;
        if bytes.is_empty() {
            break;
        }
        let command = std::str::from_utf8(&bytes)
            .map_err(|_| Error::bad("invalid SMTP command"))?
            .trim_end_matches(['\r', '\n']);
        let (verb, arg) = command.split_once(' ').unwrap_or((command, ""));
        match verb.to_ascii_uppercase().as_str() {
            "EHLO" | "HELO" => {
                greeted = true;
                mail = false;
                recipient = None;
                reply(
                    &mut socket,
                    &format!(
                        "250-{}\r\n250-SIZE 1048576\r\n250-8BITMIME\r\n250 AUTH PLAIN\r\n",
                        app.config.smtp_server_domain
                    ),
                )
                .await?;
            }
            "QUIT" => {
                reply(&mut socket, "221 Bye\r\n").await?;
                break;
            }
            "NOOP" => reply(&mut socket, "250 OK\r\n").await?,
            "RSET" => {
                mail = false;
                recipient = None;
                reply(&mut socket, "250 Reset\r\n").await?;
            }
            "AUTH" if greeted && !mail => {
                let mut parts = arg.split_ascii_whitespace();
                if !parts
                    .next()
                    .is_some_and(|v| v.eq_ignore_ascii_case("PLAIN"))
                {
                    reply(&mut socket, "504 Only AUTH PLAIN supported\r\n").await?;
                    continue;
                }
                let supplied = if let Some(value) = parts.next() {
                    value.as_bytes().to_vec()
                } else {
                    reply(&mut socket, "334 \r\n").await?;
                    line(&mut socket).await?
                };
                let raw = STANDARD.decode(supplied.trim_ascii());
                if let Ok(raw) = raw {
                    let parts: Vec<_> = raw.split(|b| *b == 0).collect();
                    if parts.len() == 3 {
                        let mut combined = parts[1].to_vec();
                        combined.push(b':');
                        combined.extend_from_slice(parts[2]);
                        credentials = format!("Basic {}", STANDARD.encode(combined));
                        reply(&mut socket, "235 Authentication accepted\r\n").await?;
                        continue;
                    }
                }
                reply(&mut socket, "535 Invalid credentials\r\n").await?;
            }
            "MAIL" if greeted && arg.to_ascii_uppercase().starts_with("FROM:") => {
                mail = true;
                recipient = None;
                reply(&mut socket, "250 Sender accepted\r\n").await?;
            }
            "RCPT" if mail && arg.to_ascii_uppercase().starts_with("TO:") => {
                if recipient.is_some() {
                    reply(&mut socket, "452 Only one recipient supported\r\n").await?;
                    continue;
                }
                match parse_recipient(&app.config, arg[3..].trim()) {
                    Ok(parsed) => {
                        recipient = Some(parsed);
                        reply(&mut socket, "250 Recipient accepted\r\n").await?;
                    }
                    Err(_) => reply(&mut socket, "550 Invalid recipient\r\n").await?,
                }
            }
            "DATA" if mail && recipient.is_some() => {
                reply(&mut socket, "354 End with <CRLF>.<CRLF>\r\n").await?;
                let mut data = Vec::new();
                loop {
                    let part = line(&mut socket).await?;
                    if part.is_empty() {
                        return Ok(());
                    }
                    if part == b".\r\n" || part == b".\n" {
                        break;
                    }
                    if data.len() + part.len() > 1048576 {
                        reply(&mut socket, "552 Message too large\r\n").await?;
                        return Ok(());
                    }
                    data.extend_from_slice(if part.starts_with(b"..") {
                        &part[1..]
                    } else {
                        &part
                    });
                }
                let (topic, token) = recipient.take().unwrap();
                mail = false;
                let result = deliver(&app, peer, &topic, &token, &credentials, &data).await;
                match result {
                    Ok(()) => reply(&mut socket, "250 Message accepted\r\n").await?,
                    Err(error)
                        if error.status.is_server_error()
                            || error.status == axum::http::StatusCode::TOO_MANY_REQUESTS =>
                    {
                        reply(&mut socket, "451 Temporary publishing failure\r\n").await?
                    }
                    Err(_) => reply(&mut socket, "550 Message rejected\r\n").await?,
                }
            }
            _ => reply(&mut socket, "503 Invalid command or command sequence\r\n").await?,
        }
    }
    Ok(())
}
fn parse_recipient(config: &Config, value: &str) -> Result<(String, String)> {
    let value = value
        .strip_prefix('<')
        .and_then(|s| s.split_once('>').map(|(address, _)| address))
        .unwrap_or(value);
    let (local, domain) = value
        .rsplit_once('@')
        .ok_or_else(|| Error::bad("invalid address"))?;
    if !domain.eq_ignore_ascii_case(&config.smtp_server_domain) {
        return Err(Error::bad("invalid domain"));
    }
    let local = local
        .strip_prefix(&config.smtp_server_addr_prefix)
        .ok_or_else(|| Error::bad("invalid address prefix"))?;
    let (topic, token) = local.split_once('+').unwrap_or((local, ""));
    topic_valid(topic)?;
    Ok((topic.into(), token.into()))
}
async fn deliver(
    app: &Arc<App>,
    peer: SocketAddr,
    topic: &str,
    token: &str,
    credentials: &str,
    data: &[u8],
) -> Result<()> {
    // mailparse is recursive; bound possible MIME nesting before asking it to parse.
    if data
        .windows(12)
        .filter(|w| w.eq_ignore_ascii_case(b"content-type"))
        .count()
        > 64
    {
        return Err(Error::bad("too many MIME parts"));
    }
    let mail = mailparse::parse_mail(data).map_err(|_| Error::bad("invalid MIME message"))?;
    let mut texts = Vec::new();
    extract_text(&mail, 0, &mut texts)?;
    let mut message = texts
        .iter()
        .find(|(kind, _)| kind == "text/plain")
        .or_else(|| texts.first())
        .map(|(_, body)| body.trim().to_owned())
        .unwrap_or_default();
    let mut title = mail
        .headers
        .get_first_value("Subject")
        .unwrap_or_default()
        .trim()
        .to_owned();
    if message.is_empty() {
        message = std::mem::take(&mut title);
    }
    let mut limit = app.config.message_size_limit.min(message.len());
    while !message.is_char_boundary(limit) {
        limit -= 1;
    }
    message.truncate(limit);
    let mut request = Request::builder().method("POST").uri(format!("/{topic}"));
    if !title.is_empty() {
        request = request.header("title", title);
    }
    if !token.is_empty() {
        request = request.header("authorization", format!("Bearer {token}"));
    } else if !credentials.is_empty() {
        request = request.header("authorization", credentials);
    }
    request = request.header(&app.config.proxy_forwarded_header, peer.ip().to_string());
    let mut request = request
        .body(Body::from(message))
        .map_err(|_| Error::bad("invalid mail headers"))?;
    request.extensions_mut().insert(ConnectInfo(peer));
    let response = app
        .router()
        .oneshot(request)
        .await
        .map_err(Error::internal)?;
    if !response.status().is_success() {
        return Err(Error::new(response.status(), "mail publish rejected"));
    }
    Ok(())
}
fn extract_text(
    mail: &mailparse::ParsedMail<'_>,
    depth: usize,
    texts: &mut Vec<(String, String)>,
) -> Result<()> {
    if depth > 2 {
        return Err(Error::bad("MIME nesting too deep"));
    }
    if mail.ctype.mimetype.starts_with("multipart/") {
        for part in &mail.subparts {
            extract_text(part, depth + 1, texts)?;
        }
        return Ok(());
    }
    if matches!(mail.ctype.mimetype.as_str(), "text/plain" | "text/html") {
        let mut body = mail
            .get_body()
            .map_err(|_| Error::bad("invalid MIME body"))?;
        if mail.ctype.mimetype == "text/html" {
            body = html2text::from_read(body.as_bytes(), 120)
                .map_err(|_| Error::bad("invalid HTML mail"))?;
        }
        texts.push((mail.ctype.mimetype.clone(), body));
    }
    Ok(())
}
