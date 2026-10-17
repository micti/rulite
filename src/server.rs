//! HTTP layer: routes under `/rest/v1` and `/auth/v1`, the request's role,
//! PostgREST-shaped errors.

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Bytes;
use axum::extract::{Path, RawQuery, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use rusqlite::ErrorCode;
use rusqlite::ffi;
use tokio::net::TcpListener;

use crate::auth::{self, Auth, Role, RoleError};
use crate::config::Config;
use crate::cors;
use crate::db::{Db, DbError};
use crate::openapi;
use crate::query::{self, Headers, ParseError};
use crate::schema::Schema;
use crate::sql::{self, BuildError, Grant, Limits, Page};
use crate::typescript;
use crate::write::{self, Action, Output, Write};

const JSON: &str = "application/json; charset=utf-8";
const OBJECT: &str = "application/vnd.pgrst.object+json; charset=utf-8";
const OPENAPI: &str = "application/openapi+json; charset=utf-8";
const TYPESCRIPT: &str = "text/plain; charset=utf-8";

/// A document built from the schema and the schema version it was built from.
type Cache = Arc<Mutex<Option<(i64, Bytes)>>>;

pub struct App {
    pub db: Db,
    limits: Limits,
    openapi: Cache,
    typescript: Cache,
    pub auth: Arc<Auth>,
}

pub async fn run(config: &Config, db: Db) -> std::io::Result<()> {
    #[cfg(feature = "tls")]
    let tls = match &config.tls {
        Some(tls) => Some(crate::tls::server_config(&tls.cert, &tls.key).map_err(std::io::Error::other)?),
        None => None,
    };
    let limits = Limits { default: config.default_limit, max: config.max_limit };
    let listener = TcpListener::bind(config.bind).await?;
    let addr = listener.local_addr()?;
    let scheme = if config.tls.is_some() { "https" } else { "http" };
    let secret = config.auth.jwt_secret.as_deref().expect("checked by the config");
    let base = config.auth.external_url.clone().unwrap_or_else(|| format!("{scheme}://{addr}"));
    let auth = Arc::new(Auth::new(&config.auth, secret, &base));
    if !config.auth.access.granted() {
        eprintln!("rulite: warning: no table is granted to anon or authenticated; only the service_role key works");
    }

    let router = Router::new()
        .route("/rest/v1", get(openapi_document))
        .route("/rest/v1/", get(openapi_document))
        .route("/rest/v1/{table}", get(read_table).post(insert).patch(update).delete(delete))
        .route("/pg/generators/typescript", get(typescript_types))
        .nest("/auth/v1", auth::routes());
    let state =
        Arc::new(App { db, limits, openapi: Cache::default(), typescript: Cache::default(), auth: auth.clone() });
    let checked = state.db.read(move |_, schema| {
        auth.check_schema(schema);
        Ok(())
    });
    checked.await.map_err(|e| std::io::Error::other(e.to_string()))?;
    let mut app = router.fallback(not_found).with_state(state);
    if let Some(origins) = &config.cors {
        app = app.layer(middleware::from_fn_with_state(Arc::new(origins.clone()), cors::layer));
    }

    if config.tls.is_none() && !addr.ip().is_loopback() {
        eprintln!("rulite: serving plain HTTP on {addr}; put an HTTPS proxy in front (see README)");
    }
    eprintln!("rulite: serving {} on {scheme}://{addr}", config.db_path.display());
    #[cfg(feature = "tls")]
    if let Some(tls) = tls {
        let listener = crate::tls::TlsListener::new(listener, tls)?;
        return axum::serve(listener, app).with_graceful_shutdown(shutdown_signal()).await;
    }
    axum::serve(listener, app).with_graceful_shutdown(shutdown_signal()).await
}

/// The request's role and grant.
struct Caller {
    grant: Grant,
    /// Denials answer `401` for anon, `403` otherwise.
    anon: bool,
    service: bool,
    auth: Arc<Auth>,
}

impl Caller {
    fn of(app: &App, headers: &HeaderMap) -> Result<Caller, Response> {
        let role = app.auth.role(headers).map_err(jwt_error)?;
        let (anon, service) = (role == Role::Anon, role == Role::Service);
        Ok(Caller { grant: app.auth.grant(role), anon, service, auth: app.auth.clone() })
    }

    /// Logs config names the schema lacks when the schema moved.
    fn check(&self, schema: &Schema) {
        self.auth.check_schema(schema);
    }

    fn build_error(&self, e: BuildError) -> Response {
        let denied = matches!(e, BuildError::PermissionDenied(_) | BuildError::RowPolicy(_));
        let mut r = build_error(e);
        if denied && self.anon {
            *r.status_mut() = StatusCode::UNAUTHORIZED;
        }
        r
    }
}

fn jwt_error(e: RoleError) -> Response {
    let (code, message) = match e {
        RoleError::Missing => ("PGRST302", "Anonymous access is disabled"),
        RoleError::Invalid => ("PGRST301", "No suitable key or wrong key type"),
        RoleError::Expired => ("PGRST303", "JWT expired"),
    };
    let mut r = error(StatusCode::UNAUTHORIZED, code, message, None, None);
    r.headers_mut().insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
    r
}

/// `Prefer` (every value), `Range` and `Accept`; values that aren't UTF-8 are ignored.
fn parse_headers(headers: &HeaderMap) -> Result<Headers, ParseError> {
    let one = |name| headers.get(name).and_then(|v| v.to_str().ok());
    Headers::parse(
        headers.get_all("prefer").iter().filter_map(|v| v.to_str().ok()),
        one(header::RANGE),
        one(header::ACCEPT),
    )
}

/// For `service_role` only.
async fn openapi_document(State(app): State<Arc<App>>, headers: HeaderMap) -> Response {
    let cache = app.openapi.clone();
    schema_document(&app, &headers, "the OpenAPI document", cache, openapi::document, OPENAPI).await
}

/// `supabase gen types typescript` output, as postgres-meta serves it; for `service_role` only.
async fn typescript_types(State(app): State<Arc<App>>, headers: HeaderMap) -> Response {
    let cache = app.typescript.clone();
    schema_document(&app, &headers, "the TypeScript types", cache, typescript::generate, TYPESCRIPT).await
}

/// A document that reveals the whole schema: `service_role` only (anon `401`,
/// authenticated `403`), rebuilt only when the schema version moves.
async fn schema_document(
    app: &App,
    headers: &HeaderMap,
    what: &str,
    cache: Cache,
    build: fn(&Schema) -> String,
    content_type: &'static str,
) -> Response {
    let caller = match Caller::of(app, headers) {
        Ok(c) => c,
        Err(r) => return r,
    };
    if !caller.service {
        let status = if caller.anon { StatusCode::UNAUTHORIZED } else { StatusCode::FORBIDDEN };
        let message = format!("permission denied for {what}");
        return error(status, "42501", &message, None, Some("Use the service_role key"));
    }
    let result = app
        .db
        .read(move |_, schema| {
            let mut cached = cache.lock().unwrap_or_else(|e| e.into_inner());
            match &*cached {
                Some((version, doc)) if *version == schema.version => Ok(doc.clone()),
                _ => {
                    let doc = Bytes::from(build(schema));
                    *cached = Some((schema.version, doc.clone()));
                    Ok(doc)
                }
            }
        })
        .await;
    match result {
        Ok(doc) => ([(header::CONTENT_TYPE, content_type)], doc).into_response(),
        Err(e) => db_error(e),
    }
}

async fn not_found() -> Response {
    error(StatusCode::NOT_FOUND, "PGRST125", "Invalid path specified in request URL", None, None)
}

async fn read_table(
    State(app): State<Arc<App>>,
    Path(table): Path<String>,
    RawQuery(raw): RawQuery,
    headers: HeaderMap,
) -> Response {
    let caller = match Caller::of(&app, &headers) {
        Ok(c) => c,
        Err(r) => return r,
    };
    let query = match query::parse_query(raw.as_deref().unwrap_or("")) {
        Ok(q) => q,
        Err(e) => return parse_error(e),
    };
    let headers = match parse_headers(&headers) {
        Ok(h) => h,
        Err(e) => return parse_error(e),
    };
    // SQLite has no cheap estimate: planned / estimated counts are exact.
    let count = headers.prefer.count.is_some();

    let (range, limits) = (headers.range, app.limits);
    let caller = Arc::new(caller);
    let job = caller.clone();
    let result = app
        .db
        .read(move |conn, schema| {
            job.check(schema);
            match sql::build_read(schema, &table, &query, range, count, limits, &job.grant) {
                Ok(read) => read.run(conn).map(|page| Ok((read.offset, page))),
                Err(e) => Ok(Err(e)),
            }
        })
        .await;
    match result {
        Ok(Ok((offset, page))) => read_response(offset, page, headers.single_object),
        Ok(Err(e)) => caller.build_error(e),
        Err(e) => db_error(e),
    }
}

/// `200`, or `206` when the page is part of a counted total; `416` past its end.
fn read_response(offset: u64, page: Page, single: bool) -> Response {
    let range = content_range(offset, page.rows, page.total);
    let status = match page.total {
        Some(total) if offset > 0 && offset >= total => {
            let details = format!("An offset of {offset} was requested, but there are only {total} rows.");
            let mut r = error(
                StatusCode::RANGE_NOT_SATISFIABLE,
                "PGRST103",
                "Requested range not satisfiable",
                Some(&details),
                None,
            );
            r.headers_mut().insert(header::CONTENT_RANGE, range);
            return r;
        }
        Some(total) if page.rows < total => StatusCode::PARTIAL_CONTENT,
        _ => StatusCode::OK,
    };
    let (content_type, body) = match single {
        true if page.rows != 1 => return build_error(BuildError::NotSingular(page.rows)),
        true => (OBJECT, unwrap_array(page.body)),
        false => (JSON, page.body),
    };
    (status, [(header::CONTENT_TYPE, HeaderValue::from_static(content_type)), (header::CONTENT_RANGE, range)], body)
        .into_response()
}

/// `first-last/total`, `*` for an unknown total, `*/total` for no rows.
fn content_range(offset: u64, rows: u64, total: Option<u64>) -> HeaderValue {
    let total = total.map_or_else(|| "*".to_string(), |t| t.to_string());
    let value = match rows {
        0 => format!("*/{total}"),
        n => format!("{offset}-{}/{total}", offset + n - 1),
    };
    HeaderValue::try_from(value).expect("digits, '-', '/' and '*' are valid header characters")
}

/// `[{...}]` → `{...}`: the one element of a JSON array built by SQLite.
fn unwrap_array(mut body: String) -> String {
    body.pop();
    body.remove(0);
    body
}

async fn insert(
    State(app): State<Arc<App>>,
    Path(table): Path<String>,
    RawQuery(raw): RawQuery,
    headers: HeaderMap,
    body: String,
) -> Response {
    let action = || write::parse_payload(&body).map(Action::Insert);
    run_write(app, table, raw, &headers, action, StatusCode::CREATED).await
}

async fn update(
    State(app): State<Arc<App>>,
    Path(table): Path<String>,
    RawQuery(raw): RawQuery,
    headers: HeaderMap,
    body: String,
) -> Response {
    let action = || write::parse_payload(&body).map(Action::Update);
    run_write(app, table, raw, &headers, action, StatusCode::OK).await
}

async fn delete(
    State(app): State<Arc<App>>,
    Path(table): Path<String>,
    RawQuery(raw): RawQuery,
    headers: HeaderMap,
) -> Response {
    run_write(app, table, raw, &headers, || Ok(Action::Delete), StatusCode::OK).await
}

/// Runs a write on the writer thread, once the role is known and `action`
/// has scanned the body. `status` is used when rows come back; without a
/// body, inserts still answer `201` and the others `204`.
async fn run_write(
    app: Arc<App>,
    table: String,
    raw: Option<String>,
    headers: &HeaderMap,
    action: impl FnOnce() -> Result<Action, BuildError>,
    status: StatusCode,
) -> Response {
    let caller = match Caller::of(&app, headers) {
        Ok(c) => Arc::new(c),
        Err(r) => return r,
    };
    let action = match action() {
        Ok(a) => a,
        Err(e) => return build_error(e),
    };
    let query = match query::parse_query(raw.as_deref().unwrap_or("")) {
        Ok(q) => q,
        Err(e) => return parse_error(e),
    };
    let headers = match parse_headers(headers) {
        Ok(h) => h,
        Err(e) => return parse_error(e),
    };
    let count = headers.prefer.count.is_some();
    let w = Write { table, query, prefer: headers.prefer, single: headers.single_object, action };

    let limits = app.limits;
    let job = caller.clone();
    let result = app
        .db
        .write(move |conn, schema| {
            job.check(schema);
            write::execute(conn, schema, &w, limits, &job.grant)
        })
        .await;
    match result {
        Ok(Ok(out)) => write_response(out, status, count, headers.single_object),
        Ok(Err(e)) => caller.build_error(e),
        Err(e) => db_error(e),
    }
}

/// Rows written are counted with `Prefer: count=exact`; otherwise the total is `*`.
fn write_response(out: Output, status: StatusCode, count: bool, single: bool) -> Response {
    let total = count.then_some(out.rows);
    match out.body {
        Some(body) => {
            let range = content_range(0, out.rows, total);
            let (content_type, body) = if single { (OBJECT, unwrap_array(body)) } else { (JSON, body) };
            (
                status,
                [(header::CONTENT_TYPE, HeaderValue::from_static(content_type)), (header::CONTENT_RANGE, range)],
                body,
            )
                .into_response()
        }
        None => {
            let status = if status == StatusCode::CREATED { status } else { StatusCode::NO_CONTENT };
            (status, [(header::CONTENT_RANGE, content_range(0, 0, total))]).into_response()
        }
    }
}

fn parse_error(e: ParseError) -> Response {
    error(StatusCode::BAD_REQUEST, "PGRST100", &e.to_string(), None, None)
}

fn build_error(e: BuildError) -> Response {
    let message = e.to_string();
    match e {
        BuildError::TableNotFound(_) => error(StatusCode::NOT_FOUND, "PGRST205", &message, None, None),
        BuildError::ColumnNotFound { .. } => error(StatusCode::BAD_REQUEST, "42703", &message, None, None),
        BuildError::RelationshipNotFound { .. } => error(
            StatusCode::BAD_REQUEST,
            "PGRST200",
            &message,
            None,
            Some("Verify that the tables have a foreign key between them"),
        ),
        BuildError::Ambiguous { to, options, .. } => {
            let hints: Vec<String> = options.iter().map(|h| format!("'{to}!{h}'")).collect();
            let hint = format!("Try changing '{to}' to one of: {}", hints.join(", "));
            error(StatusCode::MULTIPLE_CHOICES, "PGRST201", &message, None, Some(&hint))
        }
        BuildError::UnknownEmbed(_) => error(
            StatusCode::BAD_REQUEST,
            "PGRST108",
            &message,
            None,
            Some("Add the embedded resource to the 'select' parameter"),
        ),
        BuildError::InvalidBody(_) => error(StatusCode::BAD_REQUEST, "PGRST102", &message, None, None),
        BuildError::PayloadColumn { .. } => error(StatusCode::BAD_REQUEST, "PGRST204", &message, None, None),
        BuildError::NoUniqueKey { .. } => error(StatusCode::BAD_REQUEST, "42P10", &message, None, None),
        BuildError::MissingFilter(_) => {
            error(StatusCode::BAD_REQUEST, "21000", &message, None, Some("Add a filter, e.g. id=eq.<value>"))
        }
        BuildError::NotSingular(rows) => {
            let details = format!("The result contains {rows} rows");
            error(StatusCode::NOT_ACCEPTABLE, "PGRST116", &message, Some(&details), None)
        }
        BuildError::Unsupported(_) => error(StatusCode::BAD_REQUEST, "PGRST100", &message, None, None),
        BuildError::PermissionDenied(_) | BuildError::RowPolicy(_) => {
            error(StatusCode::FORBIDDEN, "42501", &message, None, None)
        }
    }
}

fn db_error(e: DbError) -> Response {
    match e {
        DbError::Closed => error(StatusCode::SERVICE_UNAVAILABLE, "PGRST000", &e.to_string(), None, None),
        DbError::Sqlite(e) => {
            let (status, code) = sqlite_status(&e);
            error(status, code, &e.to_string(), None, None)
        }
    }
}

/// HTTP status and PostgreSQL SQLSTATE for a SQLite error, so clients can
/// check `error.code` as they would against PostgREST.
fn sqlite_status(e: &rusqlite::Error) -> (StatusCode, &'static str) {
    let (f, msg) = match e {
        rusqlite::Error::SqliteFailure(f, msg) => (f, msg.as_deref()),
        rusqlite::Error::SqlInputError { error, msg, .. } => (error, Some(msg.as_str())),
        _ => return (StatusCode::INTERNAL_SERVER_ERROR, "XX000"),
    };
    match f.extended_code {
        ffi::SQLITE_CONSTRAINT_UNIQUE | ffi::SQLITE_CONSTRAINT_PRIMARYKEY => (StatusCode::CONFLICT, "23505"),
        ffi::SQLITE_CONSTRAINT_FOREIGNKEY => (StatusCode::CONFLICT, "23503"),
        ffi::SQLITE_CONSTRAINT_NOTNULL => (StatusCode::BAD_REQUEST, "23502"),
        ffi::SQLITE_CONSTRAINT_CHECK => (StatusCode::BAD_REQUEST, "23514"),
        ffi::SQLITE_CONSTRAINT_DATATYPE => (StatusCode::BAD_REQUEST, "22P02"),
        _ => match f.code {
            ErrorCode::ConstraintViolation => (StatusCode::BAD_REQUEST, "23000"),
            ErrorCode::TypeMismatch => (StatusCode::BAD_REQUEST, "22P02"),
            ErrorCode::TooBig => (StatusCode::PAYLOAD_TOO_LARGE, "54000"),
            ErrorCode::ReadOnly => (StatusCode::METHOD_NOT_ALLOWED, "25006"),
            ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked => (StatusCode::SERVICE_UNAVAILABLE, "55P03"),
            // The schema moved between validation and execution.
            _ => match msg {
                Some(m) if m.starts_with("no such table") => (StatusCode::NOT_FOUND, "42P01"),
                Some(m) if m.starts_with("no such column") => (StatusCode::BAD_REQUEST, "42703"),
                _ => (StatusCode::INTERNAL_SERVER_ERROR, "XX000"),
            },
        },
    }
}

/// PostgREST error body: `{code, message, details, hint}`.
fn error(status: StatusCode, code: &str, message: &str, details: Option<&str>, hint: Option<&str>) -> Response {
    let body = serde_json::json!({ "code": code, "message": message, "details": details, "hint": hint });
    (status, [(header::CONTENT_TYPE, JSON)], body.to_string()).into_response()
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
    eprintln!("rulite: shutting down");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sqlite_err(setup: &str, stmt: &str) -> (StatusCode, &'static str) {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        conn.execute_batch(setup).unwrap();
        sqlite_status(&conn.execute_batch(stmt).unwrap_err())
    }

    #[test]
    fn sqlite_errors_map_to_postgres_codes() {
        let setup = "CREATE TABLE p (id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE CHECK (name <> ''));
                     CREATE TABLE c (id INTEGER PRIMARY KEY, p_id INTEGER REFERENCES p(id));
                     CREATE TABLE s (n INTEGER) STRICT;
                     INSERT INTO p VALUES (1, 'a');";
        assert_eq!(sqlite_err(setup, "INSERT INTO p VALUES (2, 'a')"), (StatusCode::CONFLICT, "23505"));
        assert_eq!(sqlite_err(setup, "INSERT INTO p VALUES (1, 'b')"), (StatusCode::CONFLICT, "23505"));
        assert_eq!(sqlite_err(setup, "INSERT INTO c VALUES (1, 9)"), (StatusCode::CONFLICT, "23503"));
        assert_eq!(sqlite_err(setup, "INSERT INTO p (id) VALUES (3)"), (StatusCode::BAD_REQUEST, "23502"));
        assert_eq!(sqlite_err(setup, "INSERT INTO p VALUES (3, '')"), (StatusCode::BAD_REQUEST, "23514"));
        assert_eq!(sqlite_err(setup, "INSERT INTO s VALUES ('x')"), (StatusCode::BAD_REQUEST, "22P02"));
        assert_eq!(sqlite_err(setup, "SELECT * FROM nope"), (StatusCode::NOT_FOUND, "42P01"));
        assert_eq!(sqlite_err(setup, "SELECT nope FROM p"), (StatusCode::BAD_REQUEST, "42703"));
    }

    #[test]
    fn auth_errors() {
        let auth = Arc::new(Auth::new(&crate::config::Auth::default(), "0123456789abcdef0123456789abcdef", "http://x"));
        let caller = |anon| Caller { grant: Grant::default(), anon, service: false, auth: auth.clone() };
        let denied = || BuildError::PermissionDenied("t".into());
        assert_eq!(caller(true).build_error(denied()).status(), StatusCode::UNAUTHORIZED);
        assert_eq!(caller(false).build_error(denied()).status(), StatusCode::FORBIDDEN);
        assert_eq!(caller(true).build_error(BuildError::RowPolicy("t".into())).status(), StatusCode::UNAUTHORIZED);
        assert_eq!(caller(true).build_error(BuildError::MissingFilter("DELETE")).status(), StatusCode::BAD_REQUEST);
        for e in [RoleError::Missing, RoleError::Invalid, RoleError::Expired] {
            let r = jwt_error(e);
            assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
            assert_eq!(r.headers()[header::WWW_AUTHENTICATE], "Bearer");
        }
    }

    #[test]
    fn content_ranges() {
        assert_eq!(content_range(0, 10, Some(42)), "0-9/42");
        assert_eq!(content_range(5, 1, None), "5-5/*");
        assert_eq!(content_range(0, 0, Some(0)), "*/0");
        assert_eq!(content_range(0, 0, None), "*/*");
    }

    fn page(body: &str, rows: u64, total: Option<u64>) -> Page {
        Page { body: body.into(), rows, total }
    }

    fn status_and_range(r: &Response) -> (StatusCode, &str) {
        (r.status(), r.headers()[header::CONTENT_RANGE].to_str().unwrap())
    }

    #[test]
    fn read_statuses() {
        assert_eq!(status_and_range(&read_response(0, page("[1,2]", 2, None), false)), (StatusCode::OK, "0-1/*"));
        assert_eq!(status_and_range(&read_response(0, page("[1,2]", 2, Some(2)), false)), (StatusCode::OK, "0-1/2"));
        assert_eq!(
            status_and_range(&read_response(0, page("[1,2]", 2, Some(42)), false)),
            (StatusCode::PARTIAL_CONTENT, "0-1/42")
        );
        assert_eq!(
            status_and_range(&read_response(40, page("[1,2]", 2, Some(42)), false)),
            (StatusCode::PARTIAL_CONTENT, "40-41/42")
        );
        assert_eq!(status_and_range(&read_response(0, page("[]", 0, Some(0)), false)), (StatusCode::OK, "*/0"));
        assert_eq!(
            status_and_range(&read_response(50, page("[]", 0, Some(42)), false)),
            (StatusCode::RANGE_NOT_SATISFIABLE, "*/42")
        );
    }

    #[test]
    fn single_object() {
        let r = read_response(0, page(r#"[{"a":1}]"#, 1, None), true);
        assert_eq!(
            (r.status(), &r.headers()[header::CONTENT_TYPE]),
            (StatusCode::OK, &HeaderValue::from_static(OBJECT))
        );
        assert_eq!(read_response(0, page("[]", 0, None), true).status(), StatusCode::NOT_ACCEPTABLE);
        assert_eq!(read_response(0, page("[1,2]", 2, None), true).status(), StatusCode::NOT_ACCEPTABLE);
        assert_eq!(unwrap_array(r#"[{"a":[1]}]"#.into()), r#"{"a":[1]}"#);

        let out = Output { body: Some(r#"[{"a":1}]"#.into()), rows: 1 };
        let r = write_response(out, StatusCode::CREATED, true, true);
        assert_eq!(status_and_range(&r), (StatusCode::CREATED, "0-0/1"));
        let r = write_response(Output { body: None, rows: 3 }, StatusCode::OK, true, false);
        assert_eq!(status_and_range(&r), (StatusCode::NO_CONTENT, "*/3"));
    }
}
