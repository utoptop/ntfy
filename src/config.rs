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
    pub behind_proxy: bool,
    pub proxy_forwarded_header: String,
    pub proxy_trusted_hosts: String,
    #[serde(skip)]
    pub proxy_trusted_prefixes: Vec<ipnet::IpNet>,
    pub cache_file: PathBuf,
    pub cache_duration: String,
    pub cache_max_messages: usize,
    pub attachment_cache_dir: PathBuf,
    #[serde(deserialize_with = "size_usize")]
    pub attachment_file_size_limit: usize,
    #[serde(deserialize_with = "size_u64")]
    pub attachment_total_size_limit: u64,
    pub attachment_expiry_duration: String,
    #[serde(deserialize_with = "size_usize")]
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
    pub auth_file: String,
    pub enable_login: bool,
    pub enable_signup: bool,
    pub require_login: bool,
    pub enable_reservations: bool,
    pub smtp_sender_addr: String,
    pub smtp_sender_from: String,
    pub smtp_sender_user: String,
    pub smtp_sender_pass: String,
    pub smtp_sender_verify: bool,
    pub smtp_server_listen: String,
    pub smtp_server_domain: String,
    pub smtp_server_addr_prefix: String,
    pub visitor_email_limit_burst: usize,
    pub visitor_email_limit_replenish: String,
    pub web_push_public_key: String,
    pub web_push_private_key: String,
    pub web_push_file: String,
    pub web_push_email_address: String,
    pub web_push_startup_queries: String,
    pub web_push_expiry_warning_duration: String,
    pub web_push_expiry_duration: String,
    pub upstream_base_url: String,
    pub upstream_access_token: String,
    #[serde(deserialize_with = "users")]
    pub auth_users: Vec<User>,
    #[serde(deserialize_with = "access")]
    pub auth_access: Vec<Access>,
    #[serde(deserialize_with = "tokens")]
    pub auth_tokens: Vec<AuthToken>,
    pub web_root: PathBuf,
    pub log_level: String,
    pub worker_threads: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            listen_http: "0.0.0.0:8080".into(),
            base_url: String::new(),
            behind_proxy: false,
            proxy_forwarded_header: "X-Forwarded-For".into(),
            proxy_trusted_hosts: String::new(),
            proxy_trusted_prefixes: Vec::new(),
            cache_file: if cfg!(feature = "container") {
                "/var/lib/ntfy/cache.db"
            } else {
                "data/cache.db"
            }
            .into(),
            cache_duration: "12h".into(),
            cache_max_messages: 100_000,
            attachment_cache_dir: if cfg!(feature = "container") {
                "/var/lib/ntfy/attachments"
            } else {
                "data/attachments"
            }
            .into(),
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
            auth_file: String::new(),
            enable_login: false,
            enable_signup: false,
            require_login: false,
            enable_reservations: false,
            smtp_sender_addr: String::new(),
            smtp_sender_from: String::new(),
            smtp_sender_user: String::new(),
            smtp_sender_pass: String::new(),
            smtp_sender_verify: false,
            smtp_server_listen: String::new(),
            smtp_server_domain: String::new(),
            smtp_server_addr_prefix: String::new(),
            visitor_email_limit_burst: 16,
            visitor_email_limit_replenish: "1h".into(),
            web_push_public_key: String::new(),
            web_push_private_key: String::new(),
            web_push_file: String::new(),
            web_push_email_address: String::new(),
            web_push_startup_queries: String::new(),
            web_push_expiry_warning_duration: "55d".into(),
            web_push_expiry_duration: "60d".into(),
            upstream_base_url: String::new(),
            upstream_access_token: String::new(),
            auth_users: Vec::new(),
            auth_access: Vec::new(),
            auth_tokens: Vec::new(),
            web_root: if cfg!(feature = "container") {
                "/usr/share/ntfy/web"
            } else {
                "web/build"
            }
            .into(),
            log_level: "info".into(),
            worker_threads: 2,
        }
    }
}

impl Config {
    pub fn load(path: Option<&str>) -> std::result::Result<Self, Box<dyn std::error::Error>> {
        Self::load_with_overrides(path, &std::collections::HashMap::new())
    }
    pub fn load_with_overrides(
        path: Option<&str>,
        overrides: &std::collections::HashMap<String, String>,
    ) -> std::result::Result<Self, Box<dyn std::error::Error>> {
        let env_path = std::env::var("NTFY_CONFIG_FILE").ok();
        let path = path.or(env_path.as_deref());
        let default_path = "/etc/ntfy/server.yml";
        let mut config = if path.is_some() || std::path::Path::new(default_path).exists() {
            let mut value: serde_yaml::Value =
                serde_yaml::from_str(&std::fs::read_to_string(path.unwrap_or(default_path))?)?;
            if let Some(mapping) = value.as_mapping_mut() {
                let original = std::mem::take(mapping);
                for (key, value) in original {
                    if value.is_null() {
                        continue;
                    }
                    let key = key
                        .as_str()
                        .map(|s| serde_yaml::Value::String(s.replace('_', "-")))
                        .unwrap_or(key);
                    if mapping.insert(key, value).is_some() {
                        return Err("duplicate configuration key".into());
                    }
                }
            }
            serde_yaml::from_value(value)?
        } else {
            Self::default()
        };
        // Command-line options take precedence over environment variables and YAML.
        let mut value = serde_json::to_value(&config)?;
        for key in overrides.keys() {
            if !value.as_object().unwrap().contains_key(key) {
                return Err(format!("unknown option: {key}").into());
            }
        }
        for (key, value) in value.as_object_mut().unwrap() {
            let env = format!("NTFY_{}", key.replace('-', "_").to_uppercase());
            if let Some(raw) = overrides
                .get(key)
                .cloned()
                .or_else(|| std::env::var(env).ok())
            {
                *value = if value.is_string() {
                    raw.into()
                } else if value.is_array() && !raw.trim_start().starts_with('[') {
                    raw.split(',')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(|s| serde_json::Value::String(s.into()))
                        .collect()
                } else {
                    serde_json::from_str(&raw).unwrap_or_else(|_| raw.into())
                };
            }
        }
        config = serde_json::from_value(value)?;
        config.validate()?;
        Ok(config)
    }
    pub fn resolve_proxy_hosts(&mut self) -> std::result::Result<(), Box<dyn std::error::Error>> {
        use std::net::ToSocketAddrs;
        self.proxy_trusted_prefixes.clear();
        for host in self
            .proxy_trusted_hosts
            .split(',')
            .map(str::trim)
            .filter(|h| !h.is_empty())
        {
            if let Ok(network) = host.parse::<ipnet::IpNet>() {
                self.proxy_trusted_prefixes.push(network);
            } else if let Ok(ip) = host.parse::<std::net::IpAddr>() {
                self.proxy_trusted_prefixes.push(ip.into());
            } else {
                let addresses: Vec<_> = (host, 0).to_socket_addrs()?.collect();
                if addresses.is_empty() {
                    return Err("proxy-trusted-hosts hostname resolved to no addresses".into());
                }
                for address in addresses {
                    self.proxy_trusted_prefixes.push(address.ip().into());
                }
            }
        }
        Ok(())
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
        axum::http::HeaderName::from_bytes(self.proxy_forwarded_header.as_bytes())
            .map_err(|_| "proxy-forwarded-header must be a valid HTTP header name")?;
        for value in [
            &self.cache_duration,
            &self.visitor_email_limit_replenish,
            &self.web_push_expiry_duration,
            &self.web_push_expiry_warning_duration,
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
            self.visitor_email_limit_burst,
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
        if (self.enable_login
            || self.enable_signup
            || self.enable_reservations
            || self.require_login)
            && self.auth_file.is_empty()
        {
            return Err("account features require auth-file".into());
        }
        if !self.upstream_base_url.is_empty() {
            let u = url::Url::parse(&self.upstream_base_url)?;
            if !matches!(u.scheme(), "http" | "https")
                || u.host_str().is_none()
                || !u.username().is_empty()
                || u.password().is_some()
                || u.query().is_some()
                || u.fragment().is_some()
                || self.base_url.is_empty()
            {
                return Err("upstream-base-url requires a valid HTTP(S) URL and base-url".into());
            }
        }
        if !self.smtp_server_listen.is_empty() {
            crate::smtp::address(&self.smtp_server_listen)?;
            if self.smtp_server_domain.is_empty()
                || self
                    .smtp_server_domain
                    .bytes()
                    .any(|b| b.is_ascii_whitespace() || b == b'@')
            {
                return Err("smtp-server-domain is required and must be a domain name".into());
            }
        }
        if self.smtp_sender_verify && self.auth_file.is_empty() {
            return Err("smtp-sender-verify requires auth-file".into());
        }
        crate::webpush::validate_config(self).map_err(|e| e.to_string())?;
        let mut names = std::collections::HashSet::new();
        for user in &self.auth_users {
            if !crate::auth_db::valid_username(&user.name) || !names.insert(&user.name) {
                return Err("user names must be nonempty and unique".into());
            }
            if !user.password_hash.is_empty() {
                if user.password_hash.starts_with("$2") {
                    user.password_hash
                        .parse::<bcrypt::HashParts>()
                        .map_err(|_| "invalid bcrypt password hash")?;
                } else {
                    argon2::PasswordHash::new(&user.password_hash)
                        .map_err(|_| "invalid password hash")?;
                }
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
        let mut token_values = std::collections::HashSet::new();
        for token in &self.auth_tokens {
            if !names.contains(&token.user)
                || token.token.len() != 32
                || !token.token.starts_with("tk_")
                || !token.token[3..]
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
                || !token_values.insert(&token.token)
            {
                return Err(
                    "invalid auth-tokens entry: expected a provisioned user and unique tk_ token"
                        .into(),
                );
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
    crate::auth_db::permission(value).is_some()
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AuthToken {
    pub user: String,
    pub token: String,
    #[serde(default)]
    pub label: String,
}
fn entries<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<serde_json::Value>, D::Error> {
    Vec::<serde_json::Value>::deserialize(d)
}
fn users<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<User>, D::Error> {
    entries(d)?
        .into_iter()
        .map(|v| {
            if let Some(s) = v.as_str() {
                let parts: Vec<_> = s.split(':').map(str::trim).collect();
                if parts.len() != 3 || !matches!(parts[2], "admin" | "user") {
                    return Err(serde::de::Error::custom(
                        "auth-users expects name:hash:role",
                    ));
                }
                Ok(User {
                    name: parts[0].into(),
                    password_hash: parts[1].into(),
                    admin: parts[2] == "admin",
                    ..User::default()
                })
            } else {
                serde_json::from_value(v).map_err(serde::de::Error::custom)
            }
        })
        .collect()
}
fn access<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<Access>, D::Error> {
    entries(d)?
        .into_iter()
        .map(|v| {
            let mut rule: Access = if let Some(s) = v.as_str() {
                let parts: Vec<_> = s.split(':').map(str::trim).collect();
                if parts.len() != 3 {
                    return Err(serde::de::Error::custom(
                        "auth-access expects user:topic:permission",
                    ));
                }
                Access {
                    user: parts[0].into(),
                    topic: parts[1].into(),
                    permission: parts[2].into(),
                }
            } else {
                serde_json::from_value(v).map_err(serde::de::Error::custom)?
            };
            if rule.user == "everyone" {
                rule.user = "*".into();
            }
            Ok(rule)
        })
        .collect()
}
fn tokens<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<AuthToken>, D::Error> {
    entries(d)?
        .into_iter()
        .map(|v| {
            if let Some(s) = v.as_str() {
                let parts: Vec<_> = s.splitn(3, ':').collect();
                if parts.len() < 2 {
                    return Err(serde::de::Error::custom(
                        "auth-tokens expects user:token[:label]",
                    ));
                }
                Ok(AuthToken {
                    user: parts[0].trim().into(),
                    token: parts[1].trim().into(),
                    label: parts.get(2).unwrap_or(&"").to_string(),
                })
            } else {
                serde_json::from_value(v).map_err(serde::de::Error::custom)
            }
        })
        .collect()
}
fn size_u64<'de, D: serde::Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
    let v = serde_json::Value::deserialize(d)?;
    if let Some(n) = v.as_u64() {
        return Ok(n);
    }
    let value = v
        .as_str()
        .ok_or_else(|| serde::de::Error::custom("expected byte size"))?
        .trim()
        .to_ascii_uppercase();
    let split = value
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(value.len());
    let number: u64 = value[..split].parse().map_err(serde::de::Error::custom)?;
    let power = match value[split..].trim() {
        "" | "B" => 0,
        "K" | "KB" | "KIB" => 1,
        "M" | "MB" | "MIB" => 2,
        "G" | "GB" | "GIB" => 3,
        "T" | "TB" | "TIB" => 4,
        _ => return Err(serde::de::Error::custom("invalid byte-size unit")),
    };
    number
        .checked_mul(1024_u64.pow(power))
        .ok_or_else(|| serde::de::Error::custom("byte size overflow"))
}
fn size_usize<'de, D: serde::Deserializer<'de>>(d: D) -> Result<usize, D::Error> {
    size_u64(d)?.try_into().map_err(serde::de::Error::custom)
}
