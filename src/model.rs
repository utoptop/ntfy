use crate::error::{Error, Result};
use bytes::Bytes;
use rand::{Rng, distributions::Alphanumeric};
use serde::{Deserialize, Serialize};
use std::{
    sync::OnceLock,
    time::{SystemTime, UNIX_EPOCH},
};

pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
pub fn id() -> String {
    rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(12)
        .map(char::from)
        .collect()
}
fn is_false(value: &bool) -> bool {
    !*value
}
pub fn zero(n: &i64) -> bool {
    *n == 0
}
pub fn zero_u8(n: &u8) -> bool {
    *n == 0
}
pub const RESERVED: &[&str] = &[
    "docs",
    "static",
    "file",
    "app",
    "account",
    "settings",
    "signup",
    "login",
    "v1",
    "metrics",
    "health",
    "reset-password",
];
pub fn topic_valid(topic: &str) -> Result<()> {
    if topic.is_empty()
        || topic.len() > 64
        || !topic
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        || RESERVED.contains(&topic)
    {
        return Err(Error::bad(
            "invalid or reserved topic (use 1-64 ASCII letters, digits, _ or -)",
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Attachment {
    #[serde(default, skip_serializing_if = "is_false")]
    pub deleted: bool,
    pub name: String,
    #[serde(rename = "type", default, skip_serializing_if = "String::is_empty")]
    pub content_type: String,
    #[serde(default, skip_serializing_if = "zero")]
    pub size: i64,
    #[serde(default, skip_serializing_if = "zero")]
    pub expires: i64,
    pub url: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Message {
    #[serde(skip)]
    pub suppress_upstream: bool,
    pub id: String,
    pub time: i64,
    pub event: String,
    pub topic: String,
    #[serde(default, skip_serializing_if = "zero")]
    pub expires: i64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sequence_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub message: String,
    #[serde(default, skip_serializing_if = "zero_u8")]
    pub priority: u8,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub click: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub icon: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachment: Option<Attachment>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub content_type: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub encoding: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub poll_id: String,
}

impl Message {
    pub fn event(topic: impl Into<String>, event: &str) -> Self {
        Self {
            id: id(),
            time: now(),
            topic: topic.into(),
            event: event.into(),
            ..Self::default()
        }
    }
}

// Each wire format is generated at most once per message, shared across every receiver.
pub struct Envelope {
    pub seq: i64,
    pub cached: bool,
    pub message: Message,
    pub json: Bytes,
    ndjson: OnceLock<Bytes>,
    sse: OnceLock<Bytes>,
    raw: OnceLock<Bytes>,
}

impl Envelope {
    pub fn new(seq: i64, cached: bool, message: Message) -> Self {
        let json = Bytes::from(serde_json::to_vec(&message).expect("serializable message"));
        Self {
            seq,
            cached,
            message,
            json,
            ndjson: OnceLock::new(),
            sse: OnceLock::new(),
            raw: OnceLock::new(),
        }
    }
    pub fn wire(&self, format: &str) -> Bytes {
        match format {
            "sse" => self
                .sse
                .get_or_init(|| {
                    let prefix = if matches!(
                        self.message.event.as_str(),
                        "message" | "message_delete" | "message_clear"
                    ) {
                        String::new()
                    } else {
                        format!("event: {}\n", self.message.event)
                    };
                    Bytes::from(format!(
                        "{prefix}data: {}\n\n",
                        std::str::from_utf8(&self.json).unwrap()
                    ))
                })
                .clone(),
            "raw" => self
                .raw
                .get_or_init(|| {
                    Bytes::from(if self.message.event == "message" {
                        format!("{}\n", self.message.message.replace(['\n', '\r'], " "))
                    } else {
                        "\n".into()
                    })
                })
                .clone(),
            "ws" => self.json.clone(),
            _ => self
                .ndjson
                .get_or_init(|| {
                    let mut b = self.json.to_vec();
                    b.push(b'\n');
                    Bytes::from(b)
                })
                .clone(),
        }
    }
}
