use crate::{
    error::{Error, Result},
    model::{Message, id, now},
    store::Since,
};
use axum::http::{HeaderMap, Uri};
use serde_json::{Value, json};
use std::collections::HashMap;

pub struct Params {
    pub headers: HeaderMap,
    pub query: HashMap<String, String>,
}
impl Params {
    pub fn new(headers: HeaderMap, uri: &Uri) -> Self {
        Self {
            headers,
            query: url::form_urlencoded::parse(uri.query().unwrap_or("").as_bytes())
                .into_owned()
                .collect(),
        }
    }
    pub fn get(&self, names: &[&str]) -> Option<String> {
        names
            .iter()
            .find_map(|name| {
                self.headers
                    .get(*name)
                    .and_then(|v| std::str::from_utf8(v.as_bytes()).ok())
                    .map(str::to_owned)
            })
            .or_else(|| names.iter().find_map(|name| self.query.get(*name).cloned()))
    }
    pub fn boolean(&self, names: &[&str], default: bool) -> bool {
        self.get(names)
            .map(|v| matches!(v.to_lowercase().as_str(), "1" | "yes" | "true"))
            .unwrap_or(default)
    }
    pub fn since(&self, poll: bool) -> Result<Since> {
        let value = self.get(&["x-since", "since", "si", "last-event-id"]);
        match value.as_deref() {
            None | Some("") => Ok(if poll { Since::All } else { Since::None }),
            Some("all") => Ok(Since::All),
            Some("latest") => Ok(Since::Latest),
            Some("none") => Ok(Since::None),
            Some(value) => {
                if value.len() == 12 && value.bytes().all(|b| b.is_ascii_alphanumeric()) {
                    return Ok(Since::Id(value.into()));
                }
                if let Ok(timestamp) = value.parse::<i64>() {
                    return Ok(Since::Time(timestamp));
                }
                let duration = humantime::parse_duration(value)
                    .map_err(|_| Error::bad("invalid since parameter"))?;
                let seconds = i64::try_from(duration.as_secs())
                    .map_err(|_| Error::bad("since duration too large"))?;
                Ok(Since::Time(now().saturating_sub(seconds)))
            }
        }
    }
}

pub fn priority(value: &str) -> Result<u8> {
    match value.to_lowercase().as_str() {
        "" => Ok(0),
        "1" | "min" => Ok(1),
        "2" | "low" => Ok(2),
        "3" | "default" => Ok(3),
        "4" | "high" => Ok(4),
        "5" | "max" | "urgent" => Ok(5),
        _ => Err(Error::bad("invalid priority")),
    }
}
pub fn comma(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

#[derive(Default)]
pub struct Filter {
    pub id: Option<String>,
    pub title: Option<String>,
    pub message: Option<String>,
    pub priorities: Vec<u8>,
    pub tags: Vec<String>,
}
impl Filter {
    pub fn parse(p: &Params) -> Result<Self> {
        Ok(Self {
            id: p.get(&["x-id", "id"]),
            title: p.get(&["x-title", "title", "t"]),
            message: p.get(&["x-message", "message", "m"]),
            priorities: comma(
                &p.get(&["x-priority", "priority", "prio", "p"])
                    .unwrap_or_default(),
            )
            .iter()
            .map(|v| priority(v))
            .collect::<Result<_>>()?,
            tags: comma(&p.get(&["x-tags", "tags", "tag", "ta"]).unwrap_or_default()),
        })
    }
    pub fn matches(&self, m: &Message) -> bool {
        if m.event != "message" {
            return true;
        }
        self.id.as_ref().is_none_or(|v| v == &m.id)
            && self.title.as_ref().is_none_or(|v| v == &m.title)
            && self.message.as_ref().is_none_or(|v| v == &m.message)
            && (self.priorities.is_empty()
                || self
                    .priorities
                    .contains(&if m.priority == 0 { 3 } else { m.priority }))
            && self.tags.iter().all(|t| m.tags.contains(t))
    }
}

pub fn actions(input: &str) -> Result<Vec<Value>> {
    if input.len() > 4096 {
        return Err(Error::bad("actions too large"));
    }
    let mut actions: Vec<Value> = if input.trim_start().starts_with('[') {
        serde_json::from_str(input)?
    } else {
        let mut result = Vec::new();
        for item in split_quoted(input, ';')? {
            let parts = split_quoted(&item, ',')?;
            if parts.len() < 2 {
                return Err(Error::bad("action requires type and label"));
            }
            let mut action = json!({"action": unquote(&parts[0]), "label": unquote(&parts[1])});
            for (i, part) in parts.iter().enumerate().skip(2) {
                if let Some((key, value)) = part.split_once('=') {
                    let key = key.trim();
                    let value = unquote(value);
                    if key == "clear" {
                        action[key] = json!(value == "true");
                    } else if let Some(name) = key.strip_prefix("headers.") {
                        action["headers"][name] = value.into();
                    } else if let Some(name) = key.strip_prefix("extras.") {
                        action["extras"][name] = value.into();
                    } else {
                        action[key] = value.into();
                    }
                } else if i == 2 {
                    let key = match action["action"].as_str() {
                        Some("copy") => "value",
                        Some("broadcast") => "intent",
                        _ => "url",
                    };
                    action[key] = unquote(part).into();
                } else {
                    return Err(Error::bad("invalid action parameter"));
                }
            }
            result.push(action);
        }
        result
    };
    if actions.len() > 3 {
        return Err(Error::bad("at most 3 actions are supported"));
    }
    for action in &mut actions {
        let kind = action
            .get("action")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::bad("invalid action"))?
            .to_owned();
        if !matches!(kind.as_str(), "view" | "http" | "broadcast" | "copy")
            || action
                .get("label")
                .and_then(Value::as_str)
                .is_none_or(str::is_empty)
        {
            return Err(Error::bad("invalid action type or label"));
        }
        if matches!(kind.as_str(), "view" | "http")
            && action
                .get("url")
                .and_then(Value::as_str)
                .is_none_or(str::is_empty)
        {
            return Err(Error::bad("action URL required"));
        }
        action["id"] = id().into();
        if action.get("clear").is_none() {
            action["clear"] = false.into();
        }
        if kind == "http" && action.get("method").is_none() {
            action["method"] = "POST".into();
        }
    }
    Ok(actions)
}
fn unquote(s: &str) -> String {
    s.trim().trim_matches(['\'', '"']).to_owned()
}
fn split_quoted(input: &str, delimiter: char) -> Result<Vec<String>> {
    let mut quote = None;
    let mut parts = vec![String::new()];
    let mut escaped = false;
    for c in input.chars() {
        if escaped {
            parts.last_mut().unwrap().push(c);
            escaped = false;
            continue;
        }
        if c == '\\' {
            parts.last_mut().unwrap().push(c);
            escaped = true;
            continue;
        }
        if matches!(c, '\'' | '"') {
            if quote == Some(c) {
                quote = None;
            } else if quote.is_none() {
                quote = Some(c);
            }
        }
        if c == delimiter && quote.is_none() {
            parts.push(String::new());
        } else {
            parts.last_mut().unwrap().push(c);
        }
    }
    if quote.is_some() {
        return Err(Error::bad("unterminated action quote"));
    }
    Ok(parts)
}
