use ntfy_rs::{config::Config, http::App};
use std::io::Read;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().is_some_and(|s| s == "webpush") && args.get(1).is_some_and(|s| s == "keys") {
        let (private, public) = ntfy_rs::webpush::keys();
        let content = format!("web-push-public-key: {public}\nweb-push-private-key: {private}\n");
        if args.len() == 2 {
            print!("{content}");
        } else if args.len() == 4 && matches!(args[2].as_str(), "--output-file" | "-f") {
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            use std::io::Write;
            options.open(&args[3])?.write_all(content.as_bytes())?;
        } else {
            return Err("usage: ntfy webpush keys [--output-file FILE]".into());
        }
        return Ok(());
    }
    if matches!(
        args.first().map(String::as_str),
        Some("user" | "access" | "token")
    ) {
        return ntfy_rs::admin::run(&args);
    }
    match args.first().map(String::as_str).unwrap_or("serve") {
        "--version" | "version" => {
            println!("ntfy-rs {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        "--help" | "help" | "-h" => {
            println!(
                "ntfy-rs {}\n\nUsage:\n  ntfy serve [--config /etc/ntfy/server.yml]\n  ntfy user add [--role=admin] USERNAME\n  ntfy user list / change-pass / change-role / remove\n  ntfy access [USER [TOPIC [PERMISSION]]]\n  ntfy token add / list / remove / generate\n  ntfy webpush keys [--output-file FILE]\n  ntfy hash-password   # read password from stdin, write Argon2id hash\n  ntfy token-hash      # read access token from stdin, write SHA-256 hash\n  ntfy healthcheck\n\nConfiguration: YAML, NTFY_* environment variables, then --field=value CLI overrides. See README.md.",
                env!("CARGO_PKG_VERSION")
            );
            return Ok(());
        }
        "hash-password" | "token-hash" => {
            let mut secret = String::new();
            std::io::stdin().take(4097).read_to_string(&mut secret)?;
            let secret = secret.trim_end_matches(['\n', '\r']);
            if secret.is_empty() || secret.len() > 4096 {
                return Err("secret must be 1-4096 bytes".into());
            }
            if args[0] == "token-hash" {
                println!("{}", ntfy_rs::auth::token_hash(secret));
            } else {
                use argon2::{
                    Argon2, PasswordHasher,
                    password_hash::{SaltString, rand_core::OsRng},
                };
                let salt = SaltString::generate(&mut OsRng);
                println!(
                    "{}",
                    Argon2::default()
                        .hash_password(secret.as_bytes(), &salt)
                        .map_err(|e| e.to_string())?
                );
            }
            return Ok(());
        }
        "serve" | "healthcheck" => {}
        _ => return Err("unknown command; run ntfy --help".into()),
    }
    let mut path = None::<String>;
    let mut overrides = std::collections::HashMap::new();
    let known = serde_json::to_value(Config::default())?;
    let mut i = 1;
    while i < args.len() {
        let (raw, inline) = args[i]
            .split_once('=')
            .map(|(k, v)| (k, Some(v.to_owned())))
            .unwrap_or((&args[i], None));
        let key = match raw {
            "-c" => "config",
            "-l" => "listen-http",
            "-B" => "base-url",
            "-P" => "behind-proxy",
            _ => raw.trim_start_matches('-'),
        }
        .replace('_', "-");
        if key != "config" && known.get(&key).is_none() {
            return Err(format!("unknown option: {raw}").into());
        }
        let boolean = known.get(&key).is_some_and(serde_json::Value::is_boolean);
        let value = if let Some(value) = inline {
            value
        } else if boolean && args.get(i + 1).is_none_or(|v| v.starts_with('-')) {
            "true".into()
        } else {
            i += 1;
            args.get(i).cloned().ok_or("option requires a value")?
        };
        if key == "config" {
            path = Some(value);
        } else {
            overrides.insert(key, value);
        }
        i += 1;
    }
    let config = Config::load_with_overrides(path.as_deref(), &overrides)?;
    if args.first().is_some_and(|s| s == "healthcheck") {
        use std::io::{BufRead, Write};
        let mut address = config.address()?;
        if address.ip().is_unspecified() {
            address.set_ip("127.0.0.1".parse()?);
        }
        let mut stream =
            std::net::TcpStream::connect_timeout(&address, std::time::Duration::from_secs(3))?;
        stream.set_read_timeout(Some(std::time::Duration::from_secs(3)))?;
        stream.write_all(
            b"GET /v1/health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
        )?;
        let mut status = String::new();
        std::io::BufReader::new(stream).read_line(&mut status)?;
        if !status.starts_with("HTTP/1.1 200 ") {
            return Err("health check failed".into());
        }
        return Ok(());
    }
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| config.log_level.clone().into()),
        )
        .with_target(false)
        .init();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(config.worker_threads)
        .max_blocking_threads(8)
        .enable_all()
        .build()?;
    runtime.block_on(async move {
        let address = config.address()?;
        let app = App::new(config)?;
        let maintenance = app.start_maintenance();
        let smtp = if !app.config.smtp_server_listen.is_empty() {
            let listener = tokio::net::TcpListener::bind(ntfy_rs::smtp::address(
                &app.config.smtp_server_listen,
            )?)
            .await?;
            Some(ntfy_rs::smtp::serve(app.clone(), listener))
        } else {
            None
        };
        let listener = tokio::net::TcpListener::bind(address).await?;
        tracing::info!(%address, version=env!("CARGO_PKG_VERSION"), "ntfy-rs listening");
        let shutdown = app.shutdown.clone();
        let signal = async move {
            #[cfg(unix)]
            {
                let mut terminate =
                    tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                        .expect("register SIGTERM");
                tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = terminate.recv() => {} }
            }
            #[cfg(not(unix))]
            let _ = tokio::signal::ctrl_c().await;
            shutdown.cancel();
        };
        use std::future::IntoFuture;
        let server = axum::serve(
            listener,
            app.router()
                .into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .with_graceful_shutdown(app.shutdown.clone().cancelled_owned())
        .into_future();
        tokio::pin!(server);
        tokio::select! {
            result = &mut server => { result?; app.shutdown.cancel(); },
            _ = signal => {
                match tokio::time::timeout(std::time::Duration::from_secs(10), &mut server).await {
                    Ok(result) => result?,
                    Err(_) => tracing::warn!("graceful shutdown deadline reached"),
                }
            }
        }
        let _ = maintenance.await;
        if let Some(smtp) = smtp {
            let _ = smtp.await;
        }
        Ok::<(), Box<dyn std::error::Error>>(())
    })
}
