use crate::{
    account::{body, database, number, string, success},
    error::{Error, Result},
    http::{App, context_parts},
    model::now,
};
use axum::{
    Json,
    extract::{Request, State},
    http::{Method, StatusCode},
};
use serde_json::{Value, json};
use std::sync::Arc;
fn local_only() -> Error {
    Error::new(
        StatusCode::FORBIDDEN,
        "use the local ntfy administrative CLI for this operation",
    )
}
fn permission(read: bool, write: bool) -> &'static str {
    match (read, write) {
        (true, true) => "read-write",
        (true, false) => "read-only",
        (false, true) => "write-only",
        _ => "deny-all",
    }
}
pub async fn handle(State(app): State<Arc<App>>, request: Request) -> Result<Json<Value>> {
    let (params, identity, visitor) = app.context(context_parts(&request), true).await?;
    if !identity.admin {
        return Err(Error::new(StatusCode::FORBIDDEN, "administrator required"));
    }
    let db = database(&app)?;
    let path = request.uri().path().to_owned();
    let method = request.method().clone();
    if path == "/v1/users" && method == Method::GET {
        let username = params.query.get("username").cloned();
        let default = app.config.auth_default_access.clone();
        return Ok(Json(db.run(move|db|{
            let mut q=db.prepare("SELECT u.id,u.user,u.role,u.provisioned,t.code,t.name FROM user u LEFT JOIN tier t ON t.id=u.tier_id WHERE u.deleted IS NULL AND (?1 IS NULL OR u.user=?1) ORDER BY CASE WHEN u.user='*' THEN 1 ELSE 0 END,u.user")?;
            let rows=q.query_map([&username],|r|Ok((r.get::<_,String>(0)?,json!({"username":r.get::<_,String>(1)?,"role":r.get::<_,String>(2)?,"provisioned":r.get::<_,bool>(3)?,"tier":r.get::<_,Option<String>>(4)?.unwrap_or_default(),"tier_name":r.get::<_,Option<String>>(5)?.unwrap_or_default()}))))?.collect::<rusqlite::Result<Vec<_>>>()?;
            if username.is_some()&&rows.is_empty(){return Err(Error::new(StatusCode::NOT_FOUND,"user not found"));}
            let mut result=Vec::new();for(id,mut user)in rows{
                let mut q=db.prepare("SELECT topic,read,write,provisioned FROM user_access WHERE user_id=?1 ORDER BY topic")?;
                let grants=q.query_map([id],|r|Ok(json!({"topic":r.get::<_,String>(0)?.replace("\\_","_").replace('%',"*"),"permission":permission(r.get(1)?,r.get(2)?),"provisioned":r.get::<_,bool>(3)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
                user["grants"]=grants.into();if user["username"]=="*"{user["default_access"]=default.clone().into();}result.push(user);
            }Ok(Value::Array(result))}).await?));
    }
    let value = if method == Method::GET {
        json!({"username":params.query.get("username").cloned().unwrap_or_default()})
    } else {
        body(request, false).await?
    };
    let username = string(&value, "username", false)?;
    if path == "/v1/users" && matches!(method, Method::POST | Method::PUT) {
        if !string(&value, "role", false)?.is_empty() {
            return Err(local_only());
        }
        if !string(&value, "tier", false)?.is_empty() {
            return Err(Error::bad("tier changes are not yet available"));
        }
        let existing = db.user(&username).await?;
        if existing.as_ref().is_some_and(|u| u.role == "admin") {
            return Err(local_only());
        }
        if existing.is_some() && method == Method::POST {
            return Err(Error::new(StatusCode::CONFLICT, "user already exists"));
        }
        let password = string(&value, "password", false)?;
        let hash = string(&value, "hash", false)?;
        let hash = if !hash.is_empty() {
            hash.parse::<bcrypt::HashParts>()
                .map_err(|_| Error::bad("invalid bcrypt hash"))?;
            hash
        } else {
            app.auth.hash_password(password).await?
        };
        if let Some(user) = existing {
            db.change_password(&user.id, hash).await?;
        } else {
            db.add_user(&username, &hash, "user").await?;
        }
        return Ok(success());
    }
    if path == "/v1/users/access" {
        if method == Method::DELETE {
            if username.is_empty() {
                return Err(local_only());
            }
            let topic = string(&value, "topic", false)?;
            db.reset_access(
                Some(username),
                if topic.is_empty() { None } else { Some(topic) },
            )
            .await?;
        } else {
            let user = db
                .user(&username)
                .await?
                .ok_or_else(|| Error::new(StatusCode::NOT_FOUND, "user not found"))?;
            if user.role == "admin" {
                return Err(Error::bad("administrators already have full access"));
            }
            db.set_access(
                &username,
                &string(&value, "topic", true)?,
                &string(&value, "permission", true)?,
            )
            .await?;
        }
        return Ok(success());
    }
    if username.is_empty() || username == "*" {
        return Err(local_only());
    }
    let user = db
        .user(&username)
        .await?
        .ok_or_else(|| Error::new(StatusCode::NOT_FOUND, "user not found"))?;
    if user.role == "admin" {
        return Err(local_only());
    }
    if path == "/v1/users" && method == Method::DELETE {
        db.remove_user(&user.id, false).await?;
        return Ok(success());
    }
    if path == "/v1/users/tokens" {
        if method == Method::GET {
            let tokens = db
                .tokens(&user.id)
                .await?
                .into_iter()
                .map(|t| {
                    let mut t = serde_json::to_value(t).expect("serializable token");
                    t["username"] = username.clone().into();
                    t
                })
                .collect();
            return Ok(Json(Value::Array(tokens)));
        } else if method == Method::POST {
            let expires = number(&value, "expires")?.unwrap_or(0);
            if expires != 0 && expires <= now() {
                return Err(Error::bad("expiration must be in the future"));
            }
            let token = db
                .create_token(
                    &user.id,
                    &string(&value, "label", false)?,
                    expires,
                    &visitor.ip.to_string(),
                )
                .await?;
            let mut response = serde_json::to_value(token).map_err(Error::internal)?;
            response["username"] = username.into();
            return Ok(Json(response));
        } else if method == Method::DELETE {
            db.delete_token(&user.id, &string(&value, "token", true)?)
                .await?;
            return Ok(success());
        }
    }
    if path == "/v1/users/password/reset" && method == Method::POST {
        if user.provisioned || app.config.base_url.is_empty() {
            return Err(Error::bad(
                "password reset needs a non-provisioned user and base-url",
            ));
        }
        let email_job = if value
            .get("send_email")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            let sender = app
                .mailer
                .as_ref()
                .ok_or_else(|| Error::bad("SMTP sender is not configured"))?;
            let id = user.id.clone();
            let email: String = db
                .run(move |db| {
                    db.query_row(
                        "SELECT email FROM user_email WHERE user_id=?1 AND is_primary=1",
                        [id],
                        |r| r.get(0),
                    )
                    .map_err(|_| Error::bad("user has no primary email"))
                })
                .await?;
            let to: lettre::message::Mailbox = email
                .parse()
                .map_err(|_| Error::bad("invalid account email"))?;
            Some((sender.reserve(user.id.clone())?, to, email))
        } else {
            None
        };
        let token =
            crate::account::magic_link(&app, user.id, "password_reset", String::new(), 3600)
                .await?;
        let link = format!(
            "{}/account/password/reset/{token}",
            app.config.base_url.trim_end_matches('/')
        );
        let mut response = json!({"link":link});
        if let Some((permit, to, email)) = email_job {
            app.mailer.as_ref().unwrap().send(
                permit,
                to,
                "Reset your ntfy password".into(),
                format!("Reset your ntfy password:\n\n{link}\n\nThis link expires in one hour."),
            );
            response["email"] = email.into();
        }
        return Ok(Json(response));
    }
    Err(Error::new(
        StatusCode::NOT_FOUND,
        "unknown administrator operation",
    ))
}
