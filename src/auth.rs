use crate::{
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
    password_slots: Arc<Semaphore>,
}
#[derive(Clone, Default)]
pub struct Identity {
    pub name: Option<String>,
    pub admin: bool,
}

impl Auth {
    pub fn new(config: Arc<Config>) -> Self {
        Self {
            config,
            password_slots: Arc::new(Semaphore::new(2)),
        }
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
            return self.token(value.trim());
        }
        if !scheme.eq_ignore_ascii_case("basic") {
            return Err(unauthorized());
        }
        let decoded = STANDARD.decode(value.trim()).map_err(|_| unauthorized())?;
        let decoded = String::from_utf8(decoded).map_err(|_| unauthorized())?;
        let (username, password) = decoded.split_once(':').ok_or_else(unauthorized)?;
        if username.is_empty() {
            return self.token(password);
        }
        let user = self
            .config
            .auth_users
            .iter()
            .find(|u| u.name == username)
            .cloned()
            .ok_or_else(unauthorized)?;
        if user.password_hash.is_empty() {
            return Err(unauthorized());
        }
        let permit = self
            .password_slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| Error::busy())?;
        let password = password.to_string();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let hash = PasswordHash::new(&user.password_hash).map_err(|_| unauthorized())?;
            Argon2::default()
                .verify_password(password.as_bytes(), &hash)
                .map_err(|_| unauthorized())?;
            Ok(Identity {
                name: Some(user.name),
                admin: user.admin,
            })
        })
        .await
        .map_err(Error::internal)?
    }
    fn token(&self, token: &str) -> Result<Identity> {
        if token.is_empty() {
            return Err(unauthorized());
        }
        let hash = token_hash(token);
        for user in &self.config.auth_users {
            for stored in &user.token_hashes {
                if bool::from(stored.as_bytes().ct_eq(hash.as_bytes())) {
                    return Ok(Identity {
                        name: Some(user.name.clone()),
                        admin: user.admin,
                    });
                }
            }
        }
        Err(unauthorized())
    }
    pub fn authorize(&self, identity: &Identity, topic: &str, write: bool) -> Result<()> {
        if identity.admin {
            return Ok(());
        }
        let username = identity.name.as_deref().unwrap_or("*");
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
        if permission == "read-write"
            || (write && permission == "write-only")
            || (!write && permission == "read-only")
        {
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
