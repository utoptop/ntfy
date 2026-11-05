use crate::{
    auth::{Identity, verify_password},
    auth_db::{AuthDb, DbUser},
    config::Config,
    error::{Error, Result},
    http::{App, context_parts, visitor_ip},
    model::{Message, now},
    params::Params,
};
use axum::{
    Json,
    body::to_bytes,
    extract::{Path, Request, State},
    http::{Method, StatusCode},
};
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};
use std::{
    net::IpAddr,
    sync::{Arc, atomic::Ordering},
};

pub(crate) fn success() -> Json<Value> {
    Json(json!({"success":true}))
}
pub(crate) fn database(app: &App) -> Result<&AuthDb> {
    app.auth
        .database
        .as_ref()
        .ok_or_else(|| Error::bad("account database is not configured"))
}
fn user_id(identity: &Identity) -> Result<&str> {
    identity
        .id
        .as_deref()
        .ok_or_else(|| Error::new(StatusCode::UNAUTHORIZED, "account authentication required"))
}
async fn context(
    app: &App,
    (params, peer): (Params, IpAddr),
) -> Result<(Params, Identity, IpAddr)> {
    let ip = visitor_ip(&app.config, &params.headers, peer);
    let (params, identity, _) = app.context((params, peer), true).await?;
    Ok((params, identity, ip))
}
pub(crate) async fn body(request: Request, empty: bool) -> Result<Value> {
    let bytes = to_bytes(request.into_body(), 64 * 1024)
        .await
        .map_err(|_| Error::new(StatusCode::PAYLOAD_TOO_LARGE, "account request too large"))?;
    if bytes.is_empty() && empty {
        return Ok(json!({}));
    }
    let value: Value = serde_json::from_slice(&bytes)?;
    if !value.is_object() {
        return Err(Error::bad("expected JSON object"));
    }
    Ok(value)
}
pub(crate) fn string(value: &Value, key: &str, required: bool) -> Result<String> {
    match value.get(key) {
        Some(Value::String(v)) if !required || !v.is_empty() => Ok(v.clone()),
        None if !required => Ok(String::new()),
        _ => Err(Error::bad(format!("invalid or missing {key}"))),
    }
}
pub(crate) fn number(value: &Value, key: &str) -> Result<Option<i64>> {
    value
        .get(key)
        .map(|v| {
            v.as_i64()
                .filter(|n| *n >= 0)
                .ok_or_else(|| Error::bad(format!("invalid {key}")))
        })
        .transpose()
}
async fn sync(app: &App, identity: &Identity) {
    if let (Some(db), Some(name)) = (&app.auth.database, &identity.name) {
        if let Ok(Some(user)) = db.user(name).await {
            let mut message = Message::event(user.sync_topic, "message");
            message.message = "{\"event\":\"sync\"}".into();
            message.suppress_upstream = true;
            if let Err(error) = app.store.publish(message, false, None).await {
                tracing::warn!(%error,"account sync delivery failed");
            }
        }
    }
}
async fn confirm(app: &App, identity: &Identity, password: String) -> Result<DbUser> {
    let record = database(app)?
        .user(identity.name.as_deref().unwrap_or(""))
        .await?
        .ok_or_else(|| Error::new(StatusCode::UNAUTHORIZED, "invalid credentials"))?;
    let hash = record.hash.clone();
    let permit = app.auth.password_permit()?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        verify_password(&password, &hash)
    })
    .await
    .map_err(Error::internal)?
    .map_err(|_| Error::bad("incorrect password confirmation"))?;
    Ok(record)
}

pub async fn get(State(app): State<Arc<App>>, request: Request) -> Result<Json<Value>> {
    let (_, identity, _) = context(&app, context_parts(&request)).await?;
    let used = app.attachment_bytes.load(Ordering::Relaxed);
    let mut response = json!({"username":identity.name.as_deref().unwrap_or("*"),"role":if identity.admin {"admin"} else if identity.name.is_some(){"user"}else{"anonymous"},
        "limits":{"basis":if identity.admin{"role"}else{"ip"},"messages":0,"messages_expiry_duration":Config::duration(&app.config.cache_duration).as_secs(),"emails":0,"calls":0,"reservations":0,"attachment_file_size":app.config.attachment_file_size_limit,"attachment_total_size":app.config.attachment_total_size_limit,"attachment_expiry_duration":Config::duration(&app.config.attachment_expiry_duration).as_secs(),"attachment_bandwidth":0},
        "stats":{"messages":0,"messages_remaining":0,"emails":0,"emails_remaining":0,"calls":0,"calls_remaining":0,"reservations":0,"reservations_remaining":0,"attachment_total_size":used,"attachment_total_size_remaining":app.config.attachment_total_size_limit.saturating_sub(used)}});
    if let (Some(db), Some(name)) = (&app.auth.database, &identity.name) {
        let user = db
            .user(name)
            .await?
            .ok_or_else(|| Error::new(StatusCode::UNAUTHORIZED, "account removed"))?;
        response["sync_topic"] = user.sync_topic.into();
        response["provisioned"] = user.provisioned.into();
        for key in [
            "language",
            "date_format",
            "time_format",
            "notification",
            "subscriptions",
        ] {
            if let Some(value) = user.prefs.get(key) {
                response[key] = value.clone();
            }
        }
        let id = user.id.clone();
        response["emails"] = db.run(move|db|{
            let mut emails=db.prepare("SELECT email,is_primary FROM user_email WHERE user_id=?1 ORDER BY email")?.query_map([&id],|r|Ok(json!({"address":r.get::<_,String>(0)?,"primary":r.get::<_,bool>(1)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
            let pending=db.prepare("SELECT DISTINCT email FROM user_magic_link WHERE user_id=?1 AND kind='email_verify' AND expires>=?2 ORDER BY email")?.query_map(params![id,now()],|r|Ok(json!({"address":r.get::<_,String>(0)?,"pending":true})))?.collect::<rusqlite::Result<Vec<_>>>()?;
            emails.extend(pending);Ok(Value::Array(emails))}).await?;
        if app.config.enable_reservations {
            let id = user.id.clone();
            let (reservations,limit)=db.run(move|db|{
                let mut q=db.prepare("SELECT a.topic,e.read,e.write FROM user_access a JOIN user_access e ON e.topic=a.topic AND e.user_id='u_everyone' AND e.owner_user_id=a.user_id WHERE a.user_id=?1 AND a.owner_user_id=?1 ORDER BY a.topic")?;
                let reservations=q.query_map([&id],|r|{let (read,write):(bool,bool)=(r.get(1)?,r.get(2)?);Ok(json!({"topic":r.get::<_,String>(0)?.replace("\\_","_"),"everyone":match(read,write){(true,true)=>"read-write",(true,false)=>"read-only",(false,true)=>"write-only",_=>"deny-all"}}))})?.collect::<rusqlite::Result<Vec<_>>>()?;
                let limit:Option<i64>=db.query_row("SELECT t.reservations_limit FROM tier t JOIN user u ON u.tier_id=t.id WHERE u.id=?1",[id],|r|r.get(0)).optional()?;
                Ok((reservations,limit.unwrap_or(0)))}).await?;
            response["stats"]["reservations"] = reservations.len().into();
            response["limits"]["reservations"] = limit.into();
            response["stats"]["reservations_remaining"] =
                (limit - reservations.len() as i64).max(0).into();
            response["reservations"] = reservations.into();
        }
        response["tokens"] =
            serde_json::to_value(db.tokens(&user.id).await?).map_err(Error::internal)?;
    }
    Ok(Json(response))
}
pub async fn create(State(app): State<Arc<App>>, request: Request) -> Result<Json<Value>> {
    let (_, identity, _) = context(&app, context_parts(&request)).await?;
    let db = database(&app)?;
    if !identity.admin && (!app.config.enable_signup || identity.name.is_some()) {
        return Err(Error::bad("signup is not enabled for this request"));
    }
    let value = body(request, false).await?;
    let name = string(&value, "username", true)?;
    let password = string(&value, "password", true)?;
    let hash = app.auth.hash_password(password).await?;
    let email = string(&value, "email", false)?;
    if !email.is_empty() {
        email_address(&email)?;
    }
    db.add_user(&name, &hash, "user").await?;
    if !email.is_empty() && app.mailer.is_some() {
        if let Some(user) = db.user(&name).await? {
            if let Err(error) = send_verification(&app, &user.id, &email).await {
                tracing::warn!(%error,"signup verification email failed");
            }
        }
    }
    Ok(success())
}
pub async fn login(State(app): State<Arc<App>>, request: Request) -> Result<Json<Value>> {
    let (_, identity, ip) = context(&app, context_parts(&request)).await?;
    let id = user_id(&identity)?;
    let token = database(&app)?
        .create_token(id, "", now() + 72 * 3600, &ip.to_string())
        .await?;
    sync(&app, &identity).await;
    Ok(Json(json!({"token":token.token,"username":identity.name})))
}
pub async fn delete(State(app): State<Arc<App>>, request: Request) -> Result<Json<Value>> {
    let (_, identity, _) = context(&app, context_parts(&request)).await?;
    user_id(&identity)?;
    let value = body(request, false).await?;
    let user = confirm(&app, &identity, string(&value, "password", true)?).await?;
    database(&app)?.remove_user(&user.id, true).await?;
    Ok(success())
}
pub async fn password(State(app): State<Arc<App>>, request: Request) -> Result<Json<Value>> {
    let (_, identity, _) = context(&app, context_parts(&request)).await?;
    user_id(&identity)?;
    let value = body(request, false).await?;
    let user = confirm(&app, &identity, string(&value, "password", true)?).await?;
    let hash = app
        .auth
        .hash_password(string(&value, "new_password", true)?)
        .await?;
    database(&app)?.change_password(&user.id, hash).await?;
    Ok(success())
}
pub async fn token(State(app): State<Arc<App>>, request: Request) -> Result<Json<Value>> {
    let (params, identity, ip) = context(&app, context_parts(&request)).await?;
    let id = user_id(&identity)?;
    let db = database(&app)?;
    let response = match *request.method() {
        Method::POST => {
            let value = body(request, true).await?;
            let token = db
                .create_token(
                    id,
                    &string(&value, "label", false)?,
                    number(&value, "expires")?.unwrap_or_else(|| now() + 72 * 3600),
                    &ip.to_string(),
                )
                .await?;
            Json(serde_json::to_value(token).map_err(Error::internal)?)
        }
        Method::PATCH => {
            let value = body(request, true).await?;
            let raw = string(&value, "token", false)?;
            let raw = if raw.is_empty() {
                identity
                    .token
                    .as_deref()
                    .ok_or_else(|| Error::bad("no token provided"))?
            } else {
                &raw
            };
            let label = value
                .get("label")
                .map(|_| string(&value, "label", false))
                .transpose()?;
            let mut expires = number(&value, "expires")?;
            if expires.is_none() && label.is_none() {
                expires = Some(now() + 72 * 3600);
            }
            Json(
                serde_json::to_value(db.update_token(id, raw, label, expires).await?)
                    .map_err(Error::internal)?,
            )
        }
        Method::DELETE => {
            let raw = params
                .get(&["x-token", "token"])
                .or_else(|| identity.token.clone())
                .ok_or_else(|| Error::bad("no token provided"))?;
            db.delete_token(id, &raw).await?;
            success()
        }
        _ => return Err(Error::bad("unsupported token operation")),
    };
    sync(&app, &identity).await;
    Ok(response)
}

pub async fn settings(State(app): State<Arc<App>>, request: Request) -> Result<Json<Value>> {
    let (_, identity, _) = context(&app, context_parts(&request)).await?;
    let id = user_id(&identity)?.to_owned();
    let value = body(request, false).await?;
    for key in ["language", "date_format", "time_format"] {
        if value
            .get(key)
            .is_some_and(|v| !v.is_string() && !v.is_null())
        {
            return Err(Error::bad(format!("invalid {key}")));
        }
    }
    if value
        .get("notification")
        .is_some_and(|v| !v.is_object() && !v.is_null())
    {
        return Err(Error::bad("invalid notification preferences"));
    }
    if let Some(notification) = value.get("notification").and_then(Value::as_object) {
        if notification
            .get("sound")
            .is_some_and(|v| !v.is_null() && !v.is_string())
            || ["min_priority", "delete_after"].iter().any(|k| {
                notification
                    .get(*k)
                    .is_some_and(|v| !v.is_null() && v.as_i64().is_none())
            })
        {
            return Err(Error::bad("invalid notification preference type"));
        }
    }
    database(&app)?
        .run(move |db| {
            let tx = db.transaction()?;
            let raw: String =
                tx.query_row("SELECT prefs FROM user WHERE id=?1", [&id], |r| r.get(0))?;
            let mut prefs: Value = serde_json::from_str(&raw)?;
            if !prefs.is_object() {
                return Err(Error::internal("invalid account preferences"));
            }
            for key in ["language", "date_format", "time_format"] {
                if let Some(v) = value.get(key).filter(|v| !v.is_null()) {
                    prefs[key] = v.clone();
                }
            }
            if let Some(notification) = value.get("notification").and_then(Value::as_object) {
                if !prefs["notification"].is_object() {
                    prefs["notification"] = json!({});
                }
                for key in ["sound", "min_priority", "delete_after"] {
                    if let Some(v) = notification.get(key).filter(|v| !v.is_null()) {
                        prefs["notification"][key] = v.clone();
                    }
                }
            }
            tx.execute(
                "UPDATE user SET prefs=?1 WHERE id=?2",
                params![prefs.to_string(), id],
            )?;
            tx.commit()?;
            Ok(())
        })
        .await?;
    sync(&app, &identity).await;
    Ok(success())
}
pub async fn subscription(State(app): State<Arc<App>>, request: Request) -> Result<Json<Value>> {
    let (params, identity, _) = context(&app, context_parts(&request)).await?;
    let id = user_id(&identity)?.to_owned();
    let method = request.method().clone();
    let value = if method == Method::DELETE {
        json!({"base_url":params.get(&["x-baseurl","baseurl"]).unwrap_or_default(),"topic":params.get(&["x-topic","topic"]).unwrap_or_default()})
    } else {
        body(request, false).await?
    };
    let base = string(&value, "base_url", true)?;
    let topic = string(&value, "topic", true)?;
    let parsed = url::Url::parse(&base).map_err(|_| Error::bad("invalid subscription URL"))?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err(Error::bad("invalid subscription URL"));
    }
    crate::model::topic_valid(&topic)?;
    let result=database(&app)?.run(move|db|{let tx=db.transaction()?;
        let raw:String=tx.query_row("SELECT prefs FROM user WHERE id=?1",[&id],|r|r.get(0))?;let mut prefs:Value=serde_json::from_str(&raw)?;if !prefs.is_object(){return Err(Error::internal("invalid account preferences"));}
        if !prefs["subscriptions"].is_array(){prefs["subscriptions"]=json!([]);}
        let subscriptions=prefs["subscriptions"].as_array_mut().unwrap();
        let index=subscriptions.iter().position(|s|s["base_url"]==base && s["topic"]==topic);
        let response=match method {
            Method::POST=>{if index.is_some(){return Err(Error::new(StatusCode::CONFLICT,"subscription already exists"));}
                let entry=json!({"base_url":base,"topic":topic,"display_name":string(&value,"display_name",false)?});subscriptions.push(entry.clone());entry},
            Method::PATCH=>{let index=index.ok_or_else(||Error::new(StatusCode::NOT_FOUND,"subscription not found"))?;
                subscriptions[index]["display_name"]=string(&value,"display_name",false)?.into();subscriptions[index].clone()},
            Method::DELETE=>{if let Some(index)=index {subscriptions.remove(index);}json!({"success":true})},
            _=>return Err(Error::bad("invalid subscription operation")),
        };
        tx.execute("UPDATE user SET prefs=?1 WHERE id=?2",params![prefs.to_string(),id])?;tx.commit()?;Ok(response)}).await?;
    sync(&app, &identity).await;
    Ok(Json(result))
}

pub async fn reservation_add(State(app): State<Arc<App>>, request: Request) -> Result<Json<Value>> {
    if !app.config.enable_reservations {
        return Err(Error::bad("reservations are not enabled"));
    }
    let (_, identity, _) = context(&app, context_parts(&request)).await?;
    let id = user_id(&identity)?.to_owned();
    let admin = identity.admin;
    let value = body(request, false).await?;
    let topic = string(&value, "topic", true)?;
    crate::model::topic_valid(&topic)?;
    let (read, write) = crate::auth_db::permission(&string(&value, "everyone", true)?)
        .ok_or_else(|| Error::bad("invalid permission"))?;
    let pattern = topic.replace('_', "\\_");
    database(&app)?.run(move|db|{let tx=db.transaction()?;
        let owner:Option<String>=tx.query_row("SELECT owner_user_id FROM user_access WHERE topic=?1 AND user_id=owner_user_id",[&pattern],|r|r.get(0)).optional()?;
        if owner.as_ref().is_some_and(|owner|owner!=&id){return Err(Error::new(StatusCode::CONFLICT,"topic already reserved"));}
        if !admin {
            let limit:Option<i64>=tx.query_row("SELECT t.reservations_limit FROM tier t JOIN user u ON u.tier_id=t.id WHERE u.id=?1",[&id],|r|r.get(0)).optional()?;
            let limit=limit.ok_or_else(||Error::new(StatusCode::UNAUTHORIZED,"reservations require an eligible tier"))?;
            let count:i64=tx.query_row("SELECT COUNT(*) FROM user_access WHERE user_id=?1 AND owner_user_id=?1",[&id],|r|r.get(0))?;
            if owner.is_none() && count>=limit {return Err(Error::new(StatusCode::TOO_MANY_REQUESTS,"reservation limit reached"));}
        }
        let other:bool=tx.query_row(r"SELECT EXISTS(SELECT 1 FROM user_access WHERE (topic=?1 OR ?2 LIKE topic ESCAPE '\') AND (owner_user_id IS NULL OR owner_user_id!=?3))",params![pattern,topic,id],|r|r.get(0))?;
        if other {return Err(Error::new(StatusCode::CONFLICT,"topic has existing access rules"));}
        for (user,read,write) in [(id.as_str(),true,true),("u_everyone",read,write)] {
            tx.execute("INSERT INTO user_access(user_id,topic,read,write,owner_user_id,provisioned) VALUES (?1,?2,?3,?4,?5,0) ON CONFLICT(user_id,topic) DO UPDATE SET read=excluded.read,write=excluded.write,owner_user_id=excluded.owner_user_id",params![user,pattern,read,write,id])?;
        }
        tx.commit()?;Ok(())}).await?;
    sync(&app, &identity).await;
    Ok(success())
}
pub async fn reservation_delete(
    State(app): State<Arc<App>>,
    Path(topic): Path<String>,
    request: Request,
) -> Result<Json<Value>> {
    let (params, identity, _) = context(&app, context_parts(&request)).await?;
    let id = user_id(&identity)?.to_owned();
    crate::model::topic_valid(&topic)?;
    let erase = params.boolean(&["x-delete-messages", "delete-messages"], false);
    let cache_topic = topic.clone();
    let pattern = topic.replace('_', "\\_");
    database(&app)?.run(move|db|{let tx=db.transaction()?;
        let owned:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM user_access WHERE user_id=?1 AND owner_user_id=?1 AND topic=?2)",params![id,pattern],|r|r.get(0))?;
        if !owned {return Err(Error::new(StatusCode::UNAUTHORIZED,"topic is not reserved by this account"));}
        tx.execute("DELETE FROM user_access WHERE owner_user_id=?1 AND topic=?2",params![id,pattern])?;tx.commit()?;Ok(())}).await?;
    if erase {
        app.store.expire_topic(cache_topic).await?;
    }
    sync(&app, &identity).await;
    Ok(success())
}

fn mailer(app: &App) -> Result<&crate::smtp::Sender> {
    app.mailer
        .as_ref()
        .ok_or_else(|| Error::bad("email sending is not configured"))
}
fn email_address(value: &str) -> Result<lettre::message::Mailbox> {
    let address: lettre::Address = value
        .parse()
        .map_err(|_| Error::bad("invalid email address"))?;
    Ok(lettre::message::Mailbox::new(None, address))
}
pub(crate) async fn magic_link(
    app: &App,
    id: String,
    kind: &str,
    email: String,
    ttl: i64,
) -> Result<String> {
    let raw = crate::auth_db::random_id("", 64);
    let hash = crate::auth::token_hash(&raw);
    let kind = kind.to_owned();
    database(app)?.run(move|db|{let tx=db.transaction()?;
        tx.execute("DELETE FROM user_magic_link WHERE user_id=?1 AND kind=?2 AND (?2='password_reset' OR email=?3)",params![id,kind,email])?;
        tx.execute("INSERT INTO user_magic_link(token_hash,kind,user_id,email,expires,created) VALUES(?1,?2,?3,?4,?5,?6)",params![hash,kind,id,email,now()+ttl,now()])?;
        tx.commit()?;Ok(())}).await?;
    Ok(raw)
}
async fn send_verification(app: &App, id: &str, email: &str) -> Result<()> {
    if app.config.base_url.is_empty() {
        return Err(Error::bad("base-url is required for email verification"));
    }
    let to = email_address(email)?;
    let sender = mailer(app)?;
    let permit = sender.reserve(id.into())?;
    let token = magic_link(app, id.into(), "email_verify", email.into(), 86400).await?;
    let link = format!(
        "{}/account/email/verify/{token}",
        app.config.base_url.trim_end_matches('/')
    );
    sender.send(
        permit,
        to,
        "Verify your email for ntfy".into(),
        format!("Verify your email for ntfy:\n\n{link}\n\nThis one-time link expires in 24 hours."),
    );
    Ok(())
}
pub async fn email(State(app): State<Arc<App>>, request: Request) -> Result<Json<Value>> {
    let (_, identity, _) = context(&app, context_parts(&request)).await?;
    let id = user_id(&identity)?.to_owned();
    let method = request.method().clone();
    let path = request.uri().path().to_owned();
    let value = body(request, false).await?;
    let email = string(&value, "email", true)?;
    email_address(&email)?;
    if method == Method::PUT || path.ends_with("/resend") {
        mailer(&app)?;
        let (uid, address) = (id.clone(), email.clone());
        let resend = path.ends_with("/resend");
        database(&app)?.run(move|db|{
            let verified:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM user_email WHERE user_id=?1 AND email=?2)",params![uid,address],|r|r.get(0))?;
            if verified{return Err(Error::new(StatusCode::CONFLICT,"email already verified"));}
            if resend {
                let pending:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM user_magic_link WHERE user_id=?1 AND email=?2 AND kind='email_verify' AND expires>=?3)",params![uid,address,now()],|r|r.get(0))?;
                if !pending{return Err(Error::bad("email is not pending verification"));}
            }
            Ok(())}).await?;
        send_verification(&app, &id, &email).await?;
    } else if method == Method::DELETE {
        database(&app)?.run(move|db|{let tx=db.transaction()?;
            tx.execute("DELETE FROM user_email WHERE user_id=?1 AND email=?2",params![id,email])?;
            tx.execute("DELETE FROM user_magic_link WHERE user_id=?1 AND email=?2 AND kind='email_verify'",params![id,email])?;
            tx.commit()?;Ok(())}).await?;
    } else if path.ends_with("/primary") {
        database(&app)?.run(move|db|{let tx=db.transaction()?;
            let verified:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM user_email WHERE user_id=?1 AND email=?2)",params![id,email],|r|r.get(0))?;
            if !verified{return Err(Error::bad("email is not verified"));}
            let occupied:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM user_email WHERE email=?1 AND is_primary=1 AND user_id!=?2)",params![email,id],|r|r.get(0))?;
            if occupied{return Err(Error::new(StatusCode::CONFLICT,"email is already a primary address"));}
            tx.execute("UPDATE user_email SET is_primary=0 WHERE user_id=?1",[&id])?;
            tx.execute("UPDATE user_email SET is_primary=1 WHERE user_id=?1 AND email=?2",params![id,email])?;
            tx.commit()?;Ok(())}).await?;
    } else {
        return Err(Error::bad("invalid email operation"));
    }
    sync(&app, &identity).await;
    Ok(success())
}
pub async fn email_verify(State(app): State<Arc<App>>, request: Request) -> Result<Json<Value>> {
    let _ = context(&app, context_parts(&request)).await?;
    mailer(&app)?;
    let value = body(request, false).await?;
    let hash = crate::auth::token_hash(&string(&value, "token", true)?);
    let name=database(&app)?.run(move|db|{let tx=db.transaction()?;
        let row:Option<(String,String,String)>=tx.query_row("SELECT m.user_id,m.email,u.user FROM user_magic_link m JOIN user u ON u.id=m.user_id WHERE token_hash=?1 AND kind='email_verify' AND expires>=?2 AND u.deleted IS NULL",params![hash,now()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
        let (id,email,name)=row.ok_or_else(||Error::bad("invalid or expired verification link"))?;
        tx.execute("DELETE FROM user_magic_link WHERE token_hash=?1",[hash])?;
        tx.execute("INSERT OR IGNORE INTO user_email(user_id,email,is_primary) VALUES(?1,?2,0)",params![id,email])?;
        let occupied:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM user_email WHERE (user_id=?1 OR email=?2) AND is_primary=1)",params![id,email],|r|r.get(0))?;
        if !occupied{tx.execute("UPDATE user_email SET is_primary=1 WHERE user_id=?1 AND email=?2",params![id,email])?;}
        tx.commit()?;Ok(name)}).await?;
    sync(
        &app,
        &Identity {
            name: Some(name),
            ..Identity::default()
        },
    )
    .await;
    Ok(success())
}
pub async fn password_reset_request(
    State(app): State<Arc<App>>,
    request: Request,
) -> Result<Json<Value>> {
    let (_, _, ip) = context(&app, context_parts(&request)).await?;
    let sender = mailer(&app)?;
    let value = body(request, false).await?;
    let identifier = string(&value, "identifier", false)?;
    // Charge and reserve before lookup, so unknown and real accounts get the same response.
    let permit = sender.reserve(format!("reset:{ip}"))?;
    let target = database(&app)?.user(identifier.trim()).await?;
    if let Some(user) = target.filter(|u| !u.provisioned) {
        let id = user.id;
        let lookup = id.clone();
        let email: Option<String> = database(&app)?
            .run(move |db| {
                Ok(db
                    .query_row(
                        "SELECT email FROM user_email WHERE user_id=?1 AND is_primary=1",
                        [lookup],
                        |r| r.get(0),
                    )
                    .optional()?)
            })
            .await?;
        if let Some(email) = email.filter(|_| !app.config.base_url.is_empty()) {
            let token = magic_link(&app, id, "password_reset", String::new(), 3600).await?;
            let link = format!(
                "{}/account/password/reset/{token}",
                app.config.base_url.trim_end_matches('/')
            );
            sender.send(
                permit,
                email_address(&email)?,
                "Reset your ntfy password".into(),
                format!(
                    "Reset your ntfy password:\n\n{link}\n\nThis one-time link expires in one hour."
                ),
            );
        }
    }
    Ok(success())
}
pub async fn password_reset(State(app): State<Arc<App>>, request: Request) -> Result<Json<Value>> {
    let _ = context(&app, context_parts(&request)).await?;
    mailer(&app)?;
    let value = body(request, false).await?;
    let token_hash = crate::auth::token_hash(&string(&value, "token", true)?);
    let password = app
        .auth
        .hash_password(string(&value, "password", true)?)
        .await?;
    database(&app)?.run(move|db|{let tx=db.transaction()?;
        let id:Option<String>=tx.query_row("SELECT m.user_id FROM user_magic_link m JOIN user u ON u.id=m.user_id WHERE m.token_hash=?1 AND m.kind='password_reset' AND m.expires>=?2 AND u.provisioned=0 AND u.deleted IS NULL",params![token_hash,now()],|r|r.get(0)).optional()?;
        let id=id.ok_or_else(||Error::bad("invalid or expired password reset link"))?;
        tx.execute("UPDATE user SET pass=?1 WHERE id=?2",params![password,id])?;
        tx.execute("DELETE FROM user_magic_link WHERE token_hash=?1",[token_hash])?;
        tx.commit()?;Ok(())}).await?;
    Ok(success())
}
