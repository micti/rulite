//! Supabase-style auth: the `auth_*` tables, sessions and refresh tokens as
//! GoTrue keeps them, the `/auth/v1` routes, and the role a `/rest/v1`
//! request runs as.
//!
//! JSON for users and token claims is built by SQLite; Rust only signs.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use axum::Router;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, HeaderName, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use rusqlite::{Connection, ErrorCode, OptionalExtension, TransactionBehavior};
use serde_json::Value;
use tokio::sync::Semaphore;

use crate::config;
use crate::db::DbError;
use crate::jwt::{self, JwtError, Key};
use crate::schema::Schema;
use crate::server::App;
use crate::sql::{Grant, Policy};

/// Created on first use; `schema.rs` keeps them out of `/rest/v1`.
pub const TABLES: &str = "
CREATE TABLE IF NOT EXISTS auth_users (
  id TEXT PRIMARY KEY NOT NULL,
  aud TEXT NOT NULL DEFAULT 'authenticated',
  role TEXT NOT NULL DEFAULT 'authenticated',
  email TEXT NOT NULL UNIQUE COLLATE NOCASE,
  encrypted_password TEXT NOT NULL,
  email_confirmed_at TEXT,
  last_sign_in_at TEXT,
  raw_app_meta_data TEXT NOT NULL DEFAULT '{\"provider\":\"email\",\"providers\":[\"email\"]}',
  raw_user_meta_data TEXT NOT NULL DEFAULT '{}',
  created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
  updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);
CREATE TABLE IF NOT EXISTS auth_sessions (
  id TEXT PRIMARY KEY NOT NULL,
  user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
  aal TEXT NOT NULL DEFAULT 'aal1',
  created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
  updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
  refreshed_at TEXT
);
CREATE INDEX IF NOT EXISTS auth_sessions_user_id ON auth_sessions (user_id);
CREATE TABLE IF NOT EXISTS auth_refresh_tokens (
  id INTEGER PRIMARY KEY,
  token TEXT NOT NULL UNIQUE,
  user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
  session_id TEXT NOT NULL REFERENCES auth_sessions(id) ON DELETE CASCADE,
  parent TEXT,
  revoked INTEGER NOT NULL DEFAULT 0,
  created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
  updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);
CREATE INDEX IF NOT EXISTS auth_refresh_tokens_session_id ON auth_refresh_tokens (session_id);
";

/// GoTrue's bcrypt cost.
const COST: u32 = 10;
/// A cost-10 hash of a random password: an unknown email is checked against
/// it, so it takes as long to reject as a wrong password.
const DUMMY_HASH: &str = "$2a$10$Dxg1eN0FlNgItx/zvLaxr.EKNxvBcx47/Ie2bZNzu0fJU5qbqt0jq";
const API_VERSION: (HeaderName, &str) = (HeaderName::from_static("x-supabase-api-version"), "2024-01-01");
const JSON: &str = "application/json; charset=utf-8";

macro_rules! now {
    () => {
        "strftime('%Y-%m-%dT%H:%M:%fZ', 'now')"
    };
}

/// GoTrue's user object for the `auth_users` row `u`.
macro_rules! user_json {
    () => {
        "json_object('id', u.id, 'aud', u.aud, 'role', u.role, 'email', u.email, \
         'email_confirmed_at', u.email_confirmed_at, 'phone', '', 'confirmed_at', u.email_confirmed_at, \
         'last_sign_in_at', u.last_sign_in_at, 'app_metadata', json(u.raw_app_meta_data), \
         'user_metadata', json(u.raw_user_meta_data), 'identities', json('[]'), \
         'created_at', u.created_at, 'updated_at', u.updated_at, 'is_anonymous', json('false'))"
    };
}

/// Access-token claims (in GoTrue's order) and the user, for session `?4`.
const SESSION: &str = concat!(
    "SELECT json_object('iss', ?1, 'sub', u.id, 'aud', u.aud, 'exp', ?2, 'iat', ?3, 'email', u.email, 'phone', '', \
     'app_metadata', json(u.raw_app_meta_data), 'user_metadata', json(u.raw_user_meta_data), 'role', u.role, \
     'aal', s.aal, 'amr', json_array(json_object('method', 'password', 'timestamp', unixepoch(s.created_at))), \
     'session_id', s.id, 'is_anonymous', json('false')), ",
    user_json!(),
    " FROM auth_sessions AS s JOIN auth_users AS u ON u.id = s.user_id WHERE s.id = ?4"
);

/// The user `?1` and whether session `?2` (if any) is still theirs.
const USER: &str = concat!(
    "SELECT ",
    user_json!(),
    ", ?2 IS NULL OR EXISTS (SELECT 1 FROM auth_sessions WHERE id = ?2 AND user_id = u.id) \
     FROM auth_users AS u WHERE u.id = ?1"
);

pub fn ensure_tables(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(TABLES)
}

/// A bcrypt hash in GoTrue's `$2a$10$` format.
pub fn hash_password(password: &str) -> String {
    bcrypt::hash_with_result(password, COST).expect("cost 10 is valid").format_for_version(bcrypt::Version::TwoA)
}

fn verify_password(password: &str, hash: &str) -> bool {
    bcrypt::verify(password, hash).unwrap_or(false)
}

/// Auth state for a server with a JWT secret.
pub struct Auth {
    key: Key,
    /// `<external_url>/auth/v1`.
    iss: String,
    jwt_exp: u64,
    reuse_interval: u64,
    password_min_length: usize,
    anon: Arc<Policy>,
    authenticated: Arc<Policy>,
    access: crate::access::Access,
    /// Schema version the config names were last checked against.
    checked: AtomicI64,
    /// bcrypt runs one at a time, off the HTTP thread.
    hashing: Semaphore,
}

/// Who a `/rest/v1` request runs as.
#[derive(Debug, PartialEq)]
pub enum Role {
    Anon,
    /// Holds the user id (`sub`).
    Authenticated(String),
    Service,
}

/// Why a `/rest/v1` request has no role.
#[derive(Debug, PartialEq)]
pub enum RoleError {
    Missing,
    Invalid,
    Expired,
}

impl Auth {
    pub fn new(config: &config::Auth, secret: &str, base_url: &str) -> Auth {
        Auth {
            key: Key::new(secret.as_bytes()),
            iss: format!("{base_url}/auth/v1"),
            jwt_exp: config.jwt_exp,
            reuse_interval: config.refresh_token_reuse_interval,
            password_min_length: config.password_min_length,
            anon: Arc::new(config.access.anon.clone()),
            authenticated: Arc::new(config.access.authenticated.clone()),
            access: config.access.clone(),
            checked: AtomicI64::new(i64::MIN),
            hashing: Semaphore::new(1),
        }
    }

    /// Logs config names `schema` lacks, once per schema version.
    pub fn check_schema(&self, schema: &Schema) {
        if self.checked.swap(schema.version, Ordering::Relaxed) != schema.version {
            for w in self.access.warnings(schema) {
                eprintln!("rulite: warning: {w}");
            }
        }
    }

    /// The token from `Authorization: Bearer`, else `apikey`, verified.
    pub fn role(&self, headers: &HeaderMap) -> Result<Role, RoleError> {
        let token = bearer(headers).or_else(|| headers.get("apikey").and_then(|v| v.to_str().ok()));
        let claims = self.key.verify(token.ok_or(RoleError::Missing)?, jwt::now()).map_err(|e| match e {
            JwtError::Invalid => RoleError::Invalid,
            JwtError::Expired => RoleError::Expired,
        })?;
        match (claims["role"].as_str(), claims["sub"].as_str()) {
            (Some("anon"), _) => Ok(Role::Anon),
            (Some("service_role"), _) => Ok(Role::Service),
            (Some("authenticated"), Some(sub)) => Ok(Role::Authenticated(sub.to_string())),
            _ => Err(RoleError::Invalid),
        }
    }

    pub fn grant(&self, role: Role) -> Grant {
        match role {
            Role::Anon => Grant { policy: Some(self.anon.clone()), uid: None },
            Role::Authenticated(uid) => Grant { policy: Some(self.authenticated.clone()), uid: Some(uid) },
            Role::Service => Grant::default(),
        }
    }

    /// Runs bcrypt on the blocking pool, one hash at a time.
    async fn bcrypt<T: Send + 'static>(&self, f: impl FnOnce() -> T + Send + 'static) -> T {
        let _permit = self.hashing.acquire().await.expect("the semaphore is never closed");
        tokio::task::spawn_blocking(f).await.expect("bcrypt doesn't panic")
    }

    fn weak(&self, password: &str) -> Option<Error> {
        (password.chars().count() < self.password_min_length).then(|| Error {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            code: "weak_password",
            msg: format!("Password should be at least {} characters.", self.password_min_length),
        })
    }

    /// The session JSON for `session` with refresh token `refresh`: a new
    /// access token plus the user.
    fn issue(&self, conn: &Connection, session: &str, refresh: &str) -> rusqlite::Result<String> {
        let iat = jwt::now();
        let exp = iat + self.jwt_exp;
        let (claims, user): (String, String) = conn
            .prepare_cached(SESSION)?
            .query_row((&self.iss, exp as i64, iat as i64, session), |r| Ok((r.get(0)?, r.get(1)?)))?;
        let access = self.key.sign(&claims);
        Ok(format!(
            r#"{{"access_token":"{access}","token_type":"bearer","expires_in":{},"expires_at":{exp},"refresh_token":"{refresh}","user":{user}}}"#,
            self.jwt_exp
        ))
    }
}

fn bearer(headers: &HeaderMap) -> Option<&str> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = value.split_once(' ')?;
    scheme.eq_ignore_ascii_case("bearer").then(|| token.trim())
}

/// A new session for `user` (sign-in); `None` if the user is gone.
fn create_session(conn: &mut Connection, auth: &Auth, user: &str) -> rusqlite::Result<Option<String>> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let signed_in = tx
        .prepare_cached(concat!(
            "UPDATE auth_users SET last_sign_in_at = ",
            now!(),
            ", updated_at = ",
            now!(),
            " WHERE id = ?1"
        ))?
        .execute([user])?;
    if signed_in == 0 {
        return Ok(None);
    }
    let session = jwt::uuid();
    tx.prepare_cached("INSERT INTO auth_sessions (id, user_id) VALUES (?1, ?2)")?.execute((&session, user))?;
    let refresh = new_refresh_token(&tx, user, &session, None)?;
    let out = auth.issue(&tx, &session, &refresh)?;
    tx.commit()?;
    Ok(Some(out))
}

fn new_refresh_token(conn: &Connection, user: &str, session: &str, parent: Option<&str>) -> rusqlite::Result<String> {
    let token = jwt::refresh_token();
    conn.prepare_cached(
        "INSERT INTO auth_refresh_tokens (token, user_id, session_id, parent) VALUES (?1, ?2, ?3, ?4)",
    )?
    .execute((&token, user, session, parent))?;
    Ok(token)
}

/// Rotates `token`: a fresh one with `parent = token`, the old one revoked.
/// A revoked token within the reuse interval gets the session's current
/// token; later, the whole session is revoked.
fn rotate(conn: &mut Connection, auth: &Auth, token: &str) -> rusqlite::Result<Result<String, Error>> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let row: Option<(i64, String, String, bool, bool)> = tx
        .prepare_cached(
            "SELECT id, user_id, session_id, revoked, \
             unixepoch('now', 'subsec') - unixepoch(updated_at, 'subsec') < ?2 \
             FROM auth_refresh_tokens WHERE token = ?1",
        )?
        .query_row((token, auth.reuse_interval as i64), |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))
        .optional()?;
    let Some((id, user, session, revoked, recent)) = row else {
        return Ok(Err(refresh_not_found()));
    };
    let out = if !revoked {
        let fresh = new_refresh_token(&tx, &user, &session, Some(token))?;
        tx.prepare_cached(concat!(
            "UPDATE auth_refresh_tokens SET revoked = 1, updated_at = ",
            now!(),
            " WHERE id = ?1"
        ))?
        .execute([id])?;
        tx.prepare_cached(concat!(
            "UPDATE auth_sessions SET refreshed_at = ",
            now!(),
            ", updated_at = ",
            now!(),
            " WHERE id = ?1"
        ))?
        .execute([&session])?;
        Ok(auth.issue(&tx, &session, &fresh)?)
    } else {
        let active: Option<String> = match recent {
            true => tx
                .prepare_cached("SELECT token FROM auth_refresh_tokens WHERE session_id = ?1 AND NOT revoked ORDER BY id DESC LIMIT 1")?
                .query_row([&session], |r| r.get(0))
                .optional()?,
            false => None,
        };
        match active {
            Some(current) => Ok(auth.issue(&tx, &session, &current)?),
            None => {
                tx.prepare_cached("DELETE FROM auth_sessions WHERE id = ?1")?.execute([&session])?;
                Err(Error::new(
                    StatusCode::BAD_REQUEST,
                    "refresh_token_already_used",
                    "Invalid Refresh Token: Already Used",
                ))
            }
        }
    };
    tx.commit()?;
    Ok(out)
}

/// The user JSON of `claims`, if the user and their session still exist.
fn load_user(conn: &Connection, claims: &Claims) -> rusqlite::Result<Result<String, Error>> {
    let row: Option<(String, bool)> = conn
        .prepare_cached(USER)?
        .query_row((&claims.uid, &claims.session), |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()?;
    Ok(match row {
        None => Err(Error::new(StatusCode::FORBIDDEN, "user_not_found", "User from sub claim in JWT does not exist")),
        Some((_, false)) => Err(Error::new(
            StatusCode::FORBIDDEN,
            "session_not_found",
            "Session from session_id claim in JWT does not exist",
        )),
        Some((user, true)) => Ok(user),
    })
}

/// What `PUT /user` changes; `None` leaves a field as it is.
struct Changes {
    email: Option<String>,
    hash: Option<String>,
    /// JSON object merged into `user_metadata` (`null` removes a key).
    data: Option<String>,
}

/// Applies `changes`; a new password signs out the user's other sessions.
fn update_user(conn: &mut Connection, claims: &Claims, changes: &Changes) -> rusqlite::Result<Result<String, Error>> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if let Err(e) = load_user(&tx, claims)? {
        return Ok(Err(e));
    }
    let updated = tx
        .prepare_cached(concat!(
            "UPDATE auth_users SET email = coalesce(?2, email), encrypted_password = coalesce(?3, encrypted_password), \
             raw_user_meta_data = iif(?4 IS NULL, raw_user_meta_data, json_patch(raw_user_meta_data, ?4)), \
             updated_at = ",
            now!(),
            " WHERE id = ?1"
        ))?
        .execute((&claims.uid, &changes.email, &changes.hash, &changes.data));
    match updated {
        Err(rusqlite::Error::SqliteFailure(f, _)) if f.code == ErrorCode::ConstraintViolation => {
            return Ok(Err(Error::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "email_exists",
                "A user with this email address has already been registered",
            )));
        }
        r => r?,
    };
    if changes.hash.is_some() {
        tx.prepare_cached("DELETE FROM auth_sessions WHERE user_id = ?1 AND id IS NOT ?2")?
            .execute((&claims.uid, &claims.session))?;
    }
    let user = load_user(&tx, claims)?;
    tx.commit()?;
    Ok(user)
}

// --- /auth/v1 ---

pub fn routes() -> Router<Arc<App>> {
    Router::new()
        .route("/token", post(token))
        .route("/user", get(get_user).put(put_user))
        .route("/logout", post(logout))
        .route("/signup", post(signup))
        .route("/settings", get(settings))
        .fallback(not_found)
}

/// GoTrue's error: `{code: <status>, error_code, msg}`.
#[derive(Debug)]
pub struct Error {
    status: StatusCode,
    code: &'static str,
    msg: String,
}

impl Error {
    fn new(status: StatusCode, code: &'static str, msg: &str) -> Error {
        Error { status, code, msg: msg.to_string() }
    }
}

impl From<DbError> for Error {
    fn from(e: DbError) -> Error {
        Error { status: StatusCode::INTERNAL_SERVER_ERROR, code: "unexpected_failure", msg: e.to_string() }
    }
}

impl IntoResponse for Error {
    fn into_response(self) -> Response {
        let mut body = serde_json::json!({ "code": self.status.as_u16(), "error_code": self.code, "msg": self.msg });
        if self.code == "weak_password" {
            body["weak_password"] = serde_json::json!({ "reasons": ["length"] });
        }
        reply(self.status, body.to_string())
    }
}

type Reply = Result<Response, Error>;

/// Every `/auth/v1` response says which API version it speaks, or
/// supabase-js won't read `error_code`.
fn reply(status: StatusCode, body: String) -> Response {
    (status, [(header::CONTENT_TYPE, JSON), API_VERSION], body).into_response()
}

fn validation(msg: &str) -> Error {
    Error::new(StatusCode::BAD_REQUEST, "validation_failed", msg)
}

fn refresh_not_found() -> Error {
    Error::new(StatusCode::BAD_REQUEST, "refresh_token_not_found", "Invalid Refresh Token: Refresh Token Not Found")
}

/// Auth state, after checking that `apikey` is a valid anon or service_role key.
fn api_key(app: &App, headers: &HeaderMap) -> Result<Arc<Auth>, Error> {
    let auth = app.auth.clone().expect("/auth/v1 is only routed with auth on");
    let Some(key) = headers.get("apikey").and_then(|v| v.to_str().ok()) else {
        return Err(Error::new(StatusCode::UNAUTHORIZED, "no_api_key", "No API key found in request"));
    };
    match auth.key.verify(key, jwt::now()) {
        Ok(claims) if matches!(claims["role"].as_str(), Some("anon" | "service_role")) => Ok(auth),
        _ => Err(Error::new(StatusCode::UNAUTHORIZED, "bad_api_key", "Invalid API key")),
    }
}

/// The signed-in user behind `Authorization: Bearer`.
struct Claims {
    uid: String,
    session: Option<String>,
}

fn claims(auth: &Auth, headers: &HeaderMap) -> Result<Claims, Error> {
    let token = bearer(headers).ok_or_else(|| {
        Error::new(StatusCode::UNAUTHORIZED, "no_authorization", "This endpoint requires a Bearer token")
    })?;
    let claims = auth.key.verify(token, jwt::now()).map_err(|e| {
        let msg = match e {
            JwtError::Invalid => "invalid JWT: unable to parse or verify signature",
            JwtError::Expired => {
                "invalid JWT: unable to parse or verify signature, token has invalid claims: token is expired"
            }
        };
        Error::new(StatusCode::FORBIDDEN, "bad_jwt", msg)
    })?;
    match (claims["role"].as_str(), claims["sub"].as_str()) {
        (Some("authenticated"), Some(sub)) => {
            Ok(Claims { uid: sub.to_string(), session: claims["session_id"].as_str().map(str::to_string) })
        }
        _ => Err(Error::new(StatusCode::FORBIDDEN, "bad_jwt", "invalid claim: missing sub claim")),
    }
}

fn json_body(body: &str) -> Result<Value, Error> {
    match serde_json::from_str::<Value>(body) {
        Ok(v) if v.is_object() => Ok(v),
        _ => Err(Error::new(StatusCode::BAD_REQUEST, "bad_json", "Could not parse request body as JSON")),
    }
}

fn session_reply(session: String) -> Response {
    reply(StatusCode::OK, session)
}

async fn token(
    State(app): State<Arc<App>>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
    body: String,
) -> Reply {
    let auth = api_key(&app, &headers)?;
    let body = json_body(&body)?;
    match query.get("grant_type").map(String::as_str) {
        Some("password") => sign_in(&app, auth, &body).await,
        Some("refresh_token") => {
            let token = body["refresh_token"].as_str().ok_or_else(refresh_not_found)?.to_string();
            let session = app.db.write(move |conn, _| rotate(conn, &auth, &token)).await??;
            Ok(session_reply(session))
        }
        _ => Err(validation("unsupported_grant_type")),
    }
}

async fn sign_in(app: &App, auth: Arc<Auth>, body: &Value) -> Reply {
    let email = body["email"].as_str().filter(|e| !e.is_empty()).ok_or_else(|| validation("missing email or phone"))?;
    let password = body["password"].as_str().unwrap_or_default().to_string();
    let email = email.to_string();
    let found: Option<(String, String)> = app
        .db
        .read(move |conn, _| {
            conn.prepare_cached("SELECT id, encrypted_password FROM auth_users WHERE email = ?1")?
                .query_row([email], |r| Ok((r.get(0)?, r.get(1)?)))
                .optional()
        })
        .await?;
    let (user, hash) = found.unzip();
    let ok = auth.bcrypt(move || verify_password(&password, hash.as_deref().unwrap_or(DUMMY_HASH))).await;
    let invalid = || Error::new(StatusCode::BAD_REQUEST, "invalid_credentials", "Invalid login credentials");
    let user = user.filter(|_| ok).ok_or_else(invalid)?;
    let session = app.db.write(move |conn, _| create_session(conn, &auth, &user)).await?;
    session.map(session_reply).ok_or_else(invalid)
}

async fn get_user(State(app): State<Arc<App>>, headers: HeaderMap) -> Reply {
    let auth = api_key(&app, &headers)?;
    let claims = claims(&auth, &headers)?;
    let user = app.db.read(move |conn, _| load_user(conn, &claims)).await??;
    Ok(reply(StatusCode::OK, user))
}

async fn put_user(State(app): State<Arc<App>>, headers: HeaderMap, body: String) -> Reply {
    let auth = api_key(&app, &headers)?;
    let claims = claims(&auth, &headers)?;
    let body = json_body(&body)?;
    let text = |key: &str| match &body[key] {
        Value::Null => Ok(None),
        Value::String(s) => Ok(Some(s.clone())),
        _ => Err(validation(&format!("{key} must be a string"))),
    };
    let email = text("email")?.map(|e| e.to_lowercase());
    if email.as_deref().is_some_and(|e| !valid_email(e)) {
        return Err(validation("Unable to validate email address: invalid format"));
    }
    let data = match &body["data"] {
        Value::Null => None,
        d @ Value::Object(_) => Some(d.to_string()),
        _ => return Err(validation("data must be an object")),
    };
    let hash = match text("password")? {
        Some(p) => match auth.weak(&p) {
            Some(e) => return Err(e),
            None => Some(auth.bcrypt(move || hash_password(&p)).await),
        },
        None => None,
    };
    let changes = Changes { email, hash, data };
    let user = app.db.write(move |conn, _| update_user(conn, &claims, &changes)).await??;
    Ok(reply(StatusCode::OK, user))
}

async fn logout(
    State(app): State<Arc<App>>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Reply {
    let auth = api_key(&app, &headers)?;
    let claims = claims(&auth, &headers)?;
    let sql = match query.get("scope").map_or("global", String::as_str) {
        "global" => "DELETE FROM auth_sessions WHERE user_id = ?1 OR id = ?2",
        "local" => "DELETE FROM auth_sessions WHERE id = ?2 AND user_id = ?1",
        "others" => "DELETE FROM auth_sessions WHERE user_id = ?1 AND id IS NOT ?2",
        _ => return Err(validation("Unsupported logout scope")),
    };
    app.db.write(move |conn, _| conn.prepare_cached(sql)?.execute((&claims.uid, &claims.session))).await?;
    Ok((StatusCode::NO_CONTENT, [API_VERSION]).into_response())
}

async fn signup(State(app): State<Arc<App>>, headers: HeaderMap) -> Reply {
    api_key(&app, &headers)?;
    Err(Error::new(StatusCode::UNPROCESSABLE_ENTITY, "signup_disabled", "Signups not allowed for this instance"))
}

async fn settings(State(app): State<Arc<App>>, headers: HeaderMap) -> Reply {
    api_key(&app, &headers)?;
    let body = r#"{"external":{"anonymous_users":false,"email":true,"phone":false},"disable_signup":true,"mailer_autoconfirm":true,"phone_autoconfirm":false,"sms_provider":"","saml_enabled":false}"#;
    Ok(reply(StatusCode::OK, body.to_string()))
}

async fn not_found() -> Response {
    Error::new(StatusCode::NOT_FOUND, "not_found", "Not found").into_response()
}

/// Something `@` something, without spaces.
pub fn valid_email(email: &str) -> bool {
    matches!(email.split_once('@'), Some((user, domain)) if !user.is_empty() && !domain.is_empty() && !email.contains(char::is_whitespace))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const SECRET: &str = "0123456789abcdef0123456789abcdef";

    fn setup(reuse_interval: u64) -> (Connection, Auth, String) {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        ensure_tables(&conn).unwrap();
        ensure_tables(&conn).unwrap(); // idempotent
        conn.execute(
            "INSERT INTO auth_users (id, email, encrypted_password) VALUES ('u1', 'alice@example.com', ?1)",
            [hash_password("secret1")],
        )
        .unwrap();
        let config = config::Auth { refresh_token_reuse_interval: reuse_interval, ..config::Auth::default() };
        (conn, Auth::new(&config, SECRET, "http://127.0.0.1:3000"), "u1".into())
    }

    fn session(json: &str) -> Value {
        serde_json::from_str(json).unwrap()
    }

    fn refresh(conn: &mut Connection, auth: &Auth, token: &Value) -> Result<Value, &'static str> {
        rotate(conn, auth, token.as_str().unwrap()).unwrap().map(|s| session(&s)).map_err(|e| e.code)
    }

    #[test]
    fn passwords() {
        let hash = hash_password("secret1");
        assert!(hash.starts_with("$2a$10$") && hash.len() == 60);
        assert!(verify_password("secret1", &hash));
        assert!(!verify_password("secret2", &hash));
        assert!(DUMMY_HASH.starts_with("$2a$10$"));
        assert_eq!(bcrypt::verify("", DUMMY_HASH).ok(), Some(false)); // a real hash: rejecting costs as much
        assert!(!verify_password("x", "not a hash"));
    }

    #[test]
    fn session_and_claims_follow_gotrue() {
        let (mut conn, auth, user) = setup(10);
        let s = session(&create_session(&mut conn, &auth, &user).unwrap().unwrap());
        assert_eq!(s["token_type"], "bearer");
        assert_eq!(s["expires_in"], 3600);
        let refresh = s["refresh_token"].as_str().unwrap();
        assert!(refresh.len() == 12 && refresh.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()));
        assert_eq!(s["user"]["email"], "alice@example.com");
        assert!(s["user"]["last_sign_in_at"].is_string());
        assert_eq!(s["user"]["app_metadata"], json!({ "provider": "email", "providers": ["email"] }));

        let token = s["access_token"].as_str().unwrap();
        let payload = String::from_utf8(jwt::decode(token.split('.').nth(1).unwrap()).unwrap()).unwrap();
        let keys: Vec<String> = {
            let v: Value = serde_json::from_str(&payload).unwrap();
            let mut keys: Vec<(usize, String)> = v
                .as_object()
                .unwrap()
                .keys()
                .map(|k| (payload.find(&format!("\"{k}\":")).unwrap(), k.clone()))
                .collect();
            keys.sort();
            keys.into_iter().map(|(_, k)| k).collect()
        };
        let order = [
            "iss",
            "sub",
            "aud",
            "exp",
            "iat",
            "email",
            "phone",
            "app_metadata",
            "user_metadata",
            "role",
            "aal",
            "amr",
            "session_id",
            "is_anonymous",
        ];
        assert_eq!(keys, order);
        let claims = auth.key.verify(token, jwt::now()).unwrap();
        assert_eq!(claims["iss"], "http://127.0.0.1:3000/auth/v1");
        assert_eq!(
            (claims["sub"].as_str(), claims["role"].as_str(), claims["aal"].as_str()),
            (Some("u1"), Some("authenticated"), Some("aal1"))
        );
        assert_eq!(claims["exp"].as_u64().unwrap() - claims["iat"].as_u64().unwrap(), 3600);
        assert_eq!(s["expires_at"], claims["exp"]);
        assert_eq!(claims["amr"][0]["method"], "password");
        assert!(claims["amr"][0]["timestamp"].is_u64());
        assert_eq!(claims["is_anonymous"], false);

        // Unknown user: no session.
        assert_eq!(create_session(&mut conn, &auth, "nope").unwrap(), None);
    }

    #[test]
    fn refresh_rotation_and_reuse() {
        let (mut conn, auth, user) = setup(10);
        let first = session(&create_session(&mut conn, &auth, &user).unwrap().unwrap());
        let second = refresh(&mut conn, &auth, &first["refresh_token"]).unwrap();
        assert_ne!(second["refresh_token"], first["refresh_token"]);
        let parent: String = conn
            .query_row(
                "SELECT parent FROM auth_refresh_tokens WHERE token = ?1",
                [second["refresh_token"].as_str()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(parent, first["refresh_token"].as_str().unwrap());

        // Reused within the interval: the current token comes back.
        let again = refresh(&mut conn, &auth, &first["refresh_token"]).unwrap();
        assert_eq!(again["refresh_token"], second["refresh_token"]);
        assert_eq!(refresh(&mut conn, &auth, &json!("nope00000000")), Err("refresh_token_not_found"));

        // Reused later: the whole session is revoked.
        conn.execute("UPDATE auth_refresh_tokens SET updated_at = '2000-01-01T00:00:00.000Z' WHERE revoked", [])
            .unwrap();
        assert_eq!(refresh(&mut conn, &auth, &first["refresh_token"]), Err("refresh_token_already_used"));
        assert_eq!(refresh(&mut conn, &auth, &second["refresh_token"]), Err("refresh_token_not_found"));
        let sessions: i64 = conn.query_row("SELECT count(*) FROM auth_sessions", [], |r| r.get(0)).unwrap();
        assert_eq!(sessions, 0);

        // No reuse interval: any reuse revokes.
        let (mut conn, auth, user) = setup(0);
        let first = session(&create_session(&mut conn, &auth, &user).unwrap().unwrap());
        refresh(&mut conn, &auth, &first["refresh_token"]).unwrap();
        assert_eq!(refresh(&mut conn, &auth, &first["refresh_token"]), Err("refresh_token_already_used"));
    }

    #[test]
    fn user_updates_and_sessions() {
        let (mut conn, auth, user) = setup(10);
        let a = session(&create_session(&mut conn, &auth, &user).unwrap().unwrap());
        session(&create_session(&mut conn, &auth, &user).unwrap().unwrap());
        let session = auth.key.verify(a["access_token"].as_str().unwrap(), jwt::now()).unwrap()["session_id"].clone();
        let claims = Claims { uid: user.clone(), session: session.as_str().map(str::to_string) };
        let changes = Changes { email: None, hash: None, data: Some(r#"{"name":"Alice","x":1}"#.into()) };
        let u: Value = serde_json::from_str(&update_user(&mut conn, &claims, &changes).unwrap().unwrap()).unwrap();
        assert_eq!(u["user_metadata"], json!({ "name": "Alice", "x": 1 }));
        let changes = Changes {
            email: Some("a2@example.com".into()),
            hash: Some(hash_password("secret2")),
            data: Some(r#"{"x":null}"#.into()),
        };
        let u: Value = serde_json::from_str(&update_user(&mut conn, &claims, &changes).unwrap().unwrap()).unwrap();
        assert_eq!((u["email"].as_str(), &u["user_metadata"]), (Some("a2@example.com"), &json!({ "name": "Alice" })));
        // The other session is gone, this one stays.
        let sessions: i64 = conn.query_row("SELECT count(*) FROM auth_sessions", [], |r| r.get(0)).unwrap();
        assert_eq!(sessions, 1);
        assert!(load_user(&conn, &claims).unwrap().is_ok());

        conn.execute(
            "INSERT INTO auth_users (id, email, encrypted_password) VALUES ('u2', 'bob@example.com', 'x')",
            [],
        )
        .unwrap();
        let taken = Changes { email: Some("BOB@example.com".into()), hash: None, data: None };
        assert_eq!(update_user(&mut conn, &claims, &taken).unwrap().unwrap_err().code, "email_exists");

        let gone = Claims { uid: user.clone(), session: Some("nope".into()) };
        assert_eq!(load_user(&conn, &gone).unwrap().unwrap_err().code, "session_not_found");
        conn.execute("DELETE FROM auth_users WHERE id = 'u1'", []).unwrap();
        assert_eq!(load_user(&conn, &claims).unwrap().unwrap_err().code, "user_not_found");
        let tokens: i64 = conn.query_row("SELECT count(*) FROM auth_refresh_tokens", [], |r| r.get(0)).unwrap();
        assert_eq!(tokens, 0); // cascaded
    }

    #[test]
    fn roles_from_headers() {
        let (_, auth, _) = setup(10);
        let now = jwt::now();
        let headers = |pairs: &[(&'static str, String)]| {
            let mut h = HeaderMap::new();
            for (k, v) in pairs {
                h.insert(*k, v.parse().unwrap());
            }
            h
        };
        let anon = jwt::api_key(&auth.key, "anon", now);
        let service = jwt::api_key(&auth.key, "service_role", now);
        let user = auth.key.sign(&format!(r#"{{"sub":"u1","role":"authenticated","exp":{}}}"#, now + 60));
        let expired = auth.key.sign(&format!(r#"{{"sub":"u1","role":"authenticated","exp":{}}}"#, now - 3600));
        let bearer = |t: &str| format!("Bearer {t}");

        assert_eq!(auth.role(&headers(&[])), Err(RoleError::Missing));
        assert_eq!(auth.role(&headers(&[("apikey", anon.clone())])), Ok(Role::Anon));
        assert_eq!(
            auth.role(&headers(&[("apikey", anon.clone()), ("authorization", bearer(&service))])),
            Ok(Role::Service)
        );
        assert_eq!(
            auth.role(&headers(&[("apikey", anon.clone()), ("authorization", bearer(&user))])),
            Ok(Role::Authenticated("u1".into()))
        );
        assert_eq!(auth.role(&headers(&[("authorization", bearer(&expired))])), Err(RoleError::Expired));
        assert_eq!(auth.role(&headers(&[("authorization", bearer("x.y.z"))])), Err(RoleError::Invalid));
        let other = Key::new(b"another secret of at least 32 bytes");
        assert_eq!(
            auth.role(&headers(&[("apikey", jwt::api_key(&other, "service_role", now))])),
            Err(RoleError::Invalid)
        );
        let no_sub = auth.key.sign(r#"{"role":"authenticated"}"#);
        assert_eq!(auth.role(&headers(&[("apikey", no_sub)])), Err(RoleError::Invalid));
        assert_eq!(
            auth.role(&headers(&[("apikey", auth.key.sign(r#"{"role":"postgres"}"#))])),
            Err(RoleError::Invalid)
        );

        assert!(auth.grant(Role::Service).policy.is_none());
        assert_eq!(auth.grant(Role::Authenticated("u1".into())).uid.as_deref(), Some("u1"));
    }

    #[test]
    fn emails() {
        assert!(valid_email("a@b.dev"));
        for bad in ["", "a", "@b", "a@", "a b@c.d"] {
            assert!(!valid_email(bad), "{bad}");
        }
    }
}
