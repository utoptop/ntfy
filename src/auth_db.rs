use crate::sqlite::Sqlite;
use crate::{
    config::Config,
    error::{Error, Result},
    model::now,
};
use axum::http::StatusCode;
use rand::{Rng, distributions::Alphanumeric};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use serde_json::{Value, json};
use std::{path::Path, time::Duration};
#[derive(Clone)]
pub struct AuthDb {
    db: Sqlite,
}
#[derive(Clone, Debug)]
pub struct DbUser {
    pub id: String,
    pub name: String,
    pub hash: String,
    pub role: String,
    pub prefs: Value,
    pub sync_topic: String,
    pub provisioned: bool,
}
#[derive(Clone, Debug, Serialize)]
pub struct Token {
    pub token: String,
    pub label: String,
    pub last_access: i64,
    pub last_origin: String,
    pub expires: i64,
    pub provisioned: bool,
}
const USER_COLUMNS: &str = "u.id,u.user,u.pass,u.role,u.prefs,u.sync_topic,u.provisioned";
fn read_user(row: &rusqlite::Row<'_>) -> rusqlite::Result<DbUser> {
    let prefs: String = row.get(4)?;
    Ok(DbUser {
        id: row.get(0)?,
        name: row.get(1)?,
        hash: row.get(2)?,
        role: row.get(3)?,
        prefs: serde_json::from_str(&prefs).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(4, rusqlite::types::Type::Text, Box::new(e))
        })?,
        sync_topic: row.get(5)?,
        provisioned: row.get(6)?,
    })
}
fn read_token(row: &rusqlite::Row<'_>) -> rusqlite::Result<Token> {
    Ok(Token {
        token: row.get(0)?,
        label: row.get(1)?,
        last_access: row.get(2)?,
        last_origin: row.get(3)?,
        expires: row.get(4)?,
        provisioned: row.get(5)?,
    })
}
pub fn random_id(prefix: &str, length: usize) -> String {
    format!(
        "{prefix}{}",
        rand::thread_rng()
            .sample_iter(&Alphanumeric)
            .take(length - prefix.len())
            .map(char::from)
            .collect::<String>()
    )
}
pub fn generate_token() -> String {
    random_id("tk_", 32).to_lowercase()
}
pub fn valid_username(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.+@".contains(&b))
}
pub fn permission(value: &str) -> Option<(bool, bool)> {
    match value {
        "read-write" | "rw" => Some((true, true)),
        "read-only" | "read" | "ro" => Some((true, false)),
        "write-only" | "write" | "wo" => Some((false, true)),
        "deny-all" | "deny" | "none" => Some((false, false)),
        _ => None,
    }
}
fn sql_pattern(topic: &str) -> Result<String> {
    if topic.is_empty()
        || topic.len() > 64
        || !topic
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-*".contains(&b))
    {
        return Err(Error::bad("invalid topic pattern"));
    }
    Ok(topic.replace('_', "\\_").replace('*', "%"))
}
fn conflict(message: &str) -> Error {
    Error::new(StatusCode::CONFLICT, message)
}
fn missing() -> Error {
    Error::new(StatusCode::NOT_FOUND, "account or token not found")
}
fn check_mutable(db: &Connection, id: &str) -> Result<()> {
    if id == "u_everyone" {
        return Err(Error::bad("cannot change the everyone user"));
    }
    let provisioned: Option<bool> = db
        .query_row(
            "SELECT provisioned FROM user WHERE id=?1 AND deleted IS NULL",
            [id],
            |r| r.get(0),
        )
        .optional()?;
    match provisioned {
        Some(false) => Ok(()),
        Some(true) => Err(conflict(
            "provisioned users must be changed in the configuration",
        )),
        None => Err(missing()),
    }
}

impl AuthDb {
    pub fn open(config: &Config) -> Result<Self> {
        Self::open_inner(config, false)
    }
    pub fn open_server(config: &Config) -> Result<Self> {
        Self::open_inner(config, true)
    }
    fn open_inner(config: &Config, provision: bool) -> Result<Self> {
        let path = Path::new(&config.auth_file);
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        let is_new = !path.exists() && path.as_os_str() != ":memory:";
        if is_new {
            crate::sqlite_backup::secure_create(path)?;
        }
        let mut db = Connection::open(path)?;
        db.busy_timeout(Duration::from_secs(5))?;
        db.execute_batch("PRAGMA foreign_keys=OFF;")?;
        initialize(&mut db, path)?;
        db.execute_batch("PRAGMA foreign_keys=ON; PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA cache_size=-1024;")?;
        if provision {
            provision_config(&mut db, config)?;
        }
        Ok(Self {
            db: Sqlite::new(db, config.database_queue_size, "ntfy-auth-sqlite")?,
        })
    }
    pub async fn run<T: Send + 'static>(
        &self,
        job: impl FnOnce(&mut Connection) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        self.db.run(job).await
    }
    pub async fn record_token_access(&self, token: &str, ip: std::net::IpAddr) -> Result<()> {
        let token = token.to_owned();
        self.run(move|db|{db.execute("UPDATE user_token SET last_access=?1,last_origin=?2 WHERE token=?3 AND (last_access<?1-60 OR last_origin!=?2)",params![now(),ip.to_string(),token])?;Ok(())}).await
    }
    pub async fn maintain(&self) -> Result<()> {
        self.run(|db| {
            let tx = db.transaction()?;
            tx.execute(
                "DELETE FROM user_token WHERE expires!=0 AND expires<?1",
                [now()],
            )?;
            tx.execute("DELETE FROM user_magic_link WHERE expires<?1", [now()])?;
            tx.execute(
                "DELETE FROM user WHERE deleted IS NOT NULL AND deleted<?1",
                [now()],
            )?;
            tx.commit()?;
            Ok(())
        })
        .await
    }
    pub async fn user(&self, identifier: &str) -> Result<Option<DbUser>> {
        let identifier = identifier.to_owned();
        self.run(move |db| Ok(db.query_row(&format!("SELECT {USER_COLUMNS} FROM user u WHERE u.deleted IS NULL AND (u.user=?1 OR u.id=(SELECT user_id FROM user_email WHERE email=?1 AND is_primary=1)) ORDER BY CASE WHEN u.user=?1 THEN 0 ELSE 1 END LIMIT 1"),[identifier],read_user).optional()?)).await
    }
    pub async fn user_by_token(&self, token: &str) -> Result<Option<DbUser>> {
        let token = token.to_owned();
        self.run(move |db| Ok(db.query_row(&format!("SELECT {USER_COLUMNS} FROM user u JOIN user_token t ON t.user_id=u.id WHERE t.token=?1 AND (t.expires=0 OR t.expires>=?2) AND u.deleted IS NULL"),params![token,now()],read_user).optional()?)).await
    }
    pub async fn access(&self, username: &str, topic: &str) -> Result<Option<(bool, bool)>> {
        let (username, topic) = (username.to_owned(), topic.to_owned());
        self.run(move |db| {
            if topic.starts_with("st_") {
                let owner:Option<String>=db.query_row("SELECT user FROM user WHERE sync_topic=?1 AND deleted IS NULL",[&topic],|r|r.get(0)).optional()?;
                return Ok(Some((owner.as_deref()==Some(&username),false)));
            }
            Ok(db.query_row(r"SELECT a.read,a.write FROM user_access a JOIN user u ON u.id=a.user_id WHERE (u.user=?1 OR u.user='*') AND ?2 LIKE a.topic ESCAPE '\' ORDER BY u.user DESC,LENGTH(a.topic) DESC,a.write DESC LIMIT 1",params![username,topic],|r|Ok((r.get(0)?,r.get(1)?))).optional()?)
        }).await
    }
    pub async fn add_user(&self, name: &str, hash: &str, role: &str) -> Result<()> {
        if !valid_username(name) || !matches!(role, "user" | "admin") {
            return Err(Error::bad("invalid username or role"));
        }
        let (name, hash, role) = (name.to_owned(), hash.to_owned(), role.to_owned());
        self.run(move |db| {
            let exists:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM user WHERE user=?1)",[&name],|r|r.get(0))?;
            if exists { return Err(conflict("user already exists")); }
            db.execute("INSERT INTO user(id,user,pass,role,sync_topic,provisioned,created) VALUES (?1,?2,?3,?4,?5,0,?6)",params![random_id("u_",12),name,hash,role,random_id("st_",16),now()])?;
            Ok(())
        }).await
    }
    pub async fn users(&self) -> Result<Vec<Value>> {
        self.run(|db| {
            let mut stmt=db.prepare("SELECT id,user,role,provisioned,deleted FROM user WHERE deleted IS NULL ORDER BY user")?;
            let rows=stmt.query_map([],|r|Ok(json!({"id":r.get::<_,String>(0)?,"username":r.get::<_,String>(1)?,"role":r.get::<_,String>(2)?,"provisioned":r.get::<_,bool>(3)?,"deleted":r.get::<_,Option<i64>>(4)?})))?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        }).await
    }
    pub async fn change_password(&self, id: &str, hash: String) -> Result<()> {
        let id = id.to_owned();
        self.run(move |db| {
            check_mutable(db, &id)?;
            db.execute("UPDATE user SET pass=?1 WHERE id=?2", params![hash, id])?;
            Ok(())
        })
        .await
    }
    pub async fn change_role(&self, id: &str, role: &str) -> Result<()> {
        if !matches!(role, "user" | "admin") {
            return Err(Error::bad("invalid role"));
        }
        let (id, role) = (id.to_owned(), role.to_owned());
        self.run(move |db| {
            let tx = db.transaction()?;
            check_mutable(&tx, &id)?;
            tx.execute("UPDATE user SET role=?1 WHERE id=?2", params![role, id])?;
            if role == "admin" {
                tx.execute(
                    "DELETE FROM user_access WHERE user_id=?1 OR owner_user_id=?1",
                    [&id],
                )?;
            }
            tx.commit()?;
            Ok(())
        })
        .await
    }
    pub async fn remove_user(&self, id: &str, soft: bool) -> Result<()> {
        let id = id.to_owned();
        self.run(move |db| {
            let tx = db.transaction()?;
            check_mutable(&tx, &id)?;
            if soft {
                tx.execute(
                    "DELETE FROM user_access WHERE user_id=?1 OR owner_user_id=?1",
                    [&id],
                )?;
                tx.execute("DELETE FROM user_token WHERE user_id=?1", [&id])?;
                tx.execute(
                    "UPDATE user SET deleted=?1 WHERE id=?2",
                    params![now() + 7 * 86400, id],
                )?;
            } else {
                tx.execute("DELETE FROM user WHERE id=?1", [id])?;
            }
            tx.commit()?;
            Ok(())
        })
        .await
    }
    pub async fn create_token(
        &self,
        id: &str,
        label: &str,
        expires: i64,
        origin: &str,
    ) -> Result<Token> {
        if label.len() > 1024 || expires < 0 {
            return Err(Error::bad("invalid token label or expiration"));
        }
        let id = id.to_owned();
        let token = Token {
            token: generate_token(),
            label: label.into(),
            expires,
            last_access: now(),
            last_origin: origin.into(),
            provisioned: false,
        };
        self.run(move|db|{let tx=db.transaction()?;
            let eligible:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM user WHERE id=?1 AND role!='anonymous' AND deleted IS NULL)",[&id],|r|r.get(0))?;
            if !eligible{return Err(missing());}
            tx.execute("INSERT INTO user_token(user_id,token,label,last_access,last_origin,expires,provisioned) VALUES (?1,?2,?3,?4,?5,?6,0)",params![id,token.token,token.label,token.last_access,token.last_origin,token.expires])?;
            tx.execute("DELETE FROM user_token WHERE user_id=?1 AND token NOT IN (SELECT token FROM user_token WHERE user_id=?1 ORDER BY expires=0 DESC,expires DESC LIMIT 60) AND provisioned=0",[id])?;
            tx.commit()?;Ok(token)}).await
    }
    pub async fn tokens(&self, id: &str) -> Result<Vec<Token>> {
        let id = id.to_owned();
        self.run(move|db|{let mut q=db.prepare("SELECT token,label,last_access,last_origin,expires,provisioned FROM user_token WHERE user_id=?1 ORDER BY last_access DESC")?;Ok(q.query_map([id],read_token)?.collect::<rusqlite::Result<Vec<_>>>()?)}).await
    }
    pub async fn update_token(
        &self,
        id: &str,
        value: &str,
        label: Option<String>,
        expires: Option<i64>,
    ) -> Result<Token> {
        let (id, value) = (id.to_owned(), value.to_owned());
        self.run(move|db|{
            let tx=db.transaction()?;
            let mut token=tx.query_row("SELECT token,label,last_access,last_origin,expires,provisioned FROM user_token WHERE user_id=?1 AND token=?2",params![id,value],read_token).optional()?.ok_or_else(missing)?;
            if token.provisioned {return Err(conflict("provisioned token cannot be changed"));}
            if let Some(label)=label { if label.len()>1024 {return Err(Error::bad("token label too long"));} token.label=label; }
            if let Some(expires)=expires {if expires<0 {return Err(Error::bad("invalid expiration"));}token.expires=expires;}
            tx.execute("UPDATE user_token SET label=?1,expires=?2 WHERE user_id=?3 AND token=?4",params![token.label,token.expires,id,value])?;
            tx.commit()?;Ok(token)
        }).await
    }
    pub async fn delete_token(&self, id: &str, value: &str) -> Result<()> {
        let (id, value) = (id.to_owned(), value.to_owned());
        self.run(move |db| {
            let provisioned: Option<bool> = db
                .query_row(
                    "SELECT provisioned FROM user_token WHERE user_id=?1 AND token=?2",
                    params![id, value],
                    |r| r.get(0),
                )
                .optional()?;
            if provisioned == Some(true) {
                return Err(conflict("provisioned token cannot be removed"));
            }
            db.execute(
                "DELETE FROM user_token WHERE user_id=?1 AND token=?2",
                params![id, value],
            )?;
            Ok(())
        })
        .await
    }
    pub async fn set_access(&self, username: &str, topic: &str, access: &str) -> Result<()> {
        let (read, write) = permission(access).ok_or_else(|| Error::bad("invalid permission"))?;
        let topic = sql_pattern(topic)?;
        let username = if username == "everyone" {
            "*"
        } else {
            username
        }
        .to_owned();
        self.run(move|db|{let tx=db.transaction()?;
            let id:String=tx.query_row("SELECT id FROM user WHERE user=?1 AND deleted IS NULL",[username],|r|r.get(0)).optional()?.ok_or_else(missing)?;
            let provisioned:Option<bool>=tx.query_row("SELECT provisioned FROM user_access WHERE user_id=?1 AND topic=?2",params![id,topic],|r|r.get(0)).optional()?;
            if provisioned==Some(true) {return Err(conflict("provisioned access must be changed in the configuration"));}
            tx.execute("INSERT INTO user_access(user_id,topic,read,write,owner_user_id,provisioned) VALUES (?1,?2,?3,?4,NULL,0) ON CONFLICT(user_id,topic) DO UPDATE SET read=excluded.read,write=excluded.write,owner_user_id=NULL",params![id,topic,read,write])?;
            tx.commit()?;Ok(())}).await
    }
    pub async fn reset_access(
        &self,
        username: Option<String>,
        topic: Option<String>,
    ) -> Result<()> {
        let topic = topic.map(|t| sql_pattern(&t)).transpose()?;
        self.run(move|db|{db.execute("DELETE FROM user_access WHERE provisioned=0 AND (?1 IS NULL OR user_id=(SELECT id FROM user WHERE user=?1) OR owner_user_id=(SELECT id FROM user WHERE user=?1)) AND (?2 IS NULL OR topic=?2)",params![username,topic])?;Ok(())}).await
    }
    pub async fn list_access(&self, username: Option<String>) -> Result<Vec<Value>> {
        self.run(move|db|{let mut q=db.prepare("SELECT u.user,a.topic,a.read,a.write,a.provisioned FROM user_access a JOIN user u ON u.id=a.user_id WHERE ?1 IS NULL OR u.user=?1 ORDER BY u.user,a.topic")?;
            Ok(q.query_map([username],|r|Ok(json!({"username":r.get::<_,String>(0)?,"topic":r.get::<_,String>(1)?.replace("\\_","_").replace('%',"*"),"read":r.get::<_,bool>(2)?,"write":r.get::<_,bool>(3)?,"provisioned":r.get::<_,bool>(4)?})))?.collect::<rusqlite::Result<Vec<_>>>()?)}).await
    }
}

fn initialize(db: &mut Connection, path: &Path) -> Result<()> {
    let tables: i64 = db.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table'",
        [],
        |r| r.get(0),
    )?;
    if tables == 0 {
        let tx = db.transaction()?;
        tx.execute_batch(include_str!("schema/auth_v11.sql"))?;
        tx.execute_batch("CREATE TABLE schemaVersion(id INT PRIMARY KEY,version INT NOT NULL,min_version INT NOT NULL DEFAULT 1); INSERT INTO schemaVersion VALUES(1,11,1);")?;
        tx.commit()?;
        return Ok(());
    }
    let known:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='schemaVersion') AND EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='user')",[],|r|r.get(0))?;
    if !known {
        return Err(Error::bad(
            "auth-file is not a standard ntfy authentication database",
        ));
    }
    let version: i64 = db.query_row("SELECT version FROM schemaVersion WHERE id=1", [], |r| {
        r.get(0)
    })?;
    let has_min: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info('schemaVersion') WHERE name='min_version')",
        [],
        |r| r.get(0),
    )?;
    if has_min {
        let minimum: i64 = db.query_row(
            "SELECT min_version FROM schemaVersion WHERE id=1",
            [],
            |r| r.get(0),
        )?;
        if minimum > 11 {
            return Err(Error::bad("auth-file requires a newer ntfy schema"));
        }
    }
    if version >= 11 {
        return Ok(());
    }
    if version < 1 {
        return Err(Error::bad("unsupported auth-file schema version"));
    }
    crate::sqlite_backup::backup(db, path, &format!("v{version}"))?;
    // Match upstream's migration order with foreign keys off during table rebuilds.
    let tx = db.transaction()?;
    if !has_min {
        tx.execute_batch(
            "ALTER TABLE schemaVersion ADD COLUMN min_version INT NOT NULL DEFAULT 1;",
        )?;
    }
    for current in version..11 {
        match current {
            1 => {
                tx.execute_batch(include_str!("schema/auth_1_2_before.sql"))?;
                let names = tx
                    .prepare("SELECT user FROM user_old")?
                    .query_map([], |r| r.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                for name in names {
                    tx.execute("INSERT INTO user(id,user,pass,role,sync_topic,created) SELECT ?1,user,pass,role,?2,UNIXEPOCH() FROM user_old WHERE user=?3",params![random_id("u_",12),random_id("st_",16),name])?;
                }
                tx.execute_batch(include_str!("schema/auth_1_2_after.sql"))?;
            }
            2 => tx.execute_batch(include_str!("schema/auth_2_3.sql"))?,
            3 => tx.execute_batch(include_str!("schema/auth_3_4.sql"))?,
            4 => tx.execute_batch(include_str!("schema/auth_4_5.sql"))?,
            5 => tx.execute_batch(include_str!("schema/auth_5_6.sql"))?,
            6 => tx.execute_batch(include_str!("schema/auth_6_7.sql"))?,
            7 => tx.execute_batch(include_str!("schema/auth_7_8.sql"))?,
            8 => tx.execute_batch(include_str!("schema/auth_8_9.sql"))?,
            9 => tx.execute_batch(include_str!("schema/auth_9_10.sql"))?,
            10 => tx.execute_batch(include_str!("schema/auth_10_11.sql"))?,
            _ => unreachable!(),
        }
    }
    tx.execute("UPDATE schemaVersion SET version=11 WHERE id=1", [])?;
    tx.commit()?;
    Ok(())
}

fn provision_config(db: &mut Connection, config: &Config) -> Result<()> {
    let tx = db.transaction()?;
    // Reconcile only declarative entries. CLI-created users, ACLs and tokens survive.
    let existing = tx
        .prepare("SELECT user FROM user WHERE provisioned=1 AND user!='*'")?
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for name in existing {
        if !config.auth_users.iter().any(|u| u.name == name) {
            tx.execute("DELETE FROM user WHERE user=?1", [&name])?;
        }
    }
    for user in &config.auth_users {
        let role = if user.admin { "admin" } else { "user" };
        tx.execute("INSERT INTO user(id,user,pass,role,sync_topic,provisioned,created) VALUES(?1,?2,?3,?4,?5,1,?6) ON CONFLICT(user) DO UPDATE SET pass=excluded.pass,role=excluded.role,provisioned=1", params![random_id("u_",12),user.name,user.password_hash,role,random_id("st_",16),now()])?;
        if user.admin {
            tx.execute("DELETE FROM user_access WHERE user_id=(SELECT id FROM user WHERE user=?1) OR owner_user_id=(SELECT id FROM user WHERE user=?1)", [&user.name])?;
        }
    }
    tx.execute("DELETE FROM user_access WHERE provisioned=1", [])?;
    for acl in &config.auth_access {
        let (read, write) =
            permission(&acl.permission).ok_or_else(|| Error::bad("invalid permission"))?;
        let pattern = sql_pattern(&acl.topic)?;
        let id: String = tx.query_row("SELECT id FROM user WHERE user=?1", [&acl.user], |r| {
            r.get(0)
        })?;
        tx.execute(
            "DELETE FROM user_access WHERE owner_user_id=?1 AND topic=?2",
            params![id, pattern],
        )?;
        tx.execute("INSERT INTO user_access(user_id,topic,read,write,owner_user_id,provisioned) VALUES(?1,?2,?3,?4,NULL,1) ON CONFLICT(user_id,topic) DO UPDATE SET read=excluded.read,write=excluded.write,owner_user_id=NULL,provisioned=1", params![id,pattern,read,write])?;
    }
    let previous = tx
        .prepare("SELECT token FROM user_token WHERE provisioned=1")?
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for token in previous {
        if !config.auth_tokens.iter().any(|t| t.token == token) {
            tx.execute("DELETE FROM user_token WHERE token=?1", [token])?;
        }
    }
    for token in &config.auth_tokens {
        let id: String = tx.query_row("SELECT id FROM user WHERE user=?1", [&token.user], |r| {
            r.get(0)
        })?;
        tx.execute("INSERT INTO user_token(user_id,token,label,last_access,last_origin,expires,provisioned) VALUES(?1,?2,?3,0,'0.0.0.0',0,1) ON CONFLICT(token) DO UPDATE SET user_id=excluded.user_id,label=excluded.label,expires=0,provisioned=1", params![id,token.token,token.label])?;
    }
    tx.commit()?;
    Ok(())
}
