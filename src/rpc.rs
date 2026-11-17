//! Functions for `/rest/v1/rpc/{name}`: one `.sql` file each, whose header
//! comments declare the arguments (`-- @param q text = "x"`) and the result
//! (`-- @returns setof`), and whose `:name` parameters are bound from the
//! request's arguments. The SQL comes from the server's files, never from a
//! request; argument values are always bound.

use std::path::Path;
use std::sync::{Arc, Mutex};

use rusqlite::fallible_iterator::FallibleIterator;
use rusqlite::types::{Value, ValueRef};
use rusqlite::{Batch, Connection, Rows, TransactionBehavior};
use serde_json::Value as Json;

use crate::query::{Query, Range, SelectItem};
use crate::schema::{Affinity, Column, Schema, Table};
use crate::sql::{self, BuildError, Limits, Page};

/// Parameters every function may use without declaring them: the user id
/// (NULL for anon and service_role) and the role name.
pub const RESERVED: [&str; 2] = ["auth_uid", "auth_role"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArgType {
    Text,
    Integer,
    Real,
    /// Bound as 0 / 1.
    Boolean,
    /// Bound as JSON text.
    Json,
}

impl ArgType {
    fn parse(s: &str) -> Option<ArgType> {
        Some(match s {
            "text" => ArgType::Text,
            "integer" => ArgType::Integer,
            "real" => ArgType::Real,
            "boolean" => ArgType::Boolean,
            "json" => ArgType::Json,
            _ => return None,
        })
    }

    /// The PostgreSQL type name, for errors.
    pub fn name(self) -> &'static str {
        match self {
            ArgType::Text => "text",
            ArgType::Integer => "integer",
            ArgType::Real => "real",
            ArgType::Boolean => "boolean",
            ArgType::Json => "json",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Arg {
    pub name: String,
    pub ty: ArgType,
    /// Optional when set (`= null` is a NULL default).
    pub default: Option<Value>,
}

/// The response shape, from `@returns`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Returns {
    /// An array of row objects.
    Setof,
    /// The first row as an object, or `null`.
    Row,
    /// The first column of the first row as a JSON value, or `null`.
    Value,
    /// The first column of the first row, JSON text the function built.
    Json,
    /// No body: `204`.
    Void,
}

/// How a function runs, decided by preparing it at startup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// One read-only statement: on a reader, filterable like a table.
    Query,
    /// Writes (one or more statements): one transaction on the writer.
    Procedure,
}

#[derive(Debug)]
pub struct Function {
    pub name: String,
    /// The header's comment lines that aren't annotations.
    pub description: String,
    /// In declaration order.
    pub args: Vec<Arg>,
    pub returns: Returns,
    /// A query's SQL has its trailing `;` removed.
    pub sql: String,
    pub kind: Kind,
    /// Result columns at startup (the last statement's): name, declared type.
    pub columns: Vec<(String, Option<String>)>,
    /// A query's parameter names, in `?N` order.
    params: Vec<String>,
    /// A query's result as a table, for the schema version it was read at.
    result: Mutex<Option<(i64, Arc<Table>)>>,
}

/// The loaded functions, sorted by name.
#[derive(Debug, Default)]
pub struct Functions(Vec<Function>);

impl Functions {
    pub fn get(&self, name: &str) -> Option<&Function> {
        self.0.binary_search_by(|f| f.name.as_str().cmp(name)).ok().map(|i| &self.0[i])
    }

    pub fn iter(&self) -> impl Iterator<Item = &Function> {
        self.0.iter()
    }
}

impl Function {
    pub fn arg(&self, name: &str) -> Option<&Arg> {
        self.args.iter().find(|a| a.name == name)
    }

    /// `name(arg, ...)` with the arguments sorted, as PostgREST's hints show it.
    pub fn signature(&self) -> String {
        let mut names: Vec<&str> = self.args.iter().map(|a| a.name.as_str()).collect();
        names.sort_unstable();
        format!("{}({})", self.name, names.join(", "))
    }

    /// The result columns at startup, typed by their declared types.
    pub fn result_columns(&self) -> Vec<Column> {
        result_table(&self.name, &self.columns).columns
    }

    /// A query's result columns as a table, re-read when the schema moved.
    fn result(&self, conn: &Connection, version: i64) -> rusqlite::Result<Arc<Table>> {
        let mut cached = self.result.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((v, table)) = &*cached
            && *v == version
        {
            return Ok(table.clone());
        }
        let columns = columns(&conn.prepare(&self.sql)?);
        let table = Arc::new(result_table(&self.name, &columns));
        *cached = Some((version, table.clone()));
        Ok(table)
    }
}

/// Reads `dir/*.sql` (one function per file, named after it).
pub fn load(dir: &Path) -> Result<Vec<Function>, String> {
    let cannot = |e: std::io::Error| format!("cannot read functions in {}: {e}", dir.display());
    let mut paths = Vec::new();
    for entry in std::fs::read_dir(dir).map_err(cannot)? {
        let path = entry.map_err(cannot)?.path();
        if path.extension().is_some_and(|e| e == "sql") && path.is_file() {
            paths.push(path);
        }
    }
    paths.sort();
    paths
        .iter()
        .map(|path| {
            let text = std::fs::read_to_string(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
            let name = path.file_stem().and_then(|s| s.to_str()).unwrap_or_default();
            parse(name, &text).map_err(|e| format!("{}: {e}", path.display()))
        })
        .collect()
}

/// A function from its name and file text; the header is the leading block
/// of `--` comments.
pub fn parse(name: &str, text: &str) -> Result<Function, String> {
    if !identifier(name) {
        return Err(format!("'{name}' is not a valid function name (letters, digits and _)"));
    }
    let (mut description, mut args, mut returns) = (Vec::new(), Vec::<Arg>::new(), None);
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let Some(comment) = line.strip_prefix("--").map(str::trim) else { break };
        let Some(annotation) = comment.strip_prefix('@') else {
            if !comment.is_empty() {
                description.push(comment);
            }
            continue;
        };
        let (tag, rest) = annotation.split_once(char::is_whitespace).unwrap_or((annotation, ""));
        match tag {
            "param" => {
                let arg = param(rest.trim())?;
                if args.iter().any(|a| a.name == arg.name) {
                    return Err(format!("@param {} is declared twice", arg.name));
                }
                args.push(arg);
            }
            "returns" if returns.is_none() => {
                returns = Some(match rest.trim() {
                    "setof" => Returns::Setof,
                    "row" => Returns::Row,
                    "value" => Returns::Value,
                    "json" => Returns::Json,
                    "void" => Returns::Void,
                    other => return Err(format!("@returns {other}: expected setof, row, value, json or void")),
                });
            }
            "returns" => return Err("@returns is declared twice".into()),
            other => return Err(format!("unknown annotation @{other}")),
        }
    }
    Ok(Function {
        name: name.to_string(),
        description: description.join(" "),
        args,
        returns: returns.unwrap_or(Returns::Setof),
        sql: text.to_string(),
        kind: Kind::Procedure,
        columns: Vec::new(),
        params: Vec::new(),
        result: Mutex::new(None),
    })
}

/// `name type [= default]`, the default a JSON literal.
fn param(decl: &str) -> Result<Arg, String> {
    let (head, default) = match decl.split_once('=') {
        Some((head, default)) => (head, Some(default.trim())),
        None => (decl, None),
    };
    let usage = || format!("@param {decl}: expected '@param <name> <type> [= <default>]'");
    let mut words = head.split_whitespace();
    let (Some(name), Some(ty), None) = (words.next(), words.next(), words.next()) else { return Err(usage()) };
    if !identifier(name) {
        return Err(usage());
    }
    if RESERVED.contains(&name) {
        return Err(format!("@param {name}: :{name} is reserved and always bound"));
    }
    let ty = ArgType::parse(ty)
        .ok_or_else(|| format!("@param {name}: unknown type {ty} (text, integer, real, boolean or json)"))?;
    let default = match default {
        None => None,
        Some(text) => {
            let json: Json =
                serde_json::from_str(text).map_err(|_| format!("@param {name}: the default {text} is not JSON"))?;
            Some(from_json(ty, &json).map_err(|e| format!("@param {name}: {e}"))?)
        }
    };
    Ok(Arg { name: name.to_string(), ty, default })
}

fn identifier(s: &str) -> bool {
    let mut chars = s.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Prepares every function against the database: checks its parameters and
/// result, and decides whether it is a query or a procedure.
pub fn check(conn: &Connection, functions: Vec<Function>) -> Result<Functions, String> {
    let mut checked = Vec::with_capacity(functions.len());
    for mut f in functions {
        classify(conn, &mut f).map_err(|e| format!("function {}: {e}", f.name))?;
        checked.push(f);
    }
    checked.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(Functions(checked))
}

fn classify(conn: &Connection, f: &mut Function) -> Result<(), String> {
    let mut batch = Batch::new(conn, &f.sql);
    let (mut statements, mut writes, mut used) = (0, false, Vec::new());
    while let Some(stmt) = batch.next().map_err(|e| e.to_string())? {
        statements += 1;
        writes |= !stmt.readonly();
        let names = parameters(&stmt)?;
        if statements == 1 {
            f.params = names.clone();
        }
        used.extend(names);
        f.columns = columns(&stmt);
    }
    if statements == 0 {
        return Err("no SQL statement".into());
    }
    if let Some(name) = used.iter().find(|n| f.arg(n).is_none() && !RESERVED.contains(&n.as_str())) {
        return Err(format!(":{name} is not declared; add '-- @param {name} <type>'"));
    }
    if let Some(a) = f.args.iter().find(|a| !used.contains(&a.name)) {
        return Err(format!("@param {} is not used in the SQL", a.name));
    }
    if !writes && statements > 1 {
        return Err("a read-only function must be a single statement".into());
    }
    if f.returns != Returns::Void && f.columns.is_empty() {
        return Err("the last statement returns no columns; add RETURNING or use '-- @returns void'".into());
    }
    let mut names: Vec<&str> = f.columns.iter().map(|(n, _)| n.as_str()).collect();
    names.sort_unstable();
    if matches!(f.returns, Returns::Setof | Returns::Row)
        && let Some(w) = names.windows(2).find(|w| w[0] == w[1])
    {
        return Err(format!("the result has two columns named {}; give them aliases", w[0]));
    }
    if writes {
        f.kind = Kind::Procedure;
        return Ok(());
    }
    f.kind = Kind::Query;
    let sql = f.sql.trim_end();
    f.sql = sql.strip_suffix(';').unwrap_or(sql).trim_end().to_string();
    let wrapped = format!("WITH {} AS (\n{}\n) SELECT * FROM {0}", sql::CALL, f.sql);
    conn.prepare(&wrapped).map_err(|e| format!("{e} (a read-only function must be one SELECT statement)"))?;
    Ok(())
}

/// The statement's named parameters (`:name`, `@name`, `$name`) in index
/// order, without their prefix; positional ones are refused.
fn parameters(stmt: &rusqlite::Statement) -> Result<Vec<String>, String> {
    (1..=stmt.parameter_count())
        .map(|i| match stmt.parameter_name(i) {
            Some(n) if !n.starts_with('?') => Ok(n[1..].to_string()),
            _ => Err("positional parameters (?) are not supported; use :name".to_string()),
        })
        .collect()
}

fn columns(stmt: &rusqlite::Statement) -> Vec<(String, Option<String>)> {
    stmt.columns().iter().map(|c| (c.name().to_string(), c.decl_type().map(str::to_string))).collect()
}

/// A result as a table, so filters, order and `select` resolve against it.
/// Not STRICT, so a BLOB in any column still reads as `\x` hex.
fn result_table(name: &str, columns: &[(String, Option<String>)]) -> Table {
    let columns = columns
        .iter()
        .map(|(name, decl)| {
            let decl_type = decl.clone().unwrap_or_default();
            let upper = decl_type.to_ascii_uppercase();
            Column {
                name: name.clone(),
                affinity: Affinity::from_decl_type(&decl_type),
                json: upper == "JSON" || upper == "JSONB",
                decl_type,
                not_null: false,
                default: None,
                pk: 0,
            }
        })
        .collect();
    Table {
        name: name.to_string(),
        view: false,
        strict: false,
        without_rowid: false,
        columns,
        primary_key: Vec::new(),
        uniques: Vec::new(),
        foreign_keys: Vec::new(),
        relationships: Vec::new(),
    }
}

/// Where a request's arguments come from.
pub enum Source<'a> {
    /// `POST`: a JSON object (empty means none).
    Body(&'a str),
    /// `GET` / `HEAD`: query-string pairs whose key is an argument.
    Query(Vec<(String, String)>),
}

enum Given {
    Json(Json),
    Text(String),
}

/// The value of every parameter of `f` by name: the request's arguments in
/// their declared types, defaults, then `auth_uid` and `auth_role`.
pub fn args(f: &Function, source: Source, role: &str, uid: Option<&str>) -> Result<Vec<(String, Value)>, BuildError> {
    let given: Vec<(String, Given)> = match source {
        Source::Body(body) => body_args(body)?.into_iter().map(|(k, v)| (k, Given::Json(v))).collect(),
        Source::Query(pairs) => pairs.into_iter().map(|(k, v)| (k, Given::Text(v))).collect(),
    };
    let mut names: Vec<String> = given.iter().map(|(k, _)| k.clone()).collect();
    names.sort_unstable();
    names.dedup();
    let not_found =
        || BuildError::FunctionNotFound { name: f.name.clone(), args: names.clone(), signature: Some(f.signature()) };
    if names.iter().any(|n| f.arg(n).is_none()) {
        return Err(not_found());
    }
    let mut values = Vec::with_capacity(f.args.len() + RESERVED.len());
    for a in &f.args {
        let value = match given.iter().rev().find(|(k, _)| *k == a.name) {
            Some((_, Given::Json(v))) => from_json(a.ty, v)?,
            Some((_, Given::Text(s))) => from_text(a.ty, s)?,
            None => a.default.clone().ok_or_else(not_found)?,
        };
        values.push((a.name.clone(), value));
    }
    values.push(("auth_uid".into(), uid.map_or(Value::Null, |u| Value::Text(u.to_string()))));
    values.push(("auth_role".into(), Value::Text(role.to_string())));
    Ok(values)
}

/// The argument names of a `POST` body, for a not-found error.
pub fn body_names(body: &str) -> Vec<String> {
    let mut names: Vec<String> = body_args(body).unwrap_or_default().into_iter().map(|(k, _)| k).collect();
    names.sort_unstable();
    names
}

fn body_args(body: &str) -> Result<serde_json::Map<String, Json>, BuildError> {
    if body.trim().is_empty() {
        return Ok(serde_json::Map::new());
    }
    match serde_json::from_str(body) {
        Ok(Json::Object(map)) => Ok(map),
        Ok(_) => Err(BuildError::InvalidBody("expected a JSON object of function arguments".into())),
        Err(e) => Err(BuildError::InvalidBody(e.to_string())),
    }
}

fn from_json(ty: ArgType, v: &Json) -> Result<Value, BuildError> {
    let invalid =
        || BuildError::InvalidArg { ty: ty.name(), value: v.as_str().map_or_else(|| v.to_string(), Into::into) };
    match (ty, v) {
        (_, Json::Null) => Ok(Value::Null),
        (ArgType::Json, v) => Ok(Value::Text(v.to_string())),
        (_, Json::String(s)) => from_text(ty, s),
        (ArgType::Text, Json::Number(_) | Json::Bool(_)) => Ok(Value::Text(v.to_string())),
        (ArgType::Integer, Json::Number(n)) => n.as_i64().map(Value::Integer).ok_or_else(invalid),
        (ArgType::Real, Json::Number(n)) => n.as_f64().map(Value::Real).ok_or_else(invalid),
        (ArgType::Boolean, Json::Bool(b)) => Ok(Value::Integer(i64::from(*b))),
        _ => Err(invalid()),
    }
}

fn from_text(ty: ArgType, s: &str) -> Result<Value, BuildError> {
    let invalid = || BuildError::InvalidArg { ty: ty.name(), value: s.to_string() };
    match ty {
        ArgType::Text => Ok(Value::Text(s.to_string())),
        ArgType::Integer => s.trim().parse().map(Value::Integer).map_err(|_| invalid()),
        ArgType::Real => s.trim().parse::<f64>().ok().filter(|f| f.is_finite()).map(Value::Real).ok_or_else(invalid),
        ArgType::Boolean => match s.trim().to_ascii_lowercase().as_str() {
            "true" | "t" | "yes" | "y" | "on" | "1" => Ok(Value::Integer(1)),
            "false" | "f" | "no" | "n" | "off" | "0" => Ok(Value::Integer(0)),
            _ => Err(invalid()),
        },
        ArgType::Json => match serde_json::from_str::<serde::de::IgnoredAny>(s) {
            Ok(_) => Ok(Value::Text(s.to_string())),
            Err(_) => Err(invalid()),
        },
    }
}

fn lookup(args: &[(String, Value)], name: &str) -> Value {
    args.iter().find(|(n, _)| n == name).map_or(Value::Null, |(_, v)| v.clone())
}

/// Whether `query` only selects `*`: no filter, order or paging.
pub fn plain(query: &Query) -> bool {
    let root = query.root();
    matches!(query.select.as_slice(), [SelectItem::Star])
        && query.scopes.len() == 1
        && root.filters.is_empty()
        && root.order.is_empty()
        && root.limit.is_none()
        && root.offset.is_none()
}

/// What a call answers.
#[derive(Debug, PartialEq)]
pub enum Outcome {
    /// `setof` from a query: a page, as a table read answers.
    Page { offset: u64, page: Page },
    /// The body (`None` for `void`) and the rows it came from.
    Body { body: Option<String>, rows: u64 },
}

/// The arguments a query needs, in `?N` order.
fn params(f: &Function, args: &[(String, Value)]) -> Vec<Value> {
    f.params.iter().map(|p| lookup(args, p)).collect()
}

/// Runs a query (on a reader). Build errors come back as `Ok(Err(_))`.
#[allow(clippy::too_many_arguments)]
pub fn run_query(
    conn: &Connection,
    schema: &Schema,
    f: &Function,
    args: &[(String, Value)],
    mut query: Query,
    range: Option<Range>,
    count: bool,
    limits: Limits,
) -> rusqlite::Result<Result<Outcome, BuildError>> {
    let returns = f.returns;
    if matches!(returns, Returns::Value | Returns::Json | Returns::Void) && !plain(&query) {
        return Ok(Err(unfilterable(f)));
    }
    if returns == Returns::Void {
        return statements(conn, f, args).map(|(body, rows)| Ok(Outcome::Body { body, rows }));
    }
    let result = f.result(conn, schema.version)?;
    if matches!(returns, Returns::Value | Returns::Json) {
        let body = sql::build_scalar(&f.sql, &result, params(f, args), returns == Returns::Json).run(conn)?;
        return Ok(Ok(Outcome::Body { body: Some(body), rows: 1 }));
    }
    if returns == Returns::Row {
        query.scopes[0].limit = Some(1);
    }
    let read = match sql::build_call(schema, &f.sql, &result, params(f, args), &query, range, count, limits) {
        Ok(read) => read,
        Err(e) => return Ok(Err(e)),
    };
    let page = read.run(conn)?;
    Ok(Ok(match returns {
        Returns::Setof => Outcome::Page { offset: read.offset, page },
        _ if page.rows == 0 => Outcome::Body { body: Some("null".into()), rows: 0 },
        _ => Outcome::Body { body: Some(page.body[1..page.body.len() - 1].to_string()), rows: 1 },
    }))
}

fn unfilterable(f: &Function) -> BuildError {
    BuildError::Unsupported(format!(
        "function {} returns {}: filters, order, limit and select apply to read-only functions returning setof or row",
        f.name,
        match (f.kind, f.returns) {
            (Kind::Procedure, _) => "after writing",
            (_, Returns::Void) => "void",
            _ => "a single value",
        }
    ))
}

/// Runs a procedure in one `IMMEDIATE` transaction on the writer; any error
/// rolls it back. Filters and modifiers are refused.
pub fn run_procedure(
    conn: &mut Connection,
    f: &Function,
    args: &[(String, Value)],
    query: &Query,
) -> rusqlite::Result<Result<Outcome, BuildError>> {
    if !plain(query) {
        return Ok(Err(unfilterable(f)));
    }
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let (body, rows) = statements(&tx, f, args)?;
    tx.commit()?;
    Ok(Ok(Outcome::Body { body, rows }))
}

/// Runs every statement of `f`; the last one's rows make the body. This is
/// the one place JSON is built in Rust: a `RETURNING` can't be a subquery.
fn statements(conn: &Connection, f: &Function, args: &[(String, Value)]) -> rusqlite::Result<(Option<String>, u64)> {
    let mut batch = Batch::new(conn, &f.sql);
    let mut out = (None, 0);
    while let Some(mut stmt) = batch.next()? {
        for i in 1..=stmt.parameter_count() {
            let name = stmt.parameter_name(i).map(|n| n[1..].to_string()).unwrap_or_default();
            stmt.raw_bind_parameter(i, lookup(args, &name))?;
        }
        let names: Vec<String> = stmt.column_names().into_iter().map(String::from).collect();
        out = collect(stmt.raw_query(), &names, f.returns)?;
    }
    Ok(out)
}

fn collect(mut rows: Rows, names: &[String], returns: Returns) -> rusqlite::Result<(Option<String>, u64)> {
    let (mut n, mut body) = (0, String::new());
    while let Some(row) = rows.next()? {
        n += 1;
        match returns {
            Returns::Setof => {
                body.push(if n == 1 { '[' } else { ',' });
                object(&mut body, row, names)?;
            }
            Returns::Row if n == 1 => object(&mut body, row, names)?,
            Returns::Value if n == 1 => body = json_value(row.get_ref(0)?).to_string(),
            Returns::Json if n == 1 => body = json_text(row.get_ref(0)?)?,
            _ => {}
        }
    }
    Ok(match returns {
        Returns::Void => (None, n),
        Returns::Setof if n == 0 => (Some("[]".into()), 0),
        Returns::Setof => {
            body.push(']');
            (Some(body), n)
        }
        _ if n == 0 => (Some("null".into()), 0),
        _ => (Some(body), 1),
    })
}

fn object(out: &mut String, row: &rusqlite::Row, names: &[String]) -> rusqlite::Result<()> {
    out.push('{');
    for (i, name) in names.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&Json::from(name.as_str()).to_string());
        out.push(':');
        out.push_str(&json_value(row.get_ref(i)?).to_string());
    }
    out.push('}');
    Ok(())
}

/// A value as a read returns it (see `sql::column_value`).
fn json_value(v: ValueRef) -> Json {
    match v {
        ValueRef::Null => Json::Null,
        ValueRef::Integer(i) => i.into(),
        ValueRef::Real(f) => serde_json::Number::from_f64(f).map_or(Json::Null, Json::Number),
        ValueRef::Text(t) => String::from_utf8_lossy(t).into_owned().into(),
        ValueRef::Blob(b) => {
            let mut hex = String::with_capacity(2 + b.len() * 2);
            hex.push_str("\\x");
            for byte in b {
                hex.push_str(&format!("{byte:02x}"));
            }
            hex.into()
        }
    }
}

/// A value the function built as JSON text.
fn json_text(v: ValueRef) -> rusqlite::Result<String> {
    let invalid = |e: Box<dyn std::error::Error + Send + Sync>| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, e)
    };
    match v {
        ValueRef::Text(t) => {
            let text = std::str::from_utf8(t).map_err(|e| invalid(Box::new(e)))?;
            serde_json::from_str::<serde::de::IgnoredAny>(text).map_err(|e| invalid(Box::new(e)))?;
            Ok(text.to_string())
        }
        ValueRef::Blob(_) => Err(invalid("a BLOB is not JSON".into())),
        v => Ok(json_value(v).to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::parse_query;
    use crate::schema::introspect;
    use serde_json::json;

    const LIMITS: Limits = Limits { default: 100, max: 1000 };

    fn novel() -> (Connection, Schema) {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(include_str!("../sample/novel.sql")).unwrap();
        let schema = introspect(&conn).unwrap();
        (conn, schema)
    }

    fn function(conn: &Connection, name: &str, text: &str) -> Result<Functions, String> {
        check(conn, vec![parse(name, text)?])
    }

    fn one(conn: &Connection, text: &str) -> Functions {
        function(conn, "f", text).unwrap_or_else(|e| panic!("{e}"))
    }

    fn body_args(f: &Function, body: Json) -> Vec<(String, Value)> {
        args(f, Source::Body(&body.to_string()), "anon", None).unwrap()
    }

    /// Calls `f` (a query) with a JSON body and a query string; the body as JSON.
    fn call(db: &(Connection, Schema), fns: &Functions, body: Json, raw: &str) -> Result<Json, BuildError> {
        let f = fns.get("f").unwrap();
        let a = args(f, Source::Body(&body.to_string()), "anon", None)?;
        let out = run_query(&db.0, &db.1, f, &a, parse_query(raw).unwrap(), None, true, LIMITS).unwrap()?;
        Ok(match out {
            Outcome::Page { page, .. } => serde_json::from_str(&page.body).unwrap(),
            Outcome::Body { body: Some(b), .. } => serde_json::from_str(&b).unwrap(),
            Outcome::Body { body: None, .. } => Json::Null,
        })
    }

    #[test]
    fn header() {
        let f = parse(
            "search",
            "-- Novels by title.\n--\n-- Matches anywhere.\n-- @param q text\n-- @param max integer = 10\n\
             -- @param tags json = [\"a\"]\n-- @param on boolean = true\n-- @param r real = null\n-- @returns row\n\
             SELECT 1;\n-- @param late text\n",
        )
        .unwrap();
        assert_eq!(f.description, "Novels by title. Matches anywhere.");
        assert_eq!(f.returns, Returns::Row);
        let a: Vec<(&str, ArgType, Option<Value>)> =
            f.args.iter().map(|a| (a.name.as_str(), a.ty, a.default.clone())).collect();
        assert_eq!(
            a,
            [
                ("q", ArgType::Text, None),
                ("max", ArgType::Integer, Some(Value::Integer(10))),
                ("tags", ArgType::Json, Some(Value::Text("[\"a\"]".into()))),
                ("on", ArgType::Boolean, Some(Value::Integer(1))),
                ("r", ArgType::Real, Some(Value::Null)),
            ]
        );
        assert_eq!(f.signature(), "search(max, on, q, r, tags)");
        assert_eq!(parse("f", "SELECT 1").unwrap().returns, Returns::Setof);

        for bad in [
            "-- @param q\nSELECT 1",
            "-- @param q text extra\nSELECT 1",
            "-- @param q varchar\nSELECT 1",
            "-- @param 1q text\nSELECT 1",
            "-- @param q integer = x\nSELECT 1",
            "-- @param q integer = \"x\"\nSELECT 1",
            "-- @param q text\n-- @param q text\nSELECT 1",
            "-- @param auth_uid text\nSELECT 1",
            "-- @returns table\nSELECT 1",
            "-- @returns row\n-- @returns setof\nSELECT 1",
            "-- @grant anon\nSELECT 1",
        ] {
            assert!(parse("f", bad).is_err(), "{bad}");
        }
        assert!(parse("my-fn", "SELECT 1").is_err());
    }

    #[test]
    fn startup_checks() {
        let (conn, _) = novel();
        let kind = |text: &str| one(&conn, text).get("f").unwrap().kind;
        assert_eq!(kind("SELECT title FROM novel"), Kind::Query);
        assert_eq!(
            kind("-- @param t text\nINSERT INTO novel (title, author) VALUES (:t, 'a') RETURNING id"),
            Kind::Procedure
        );
        assert_eq!(
            kind("-- @returns void\nDELETE FROM part WHERE 0; UPDATE novel SET genre = genre WHERE 0;"),
            Kind::Procedure
        );
        let f = one(&conn, "-- @param q text\nSELECT id, title AS name FROM novel WHERE title = :q;  \n");
        let f = f.get("f").unwrap();
        assert_eq!(f.sql, "-- @param q text\nSELECT id, title AS name FROM novel WHERE title = :q");
        assert_eq!(f.columns, [("id".into(), Some("TEXT".into())), ("name".into(), Some("TEXT".into()))]);

        let err = |text: &str| function(&conn, "f", text).unwrap_err();
        assert_eq!(
            err("SELECT * FROM novel WHERE id = ?"),
            "function f: positional parameters (?) are not supported; use :name"
        );
        assert!(err("SELECT * FROM novel WHERE id = :id").contains(":id is not declared"));
        assert!(err("-- @param id text\nSELECT 1").contains("@param id is not used"));
        assert!(err("SELECT 1; SELECT 2").contains("single statement"));
        assert!(err("SELECT n.id, e.id FROM novel n JOIN edition e").contains("two columns named id"));
        assert!(err("DELETE FROM part WHERE 0").contains("returns no columns"));
        assert!(err("SELECT * FROM nope").contains("no such table"));
        assert!(err("-- nothing").contains("no SQL statement"));
        assert!(err("SELECT 1; -- trailing").contains("one SELECT statement"));
        // Reserved parameters need no declaration; a void query needs no columns.
        one(&conn, "SELECT :auth_uid AS uid, :auth_role AS role");
        one(&conn, "-- @returns void\nSELECT 1 WHERE 0");

        let dir = std::env::temp_dir().join(format!("rulite-rpc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("b.sql"), "SELECT 1 AS one").unwrap();
        std::fs::write(dir.join("a.sql"), "-- @returns value\nSELECT 2").unwrap();
        std::fs::write(dir.join("notes.txt"), "not a function").unwrap();
        let loaded = load(&dir).unwrap();
        assert_eq!(loaded.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(), ["a", "b"]);
        std::fs::write(dir.join("c.sql"), "-- @returns nope\nSELECT 3").unwrap();
        assert!(load(&dir).unwrap_err().contains("c.sql: @returns nope"));
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(load(Path::new("/nonexistent/rulite")).is_err());
    }

    #[test]
    fn arguments() {
        let (conn, _) = novel();
        let fns = one(
            &conn,
            "-- @param q text\n-- @param n integer = 5\n-- @param r real = null\n-- @param b boolean = false\n\
             -- @param j json = {}\nSELECT :q, :n, :r, :b, :j, :auth_uid, :auth_role",
        );
        let f = fns.get("f").unwrap();
        let values = |a: Vec<(String, Value)>| a.into_iter().map(|(_, v)| v).collect::<Vec<_>>();
        assert_eq!(
            values(
                args(
                    f,
                    Source::Body(r#"{"q": "x", "n": 2, "r": 1.5, "b": true, "j": [1]}"#),
                    "authenticated",
                    Some("u1")
                )
                .unwrap()
            ),
            [
                Value::Text("x".into()),
                Value::Integer(2),
                Value::Real(1.5),
                Value::Integer(1),
                Value::Text("[1]".into()),
                Value::Text("u1".into()),
                Value::Text("authenticated".into()),
            ]
        );
        assert_eq!(
            values(body_args(f, json!({ "q": 7 }))),
            [
                Value::Text("7".into()),
                Value::Integer(5),
                Value::Null,
                Value::Integer(0),
                Value::Text("{}".into()),
                Value::Null,
                Value::Text("anon".into())
            ]
        );
        let query = |pairs: &[(&str, &str)]| {
            let pairs = pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
            args(f, Source::Query(pairs), "anon", None).map(values)
        };
        assert_eq!(
            query(&[("q", "a"), ("n", " 3"), ("b", "t"), ("j", "[1,2]")]).unwrap()[..5],
            [Value::Text("a".into()), Value::Integer(3), Value::Null, Value::Integer(1), Value::Text("[1,2]".into())]
        );

        let not_found = |args: &[&str]| BuildError::FunctionNotFound {
            name: "f".into(),
            args: args.iter().map(|s| s.to_string()).collect(),
            signature: Some("f(b, j, n, q, r)".into()),
        };
        assert_eq!(args(f, Source::Body(""), "anon", None).unwrap_err(), not_found(&[]));
        assert_eq!(args(f, Source::Body(r#"{"q":"x","z":1}"#), "anon", None).unwrap_err(), not_found(&["q", "z"]));
        let invalid = |ty, value: &str| Err(BuildError::InvalidArg { ty, value: value.into() });
        assert_eq!(query(&[("q", "a"), ("n", "x")]), invalid("integer", "x"));
        assert_eq!(query(&[("q", "a"), ("r", "inf")]), invalid("real", "inf"));
        assert_eq!(query(&[("q", "a"), ("b", "maybe")]), invalid("boolean", "maybe"));
        assert_eq!(query(&[("q", "a"), ("j", "{x")]), invalid("json", "{x"));
        let body = |v: Json| args(f, Source::Body(&v.to_string()), "anon", None).map(values);
        assert_eq!(body(json!({ "q": "a", "n": 1.5 })), invalid("integer", "1.5"));
        assert_eq!(body(json!({ "q": "a", "n": "x" })), invalid("integer", "x"));
        assert_eq!(body(json!({ "q": [1] })), invalid("text", "[1]"));
        assert!(matches!(args(f, Source::Body("[1]"), "anon", None), Err(BuildError::InvalidBody(_))));
        assert!(matches!(args(f, Source::Body("{"), "anon", None), Err(BuildError::InvalidBody(_))));
        assert_eq!(body_names(r#"{"b":1,"a":2}"#), ["a", "b"]);
        assert_eq!(
            BuildError::InvalidArg { ty: "integer", value: "x".into() }.to_string(),
            "invalid input syntax for type integer: \"x\""
        );
        assert_eq!(
            not_found(&[]).to_string(),
            "Could not find the function public.f without parameters in the schema cache"
        );
        assert_eq!(
            not_found(&["q", "z"]).to_string(),
            "Could not find the function public.f(q, z) in the schema cache"
        );
    }

    #[test]
    fn queries_filter_like_tables() {
        let db = novel();
        let fns = one(
            &db.0,
            "-- @param q text\n-- @param max integer = 10\n\
             SELECT title, status, length(title) AS len FROM novel WHERE title LIKE '%' || :q || '%' OR :q = '' LIMIT :max;",
        );
        let titles = |v: Json| {
            v.as_array().unwrap().iter().map(|r| r["title"].as_str().unwrap().to_string()).collect::<Vec<_>>()
        };
        assert_eq!(
            titles(call(&db, &fns, json!({ "q": "" }), "order=title").unwrap()),
            ["Quiet Harbor", "Signal Drift", "The Lantern Road"]
        );
        assert_eq!(
            titles(call(&db, &fns, json!({ "q": "a" }), "status=neq.hiatus&order=title.desc").unwrap()),
            ["The Lantern Road", "Signal Drift"]
        );
        assert_eq!(titles(call(&db, &fns, json!({ "q": "", "max": 1 }), "").unwrap()).len(), 1);
        assert_eq!(
            call(&db, &fns, json!({ "q": "Road" }), "select=t:title,len").unwrap(),
            json!([{ "t": "The Lantern Road", "len": 16 }])
        );
        assert_eq!(
            titles(call(&db, &fns, json!({ "q": "" }), "len=lt.13&order=title&limit=1&offset=0").unwrap()),
            ["Quiet Harbor"]
        );
        assert_eq!(
            call(&db, &fns, json!({ "q": "" }), "nope=eq.1").unwrap_err(),
            BuildError::ColumnNotFound { table: "f".into(), column: "nope".into() }
        );
        assert!(matches!(
            call(&db, &fns, json!({ "q": "" }), "select=title,edition(name)"),
            Err(BuildError::RelationshipNotFound { .. })
        ));

        // Count and paging, as a table read.
        let f = fns.get("f").unwrap();
        let a = body_args(f, json!({ "q": "" }));
        let out = run_query(&db.0, &db.1, f, &a, parse_query("limit=1&offset=1").unwrap(), None, true, LIMITS)
            .unwrap()
            .unwrap();
        let Outcome::Page { offset, page } = out else { panic!() };
        assert_eq!((offset, page.rows, page.total), (1, 1, Some(3)));

        // Same shape, same SQL: the statement cache hits.
        let sql = |raw: &str| {
            let read = sql::build_call(
                &db.1,
                &f.sql,
                &f.result(&db.0, db.1.version).unwrap(),
                params(f, &a),
                &parse_query(raw).unwrap(),
                None,
                false,
                LIMITS,
            )
            .unwrap();
            (read.stmt.sql, read.stmt.params)
        };
        let (s1, p1) = sql("title=eq.A");
        assert_eq!(s1, sql("title=eq.B").0);
        assert!(s1.starts_with("WITH \"rulite rpc\" AS (\n-- @param q text"));
        // :q (used twice) and :max are ?1 and ?2; the request's values follow.
        assert_eq!(p1[..3], [Value::Text(String::new()), Value::Integer(10), Value::Text("A".into())]);
        assert!(s1.contains("\"title\" = ?3"));
    }

    #[test]
    fn query_shapes() {
        let db = novel();
        let shape = |text: &str, body: Json, raw: &str| call(&db, &one(&db.0, text), body, raw);
        let row = "-- @param s text\n-- @returns row\nSELECT title, status FROM novel WHERE status = :s ORDER BY title";
        assert_eq!(
            shape(row, json!({ "s": "ongoing" }), "").unwrap(),
            json!({ "title": "Signal Drift", "status": "ongoing" })
        );
        assert_eq!(shape(row, json!({ "s": "none" }), "").unwrap(), Json::Null);
        let value =
            "-- @param s text = null\n-- @returns value\nSELECT count(*) FROM novel WHERE :s IS NULL OR status = :s";
        assert_eq!(shape(value, json!({}), "").unwrap(), json!(3));
        assert_eq!(shape(value, json!({ "s": "hiatus" }), "").unwrap(), json!(1));
        assert_eq!(shape("-- @returns value\nSELECT title FROM novel WHERE 0", json!({}), "").unwrap(), Json::Null);
        assert_eq!(shape("-- @returns value\nSELECT x'00ff'", json!({}), "").unwrap(), json!("\\x00ff"));
        let summary = "-- @returns json\nSELECT json_object('novels', count(*), 'titles', json_group_array(title)) FROM (SELECT title FROM novel ORDER BY title)";
        assert_eq!(
            shape(summary, json!({}), "").unwrap(),
            json!({ "novels": 3, "titles": ["Quiet Harbor", "Signal Drift", "The Lantern Road"] })
        );
        assert_eq!(shape("-- @returns void\nSELECT 1", json!({}), "").unwrap(), Json::Null);
        assert!(matches!(shape(value, json!({}), "limit=1"), Err(BuildError::Unsupported(_))));
        assert!(matches!(shape(summary, json!({}), "select=novels"), Err(BuildError::Unsupported(_))));

        // Reserved parameters.
        let who = one(&db.0, "-- @returns row\nSELECT :auth_uid AS uid, :auth_role AS role");
        let f = who.get("f").unwrap();
        let a = args(f, Source::Body(""), "authenticated", Some("u1")).unwrap();
        let out = run_query(&db.0, &db.1, f, &a, parse_query("").unwrap(), None, false, LIMITS).unwrap().unwrap();
        assert_eq!(out, Outcome::Body { body: Some(r#"{"uid":"u1","role":"authenticated"}"#.into()), rows: 1 });
    }

    #[test]
    fn procedures() {
        let (mut conn, _) = novel();
        let fns = check(
            &conn,
            vec![
                parse(
                    "add",
                    "-- @param title text\n-- @param genre text = null\n-- @returns row\n\
                     INSERT INTO novel (title, author, genre) VALUES (:title, coalesce(:auth_uid, 'anon'), :genre)\n\
                     RETURNING title, author, genre, length(id) AS id_len, x'0a' AS raw, 1.5 AS r;",
                )
                .unwrap(),
                parse(
                    "rename",
                    "-- @param from text\n-- @param to text\n\
                     UPDATE novel SET title = :to WHERE title = :from;\n\
                     SELECT title FROM novel WHERE title IN (:from, :to) ORDER BY title;",
                )
                .unwrap(),
                parse("wipe", "-- @returns void\nDELETE FROM part; DELETE FROM chapter;").unwrap(),
                parse("count", "-- @returns value\nUPDATE novel SET genre = genre RETURNING 1").unwrap(),
            ],
        )
        .unwrap();
        let run = |conn: &mut Connection, name: &str, body: Json, raw: &str| {
            let f = fns.get(name).unwrap();
            let a = args(f, Source::Body(&body.to_string()), "authenticated", Some("u1"))?;
            run_procedure(conn, f, &a, &parse_query(raw).unwrap()).unwrap()
        };
        let body = |out: Result<Outcome, BuildError>| match out.unwrap() {
            Outcome::Body { body, rows } => (body.map(|b| serde_json::from_str::<Json>(&b).unwrap()), rows),
            other => panic!("{other:?}"),
        };
        assert_eq!(
            body(run(&mut conn, "add", json!({ "title": "New" }), "")),
            (Some(json!({ "title": "New", "author": "u1", "genre": null, "id_len": 8, "raw": "\\x0a", "r": 1.5 })), 1)
        );
        assert_eq!(
            body(run(&mut conn, "rename", json!({ "from": "New", "to": "Newer" }), "")),
            (Some(json!([{ "title": "Newer" }])), 1)
        );
        assert_eq!(body(run(&mut conn, "count", json!({}), "")), (Some(json!(1)), 1));
        assert_eq!(body(run(&mut conn, "wipe", json!({}), "")), (None, 0));
        assert_eq!(conn.query_row("SELECT count(*) FROM chapter", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
        assert!(matches!(
            run(&mut conn, "rename", json!({ "from": "a", "to": "b" }), "title=eq.a"),
            Err(BuildError::Unsupported(_))
        ));

        // A failing statement rolls the whole call back.
        let f = fns.get("add").unwrap();
        let a = body_args(f, json!({ "title": "Newer" }));
        let e = run_procedure(&mut conn, f, &a, &parse_query("").unwrap()).unwrap_err();
        assert!(e.to_string().contains("UNIQUE"), "{e}");
        let renamed = fns.get("rename").unwrap();
        let both = "-- @param t text\n-- @returns void\nINSERT INTO novel (title, author) VALUES (:t, 'a'); INSERT INTO novel (title, author) VALUES (:t, 'a');";
        let dup = function(&conn, "dup", both).unwrap();
        let d = dup.get("dup").unwrap();
        assert!(
            run_procedure(&mut conn, d, &body_args(d, json!({ "t": "Twice" })), &parse_query("").unwrap()).is_err()
        );
        assert_eq!(
            conn.query_row("SELECT count(*) FROM novel WHERE title = 'Twice'", [], |r| r.get::<_, i64>(0)).unwrap(),
            0
        );
        assert_eq!(renamed.kind, Kind::Procedure);
    }
}
