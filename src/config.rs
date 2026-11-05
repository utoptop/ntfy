use serde::{Deserialize, Serialize};
use std::{net::SocketAddr, path::PathBuf, time::Duration};

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct User {
    pub name: String,
    #[serde(default)]
    pub password_hash: String,
    #[serde(default)]
    pub token_hashes: Vec<String>,
    #[serde(default)]
    pub admin: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Access {
    pub user: String,
    pub topic: String,
    pub permission: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, rename_all = "kebab-case", deny_unknown_fields)]
pub struct Config {
    pub listen_http: String,
    pub base_url: String,
    pub cache_file: PathBuf,
    pub cache_duration: String,
    pub cache_max_messages: usize,
    pub attachment_cache_dir: PathBuf,
    pub attachment_file_size_limit: usize,
    pub attachment_total_size_limit: u64,
    pub attachment_expiry_duration: String,
    pub message_size_limit: usize,
    pub message_delay_max: String,
    pub keepalive_interval: String,
    pub subscriber_buffer_size: usize,
    pub subscription_limit: usize,
    pub visitor_subscription_limit: usize,
    pub visitor_request_limit_burst: usize,
    pub visitor_request_limit_replenish: String,
    pub visitor_limit: usize,
    pub topic_limit: usize,
    pub replay_limit: usize,
    pub replay_bytes_limit: usize,
    pub database_queue_size: usize,
    pub request_limit: usize,
    pub request_timeout: String,
    pub auth_default_access: String,
    pub auth_users: Vec<User>,
    pub auth_access: Vec<Access>,
    pub web_root: PathBuf,
    pub log_level: String,
    pub worker_threads: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            listen_http: "0.0.0.0:8080".into(),
            base_url: String::new(),
            cache_file: "data/cache.db".into(),
            cache_duration: "12h".into(),
            cache_max_messages: 100_000,
            attachment_cache_dir: "data/attachments".into(),
            attachment_file_size_limit: 15 * 1024 * 1024,
            attachment_total_size_limit: 1024 * 1024 * 1024,
            attachment_expiry_duration: "3h".into(),
            message_size_limit: 4096,
            message_delay_max: "3d".into(),
            keepalive_interval: "45s".into(),
            subscriber_buffer_size: 256,
            subscription_limit: 10_000,
            visitor_subscription_limit: 30,
            visitor_request_limit_burst: 60,
            visitor_request_limit_replenish: "5s".into(),
            visitor_limit: 10_000,
            topic_limit: 10_000,
            replay_limit: 1000,
            replay_bytes_limit: 1024 * 1024,
            database_queue_size: 256,
            request_limit: 256,
            request_timeout: "30s".into(),
            auth_default_access: "read-write".into(),
            auth_users: Vec::new(),
            auth_access: Vec::new(),
            web_root: "web/build".into(),
            log_level: "info".into(),
            worker_threads: 2,
        }
    }
}

impl Config {
    pub fn load(path: Option<&str>) -> std::result::Result<Self, Box<dyn std::error::Error>> {
        let default_path = "/etc/ntfy/server.yml";
        let mut config = if path.is_some() || std::path::Path::new(default_path).exists() {
            serde_yaml::from_str(&std::fs::read_to_string(path.unwrap_or(default_path))?)?
        } else {
            Self::default()
        };
        // Environment variables override YAML; arrays use JSON to avoid ambiguous delimiters.
        let mut value = serde_json::to_value(&config)?;
        for (key, value) in value.as_object_mut().unwrap() {
            let env = format!("NTFY_{}", key.replace('-', "_").to_uppercase());
            if let Ok(raw) = std::env::var(env) {
                *value = if value.is_string() {
                    raw.into()
                } else {
                    serde_json::from_str(&raw)?
                };
            }
        }
        config = serde_json::from_value(value)?;
        config.validate()?;
        Ok(config)
    }
    pub fn duration(value: &str) -> Duration {
        humantime::parse_duration(value).expect("validated duration")
    }
    pub fn address(&self) -> std::result::Result<SocketAddr, std::net::AddrParseError> {
        if self.listen_http.starts_with(':') {
            format!("0.0.0.0{}", self.listen_http).parse()
        } else {
            self.listen_http.parse()
        }
    }
    pub fn validate(&self) -> std::result::Result<(), Box<dyn std::error::Error>> {
        self.address()?;
        for value in [
            &self.cache_duration,
            &self.attachment_expiry_duration,
            &self.message_delay_max,
            &self.keepalive_interval,
            &self.visitor_request_limit_replenish,
            &self.request_timeout,
        ] {
            let duration = humantime::parse_duration(value)?;
            if duration.as_secs() == 0 || duration.as_secs() > 365 * 86400 {
                return Err("durations must be between 1 second and 365 days".into());
            }
        }
        for size in [
            self.cache_max_messages,
            self.message_size_limit,
            self.subscriber_buffer_size,
            self.subscription_limit,
            self.visitor_subscription_limit,
            self.visitor_request_limit_burst,
            self.visitor_limit,
            self.topic_limit,
            self.replay_limit,
            self.replay_bytes_limit,
            self.database_queue_size,
            self.request_limit,
            self.worker_threads,
        ] {
            if size == 0 || size > 10_000_000 {
                return Err("resource limits must be between 1 and 10000000".into());
            }
        }
        if self.message_size_limit > 64 * 1024
            || self.attachment_file_size_limit > 1024 * 1024 * 1024
            || self.worker_threads > 128
            || self.subscriber_buffer_size > 65_536
        {
            return Err("resource limit exceeds safe maximum".into());
        }
        if !self.base_url.is_empty() {
            let u = url::Url::parse(&self.base_url)?;
            if !matches!(u.scheme(), "http" | "https")
                || u.host_str().is_none()
                || !u.username().is_empty()
                || u.password().is_some()
                || u.query().is_some()
                || u.fragment().is_some()
                || u.path() != "/"
            {
                return Err(
                    "base-url must be an http(s) origin without credentials or a path".into(),
                );
            }
        }
        if !valid_permission(&self.auth_default_access) {
            return Err("invalid auth-default-access".into());
        }
        let mut names = std::collections::HashSet::new();
        for user in &self.auth_users {
            if user.name.is_empty() || user.name == "*" || !names.insert(&user.name) {
                return Err("user names must be nonempty and unique".into());
            }
            if !user.password_hash.is_empty() {
                argon2::PasswordHash::new(&user.password_hash)
                    .map_err(|_| "invalid Argon2 password hash")?;
            }
            for hash in &user.token_hashes {
                if hash.len() != 64
                    || !hash
                        .bytes()
                        .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
                {
                    return Err("token hashes must be lowercase SHA-256 hex".into());
                }
            }
        }
        for acl in &self.auth_access {
            if !valid_permission(&acl.permission)
                || acl.topic.is_empty()
                || acl.topic.len() > 64
                || !acl
                    .topic
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_-*".contains(&b))
            {
                return Err("invalid access rule".into());
            }
            if acl.user != "*" && !names.contains(&acl.user) {
                return Err("access rule refers to unknown user".into());
            }
        }
        Ok(())
    }
}

pub fn valid_permission(value: &str) -> bool {
    matches!(
        value,
        "read-write" | "read-only" | "write-only" | "deny-all"
    )
}
