use crate::{
    auth_db::AuthDb,
    config::Config,
    error::{Error, Result},
};
use argon2::{Argon2, PasswordHash, PasswordVerifier};
use axum::http::{HeaderMap, StatusCode};
use base64::{
    Engine,
    engine::general_purpose::{STANDARD, URL_SAFE, URL_SAFE_NO_PAD},
};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use subtle::ConstantTimeEq;
use tokio::sync::Semaphore;

pub fn token_hash(token: &str) -> String {
    format!("{:x}", Sha256::digest(token.as_bytes()))
}
pub struct Auth {
    config: Arc<Config>,
    pub database: Option<AuthDb>,
    password_slots: Arc<Semaphore>,
}
#[derive(Clone, Default)]
pub struct Identity {
    pub name: Option<String>,
    pub admin: bool,
    pub id: Option<String>,
    pub token: Option<String>,
}

impl Auth {
    pub fn new(config: Arc<Config>) -> Result<Self> {
        let database = if config.auth_file.is_empty() {
            None
        } else {
            Some(AuthDb::open_server(&config)?)
        };
        Ok(Self {
            database,
            config,
            password_slots: Arc::new(Semaphore::new(2)),
        })
    }
    pub async fn authenticate(
        &self,
        headers: &HeaderMap,
        query: &std::collections::HashMap<String, String>,
    ) -> Result<Identity> {
        let header = if let Some(value) = query.get("auth").or_else(|| query.get("authorization")) {
            let decoded = URL_SAFE_NO_PAD
                .decode(value)
                .or_else(|_| URL_SAFE.decode(value))
                .map_err(|_| unauthorized())?;
            String::from_utf8(decoded).map_err(|_| unauthorized())?
        } else {
            headers
                .get("authorization")
                .and_then(|h| h.to_str().ok())
                .unwrap_or("")
                .to_owned()
        };
        if header.is_empty() {
            return Ok(Identity::default());
        }
        let (scheme, value) = header.split_once(' ').ok_or_else(unauthorized)?;
        if scheme.eq_ignore_ascii_case("bearer") {
            return self.token(value.trim()).await;
        }
        if !scheme.eq_ignore_ascii_case("basic") {
            return Err(unauthorized());
        }
        let decoded = STANDARD.decode(value.trim()).map_err(|_| unauthorized())?;
        let decoded = String::from_utf8(decoded).map_err(|_| unauthorized())?;
        let (username, password) = decoded.split_once(':').ok_or_else(unauthorized)?;
        if username.is_empty() {
            return self.token(password).await;
        }
        let record = if let Some(db) = &self.database {
            db.user(username).await?.map(|u| {
                (
                    u.hash,
                    Identity {
                        name: Some(u.name),
                        admin: u.role == "admin",
                        id: Some(u.id),
                        token: None,
                    },
                )
            })
        } else {
            self.config
                .auth_users
                .iter()
                .find(|u| u.name == username)
                .map(|u| {
                    (
                        u.password_hash.clone(),
                        Identity {
                            name: Some(u.name.clone()),
                            admin: u.admin,
                            ..Identity::default()
                        },
                    )
                })
        };
        let permit = self
            .password_slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| Error::busy())?;
        let password = password.to_owned();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let (hash, identity) = record.unwrap_or_else(|| {
                (
                    "$2a$10$YFCQvqQDwIIwnJM1xkAYOeih0dg17UVGanaTStnrSzC8NCWxcLDwy".into(),
                    Identity::default(),
                )
            });
            verify_password(&password, &hash)?;
            if identity.name.is_none() {
                return Err(unauthorized());
            }
            Ok(identity)
        })
        .await
        .map_err(Error::internal)?
    }
    async fn token(&self, token: &str) -> Result<Identity> {
        if token.is_empty() {
            return Err(unauthorized());
        }
        if let Some(db) = &self.database {
            let user = db.user_by_token(token).await?.ok_or_else(unauthorized)?;
            return Ok(Identity {
                name: Some(user.name),
                admin: user.role == "admin",
                id: Some(user.id),
                token: Some(token.to_owned()),
            });
        }
        for record in &self.config.auth_tokens {
            if bool::from(record.token.as_bytes().ct_eq(token.as_bytes())) {
                if let Some(user) = self
                    .config
                    .auth_users
                    .iter()
                    .find(|u| u.name == record.user)
                {
                    return Ok(Identity {
                        name: Some(user.name.clone()),
                        admin: user.admin,
                        token: Some(token.to_owned()),
                        ..Identity::default()
                    });
                }
            }
        }
        let hash = token_hash(token);
        for user in &self.config.auth_users {
            for stored in &user.token_hashes {
                if bool::from(stored.as_bytes().ct_eq(hash.as_bytes())) {
                    return Ok(Identity {
                        name: Some(user.name.clone()),
                        admin: user.admin,
                        token: Some(token.to_owned()),
                        ..Identity::default()
                    });
                }
            }
        }
        Err(unauthorized())
    }
    pub(crate) fn password_permit(&self) -> Result<tokio::sync::OwnedSemaphorePermit> {
        self.password_slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| Error::busy())
    }
    pub async fn hash_password(&self, password: String) -> Result<String> {
        if password.is_empty() || password.len() > 72 {
            return Err(Error::bad("password must be 1-72 bytes"));
        }
        let permit = self
            .password_slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| Error::busy())?;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            bcrypt::hash(password, 10).map_err(Error::internal)
        })
        .await
        .map_err(Error::internal)?
    }
    pub async fn subscription_allowed(&self, identity: &Identity, topics: &[String]) -> Result<()> {
        let Some(db) = &self.database else {
            return Ok(());
        };
        let identity = if let Some(name) = &identity.name {
            let user = match &identity.token {
                Some(token) => db.user_by_token(token).await?,
                None => db.user(name).await?,
            }
            .ok_or_else(unauthorized)?;
            Identity {
                name: Some(user.name),
                id: Some(user.id),
                admin: user.role == "admin",
                token: identity.token.clone(),
            }
        } else {
            identity.clone()
        };
        for topic in topics {
            self.authorize(&identity, topic, false).await?;
        }
        Ok(())
    }
    pub async fn authorize(&self, identity: &Identity, topic: &str, write: bool) -> Result<()> {
        if identity.admin {
            return Ok(());
        }
        let username = identity.name.as_deref().unwrap_or("*");
        if let Some(db) = &self.database {
            if let Some((read, allowed_write)) = db.access(username, topic).await? {
                if (write && allowed_write) || (!write && read) {
                    return Ok(());
                }
                return Err(Error::new(
                    if identity.name.is_some() {
                        StatusCode::FORBIDDEN
                    } else {
                        StatusCode::UNAUTHORIZED
                    },
                    "topic access denied",
                ));
            }
        }

        // User-specific rules override everyone rules; within a class, most specific wins.
        let permission = self
            .config
            .auth_access
            .iter()
            .filter(|a| (a.user == username || a.user == "*") && glob(&a.topic, topic))
            .max_by_key(|a| {
                (
                    a.user != "*",
                    a.topic.bytes().filter(|b| *b != b'*').count(),
                )
            })
            .map(|a| a.permission.as_str())
            .unwrap_or(&self.config.auth_default_access);
        let (read, allowed_write) = crate::auth_db::permission(permission).unwrap_or_default();
        if (write && allowed_write) || (!write && read) {
            return Ok(());
        }
        Err(Error::new(
            if identity.name.is_some() {
                StatusCode::FORBIDDEN
            } else {
                StatusCode::UNAUTHORIZED
            },
            "topic access denied",
        ))
    }
}
fn unauthorized() -> Error {
    Error::new(StatusCode::UNAUTHORIZED, "invalid credentials")
}

// Linear wildcard matcher; only '*' has special meaning.
pub fn glob(pattern: &str, text: &str) -> bool {
    let (p, t) = (pattern.as_bytes(), text.as_bytes());
    let (mut i, mut j, mut star, mut mark) = (0, 0, None, 0);
    while j < t.len() {
        if i < p.len() && p[i] == t[j] {
            i += 1;
            j += 1;
        } else if i < p.len() && p[i] == b'*' {
            star = Some(i);
            i += 1;
            mark = j;
        } else if let Some(s) = star {
            mark += 1;
            j = mark;
            i = s + 1;
        } else {
            return false;
        }
    }
    while i < p.len() && p[i] == b'*' {
        i += 1;
    }
    i == p.len()
}

pub fn verify_password(password: &str, hash: &str) -> Result<()> {
    if hash.starts_with("$2") {
        if password.len() > 72 || !bcrypt::verify(password, hash).map_err(|_| unauthorized())? {
            return Err(unauthorized());
        }
        Ok(())
    } else {
        let hash = PasswordHash::new(hash).map_err(|_| unauthorized())?;
        Argon2::default()
            .verify_password(password.as_bytes(), &hash)
            .map_err(|_| unauthorized())
    }
}
