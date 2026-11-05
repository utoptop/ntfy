use crate::{
    auth_db::{AuthDb, generate_token},
    config::Config,
    error::Error,
    model::now,
};
use std::{
    collections::{HashMap, HashSet},
    io::{self, IsTerminal, Write},
};

pub fn run(args: &[String]) -> std::result::Result<(), Box<dyn std::error::Error>> {
    let family = args.first().map(String::as_str).unwrap_or("");
    let mut positional = Vec::new();
    let mut options = HashMap::new();
    let mut switches = HashSet::new();
    let mut i = 1;
    while i < args.len() {
        let arg = &args[i];
        if !arg.starts_with('-') || arg == "-" {
            positional.push(arg.clone());
            i += 1;
            continue;
        }
        let (raw, inline) = arg
            .split_once('=')
            .map(|(k, v)| (k, Some(v.to_owned())))
            .unwrap_or((arg.as_str(), None));
        let key = match raw {
            "-c" => "config",
            "-H" => "auth-file",
            "-r" if family == "access" => "reset",
            "-r" => "role",
            "-e" => "expires",
            "-l" => "label",
            "-h" => "help",
            _ => raw.trim_start_matches('-'),
        };
        if matches!(key, "help" | "reset" | "ignore-exists") {
            switches.insert(key.to_owned());
            i += 1;
            continue;
        }
        if !matches!(
            key,
            "config" | "auth-file" | "role" | "expires" | "label" | "auth-default-access"
        ) {
            return Err(format!("unknown option {raw}").into());
        }
        let value = match inline {
            Some(value) => value,
            None => {
                i += 1;
                args.get(i).cloned().ok_or("option value required")?
            }
        };
        options.insert(key.to_owned(), value);
        i += 1;
    }
    if switches.contains("help") {
        println!(
            "Usage:\n  ntfy user [--config FILE] list\n  ntfy user add [--role=admin|user] USERNAME\n  ntfy user remove USERNAME\n  ntfy user change-pass USERNAME\n  ntfy user change-role USERNAME admin|user\n  ntfy user hash\n  ntfy access [--reset] [USERNAME [TOPIC [PERMISSION]]]\n  ntfy token add [--expires=72h] [--label=LABEL] USERNAME\n  ntfy token remove USERNAME TOKEN\n  ntfy token list [USERNAME]\n  ntfy token generate\n\nUse NTFY_PASSWORD or NTFY_PASSWORD_HASH for scripted password input."
        );
        return Ok(());
    }
    let command = positional.first().map(String::as_str).unwrap_or("list");
    if family == "token" && command == "generate" {
        println!("{}", generate_token());
        return Ok(());
    }
    if family == "user" && command == "hash" {
        println!("{}", password_hash()?);
        return Ok(());
    }
    let path = options
        .get("config")
        .cloned()
        .or_else(|| std::env::var("NTFY_CONFIG_FILE").ok());
    let overrides = options
        .iter()
        .filter(|(k, _)| matches!(k.as_str(), "auth-file" | "auth-default-access"))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let mut config = Config::load_with_overrides(path.as_deref(), &overrides)?;
    if let Some(file) = options
        .get("auth-file")
        .cloned()
        .or_else(|| std::env::var("NTFY_AUTH_FILE").ok())
    {
        config.auth_file = file;
    }
    if let Some(access) = options.get("auth-default-access") {
        config.auth_default_access = access.clone();
    }
    if config.auth_file.is_empty() {
        return Err("this command requires auth-file or NTFY_AUTH_FILE".into());
    }
    let db = AuthDb::open(&config)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let arg = |index: usize| {
            positional
                .get(index)
                .cloned()
                .ok_or_else(|| Error::bad("missing command argument"))
        };
        let find = |name: String| {
            let db = db.clone();
            async move {
                db.user(&name)
                    .await?
                    .ok_or_else(|| Error::bad("user not found"))
            }
        };
        match family {
            "user" => match command {
                "list" | "l" => {
                    for value in db.users().await? {
                        println!(
                            "user {} (role: {})",
                            value["username"].as_str().unwrap_or(""),
                            value["role"].as_str().unwrap_or("")
                        );
                    }
                }
                "add" | "a" => {
                    let name = arg(1)?;
                    if switches.contains("ignore-exists") && db.user(&name).await?.is_some() {
                        return Ok(());
                    }
                    let hash = password_hash().map_err(|e| Error::bad(e.to_string()))?;
                    db.add_user(
                        &name,
                        &hash,
                        options.get("role").map(String::as_str).unwrap_or("user"),
                    )
                    .await?;
                    println!("user {name} added");
                }
                "remove" | "del" | "rm" => {
                    let user = find(arg(1)?).await?;
                    db.remove_user(&user.id, false).await?;
                    println!("user {} removed", user.name);
                }
                "change-pass" | "chp" => {
                    let user = find(arg(1)?).await?;
                    let hash = password_hash().map_err(|e| Error::bad(e.to_string()))?;
                    db.change_password(&user.id, hash).await?;
                    println!("password changed for user {}", user.name);
                }
                "change-role" | "chr" => {
                    let user = find(arg(1)?).await?;
                    let role = arg(2)?;
                    db.change_role(&user.id, &role).await?;
                    println!("role changed for user {}", user.name);
                }
                _ => return Err(Error::bad("unknown user subcommand")),
            },
            "access" => {
                let name = positional.first().map(|v| {
                    if v == "everyone" {
                        "*".to_owned()
                    } else {
                        v.clone()
                    }
                });
                let topic = positional.get(1).cloned();
                if switches.contains("reset") {
                    if positional.len() > 2 {
                        return Err(Error::bad("too many arguments"));
                    }
                    db.reset_access(name, topic).await?;
                } else if let Some(access) = positional.get(2) {
                    db.set_access(name.as_deref().unwrap(), topic.as_deref().unwrap(), access)
                        .await?;
                } else {
                    for row in db.list_access(name).await? {
                        println!("{}", row);
                    }
                }
            }
            "token" => match command {
                "add" | "a" => {
                    let user = find(arg(1)?).await?;
                    let expires = options
                        .get("expires")
                        .map(|v| {
                            humantime::parse_duration(v).map(|d| {
                                now().saturating_add(d.as_secs().min(i64::MAX as u64) as i64)
                            })
                        })
                        .transpose()
                        .map_err(|_| Error::bad("invalid token duration"))?
                        .unwrap_or(0);
                    let token = db
                        .create_token(
                            &user.id,
                            options.get("label").map(String::as_str).unwrap_or(""),
                            expires,
                            "0.0.0.0",
                        )
                        .await?;
                    println!("token {} created for user {}", token.token, user.name);
                }
                "remove" | "del" | "rm" => {
                    let user = find(arg(1)?).await?;
                    db.delete_token(&user.id, &arg(2)?).await?;
                    println!("token removed for user {}", user.name);
                }
                "list" | "l" => {
                    let names = if let Some(name) = positional.get(1) {
                        vec![name.clone()]
                    } else {
                        db.users()
                            .await?
                            .iter()
                            .filter_map(|u| {
                                u["username"]
                                    .as_str()
                                    .filter(|name| *name != "*")
                                    .map(str::to_owned)
                            })
                            .collect()
                    };
                    for name in names {
                        let user = find(name).await?;
                        for token in db.tokens(&user.id).await? {
                            println!(
                                "{}",
                                serde_json::to_string(&token).map_err(Error::internal)?
                            );
                        }
                    }
                }
                _ => return Err(Error::bad("unknown token subcommand")),
            },
            _ => return Err(Error::bad("unknown administrative command")),
        }
        Ok::<(), Error>(())
    })?;
    Ok(())
}
fn password_hash() -> std::result::Result<String, Box<dyn std::error::Error>> {
    if let Ok(hash) = std::env::var("NTFY_PASSWORD_HASH") {
        bcrypt::HashParts::from_str(&hash)?;
        return Ok(hash);
    }
    let password = if let Ok(password) = std::env::var("NTFY_PASSWORD") {
        password
    } else if io::stdin().is_terminal() {
        let password = rpassword::prompt_password("Password: ")?;
        if password != rpassword::prompt_password("Confirm password: ")? {
            return Err("passwords do not match".into());
        }
        password
    } else {
        let mut password = String::new();
        io::stderr().write_all(b"Password: ")?;
        io::stdin().read_line(&mut password)?;
        password.trim_end_matches(['\r', '\n']).to_owned()
    };
    if password.is_empty() || password.len() > 72 {
        return Err("password must be 1-72 bytes".into());
    }
    Ok(bcrypt::hash(password, 10)?)
}
use std::str::FromStr;
