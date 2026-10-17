//! SQL builder (read): query AST + schema cache → one SQL statement whose
//! single TEXT result is the JSON response body.
//!
//! Every table, column and relationship is resolved against the schema and
//! written from the schema's own names; request values, aliases and limits
//! are bound as numbered `?N` parameters, so equal query shapes share one
//! cached statement.

use std::fmt::{self, Write};
use std::sync::Arc;

use rusqlite::Connection;
use rusqlite::types::Value;

use crate::query::{Cmp, Condition, Embed, IsValue, LogicOp, Nulls, Op, OrderTerm, Query, Range, Scope, SelectItem};
use crate::schema::{Affinity, Cardinality, Column, RelError, Schema, Table};

#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// Root rows when the request gives no limit.
    pub default: u32,
    /// Upper bound for the root and for every embedded array.
    pub max: u32,
}

#[derive(Debug, PartialEq)]
pub enum BuildError {
    TableNotFound(String),
    ColumnNotFound {
        table: String,
        column: String,
    },
    RelationshipNotFound {
        from: String,
        to: String,
    },
    /// Several relationships match; `options` lists the possible hints.
    Ambiguous {
        from: String,
        to: String,
        options: Vec<String>,
    },
    /// Filters or modifiers on an embed path that the `select` doesn't have.
    UnknownEmbed(String),
    /// Write body is not JSON, or not the expected object / array of objects.
    InvalidBody(String),
    /// A body (or `columns`) key that is not a column of the table.
    PayloadColumn {
        table: String,
        column: String,
    },
    /// `on_conflict` (or the default primary key) matches no unique key.
    NoUniqueKey {
        table: String,
        columns: Vec<String>,
    },
    /// UPDATE / DELETE without a filter; holds the verb.
    MissingFilter(&'static str),
    /// A single object was requested, but this many rows came back.
    NotSingular(u64),
    /// Valid syntax that this endpoint doesn't support.
    Unsupported(String),
    /// The role lacks the permission on this table.
    PermissionDenied(String),
    /// A written row belongs to another user (owner column).
    RowPolicy(String),
}

impl fmt::Display for BuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BuildError::TableNotFound(t) => write!(f, "Could not find the table '{t}' in the schema cache"),
            BuildError::ColumnNotFound { table, column } => write!(f, "column {table}.{column} does not exist"),
            BuildError::RelationshipNotFound { from, to } => {
                write!(f, "Could not find a relationship between '{from}' and '{to}' in the schema cache")
            }
            BuildError::Ambiguous { from, to, .. } => {
                write!(f, "Could not embed because more than one relationship was found for '{from}' and '{to}'")
            }
            BuildError::UnknownEmbed(path) => {
                write!(f, "'{path}' is not an embedded resource in this request")
            }
            BuildError::InvalidBody(msg) => write!(f, "Empty or invalid json: {msg}"),
            BuildError::PayloadColumn { table, column } => {
                write!(f, "Could not find the '{column}' column of '{table}' in the schema cache")
            }
            BuildError::NoUniqueKey { table, columns } => write!(
                f,
                "there is no unique or exclusion constraint on {table}({}) matching the ON CONFLICT specification",
                columns.join(", ")
            ),
            BuildError::MissingFilter(verb) => write!(f, "{verb} requires a WHERE clause"),
            BuildError::NotSingular(_) => f.write_str("JSON object requested, multiple (or no) rows returned"),
            BuildError::Unsupported(msg) => f.write_str(msg),
            BuildError::PermissionDenied(t) => write!(f, "permission denied for table {t}"),
            BuildError::RowPolicy(t) => write!(f, "new row violates row-level security policy for table \"{t}\""),
        }
    }
}

type Result<T> = std::result::Result<T, BuildError>;

/// What a role may do on a table; `Write` includes `Read`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum Perm {
    #[default]
    None,
    Read,
    Write,
}

/// One table's permission for a role.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Rule {
    pub perm: Perm,
    /// Column holding the owner's user id: only rows where it equals the uid.
    pub owner: Option<String>,
    /// `owner_scope: "write"`: every row is readable, only own rows writable.
    pub read_all: bool,
}

/// A role's rules: per table (sorted by name), else `default`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Policy {
    pub tables: Vec<(String, Rule)>,
    pub default: Rule,
}

impl Policy {
    pub fn rule(&self, table: &str) -> &Rule {
        match self.tables.binary_search_by(|(t, _)| t.as_str().cmp(table)) {
            Ok(i) => &self.tables[i].1,
            Err(_) => &self.default,
        }
    }
}

/// Per-request access. Without a policy (auth off, `service_role`) nothing is
/// checked; `uid` is the signed-in user, bound as a parameter, never inlined.
#[derive(Debug, Clone, Default)]
pub struct Grant {
    pub policy: Option<Arc<Policy>>,
    pub uid: Option<String>,
}

impl Grant {
    /// Checks `need` on `table`; returns the owner column that limits the
    /// rows, if any. A configured owner column the table lacks denies it.
    pub fn owner<'t>(&self, table: &'t Table, need: Perm) -> Result<Option<&'t Column>> {
        let Some(policy) = &self.policy else { return Ok(None) };
        let rule = policy.rule(&table.name);
        let denied = || BuildError::PermissionDenied(table.name.clone());
        if rule.perm < need {
            return Err(denied());
        }
        match &rule.owner {
            None => Ok(None),
            Some(_) if self.uid.is_none() => Err(denied()),
            Some(_) if need == Perm::Read && rule.read_all => Ok(None),
            Some(name) => table.column(name).map(Some).ok_or_else(denied),
        }
    }
}

/// A generated statement and its parameters, in `?N` order.
#[derive(Debug)]
pub struct Statement {
    pub sql: String,
    pub params: Vec<Value>,
}

impl Statement {
    /// Runs on `conn` through its statement cache; returns the JSON body.
    pub fn run(&self, conn: &Connection) -> rusqlite::Result<String> {
        let mut stmt = conn.prepare_cached(&self.sql)?;
        stmt.query_row(rusqlite::params_from_iter(&self.params), |r| r.get(0))
    }

    /// Runs a write; returns how many rows it changed.
    pub fn execute(&self, conn: &Connection) -> rusqlite::Result<usize> {
        conn.prepare_cached(&self.sql)?.execute(rusqlite::params_from_iter(&self.params))
    }

    /// Runs a write whose `RETURNING` yields one JSON text per row; returns
    /// the row count and those values as one JSON array.
    pub fn returning(&self, conn: &Connection) -> rusqlite::Result<(usize, String)> {
        let mut stmt = conn.prepare_cached(&self.sql)?;
        let mut rows = stmt.query(rusqlite::params_from_iter(&self.params))?;
        let (mut n, mut out) = (0, String::from("["));
        while let Some(r) = rows.next()? {
            if n > 0 {
                out.push(',');
            }
            out.push_str(
                r.get_ref(0)?.as_str().map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
                })?,
            );
            n += 1;
        }
        out.push(']');
        Ok((n, out))
    }
}

/// `GET /rest/v1/{table}`: one page of the matching rows.
#[derive(Debug)]
pub struct Read {
    pub stmt: Statement,
    /// Position of the page's first row among all matching rows.
    pub offset: u64,
}

/// What a [`Read`] returns: the body plus what `Content-Range` needs.
#[derive(Debug, PartialEq)]
pub struct Page {
    pub body: String,
    pub rows: u64,
    /// Rows matching the filters, ignoring paging; only with `count`.
    pub total: Option<u64>,
}

impl Read {
    pub fn run(&self, conn: &Connection) -> rusqlite::Result<Page> {
        let mut stmt = conn.prepare_cached(&self.stmt.sql)?;
        stmt.query_row(rusqlite::params_from_iter(&self.stmt.params), |r| {
            Ok(Page {
                body: r.get(0)?,
                rows: r.get::<_, i64>(1)? as u64,
                total: r.get::<_, Option<i64>>(2)?.map(|n| n as u64),
            })
        })
    }
}

/// `count` adds the total row count (`Prefer: count=exact`) to the same statement.
pub fn build_read(
    schema: &Schema,
    table: &str,
    query: &Query,
    range: Option<Range>,
    count: bool,
    limits: Limits,
    grant: &Grant,
) -> Result<Read> {
    let stmt = build_select(schema, table, query, Rows::Page { range, count }, limits, grant)?;
    let (_, offset) = paging(query.root(), range, limits);
    Ok(Read { stmt, offset })
}

/// Which root rows a select returns.
pub enum Rows {
    /// The request's filters, paged by `limit`/`offset`, `Range` and the
    /// default limit; `count` also counts every matching row.
    Page { range: Option<Range>, count: bool },
    /// The request's filters, every row the role may write (what a DELETE
    /// is about to remove).
    All,
    /// Rows by key, as a JSON array of [`row_key`] arrays (what a write
    /// returned); the request's root filters are not applied.
    Keys(String),
}

/// Root `(limit, offset)`: the query's own, else the `Range` header's, else
/// the default limit; clamped to the max.
fn paging(root: &Scope, range: Option<Range>, limits: Limits) -> (u64, u64) {
    let limit = root.limit.or(range.and_then(|r| r.limit)).unwrap_or(limits.default.into());
    let offset = root.offset.or(range.map(|r| r.offset)).unwrap_or(0);
    (limit.min(limits.max.into()), offset.min(i64::MAX as u64))
}

/// One row: a JSON array of the matching rows (embeds and all), the number
/// of rows in it, and the total count or NULL.
pub fn build_select(
    schema: &Schema,
    table: &str,
    query: &Query,
    rows: Rows,
    limits: Limits,
    grant: &Grant,
) -> Result<Statement> {
    let table = find_table(schema, table)?;
    let mut b = Builder::new(schema, query, limits, grant);
    let alias = b.alias();
    let need = if matches!(rows, Rows::All) { Perm::Write } else { Perm::Read };
    let owner = b.access(table, &alias, need)?;
    let mut path = Vec::new();
    let (object, mut conds) = b.object(table, &alias, &query.select, &mut path)?;
    conds.extend(owner);

    let root = query.root();
    b.used[0] = true;
    let (page, count) = match rows {
        Rows::Page { range, count } => {
            conds.extend(b.filters(table, &alias, &root.filters)?);
            (Some(range), count)
        }
        Rows::All => {
            conds.extend(b.filters(table, &alias, &root.filters)?);
            (None, false)
        }
        Rows::Keys(keys) => {
            let key = row_key(table);
            let cols: Vec<String> = key.iter().map(|k| format!("{alias}.{k}")).collect();
            let values: Vec<String> = (0..key.len()).map(|i| format!("value ->> {i}")).collect();
            let keys = b.param(Value::Text(keys));
            conds.push(format!("({}) IN (SELECT {} FROM json_each({keys}))", cols.join(", "), values.join(", ")));
            (None, false)
        }
    };
    let order = b.order(table, &alias, &root.order)?;

    let mut paging_sql = String::new();
    if let Some(range) = page {
        let (limit, offset) = paging(root, range, limits);
        let limit = b.param(Value::Integer(limit as i64));
        let offset = b.param(Value::Integer(offset as i64));
        paging_sql = format!(" LIMIT {limit} OFFSET {offset}");
    }

    if let Some(i) = b.used.iter().position(|u| !u) {
        return Err(BuildError::UnknownEmbed(query.scopes[i].path.join(".")));
    }

    let mut from = format!(" FROM {} AS {alias}", ident(&table.name));
    push_where(&mut from, &conds);
    // The count reuses the page's conditions (and their `?N` parameters).
    let total = if count { format!("(SELECT count(*){from})") } else { "NULL".to_string() };
    let sql = format!(
        "SELECT json_group_array(json(j)), count(*), {total} FROM (SELECT {object} AS j{from}{order}{paging_sql})"
    );
    Ok(Statement { sql, params: b.params })
}

pub fn find_table<'s>(schema: &'s Schema, name: &str) -> Result<&'s Table> {
    schema.table(name).ok_or_else(|| BuildError::TableNotFound(name.to_string()))
}

/// SQL expressions identifying a row: `rowid`, or the primary key of a
/// `WITHOUT ROWID` table. Unqualified, so they also work in `RETURNING`.
pub fn row_key(table: &Table) -> Vec<String> {
    if table.without_rowid { table.primary_key.iter().map(|c| ident(c)).collect() } else { vec!["rowid".to_string()] }
}

pub struct Builder<'a> {
    schema: &'a Schema,
    query: &'a Query,
    limits: Limits,
    grant: &'a Grant,
    /// `?N` of the bound uid, once an owner condition needs it.
    uid: Option<String>,
    params: Vec<Value>,
    aliases: usize,
    /// Which of `query.scopes` an embed picked up.
    used: Vec<bool>,
}

impl<'a> Builder<'a> {
    pub fn new(schema: &'a Schema, query: &'a Query, limits: Limits, grant: &'a Grant) -> Builder<'a> {
        let used = vec![false; query.scopes.len()];
        Builder { schema, query, limits, grant, uid: None, params: Vec::new(), aliases: 0, used }
    }

    pub fn into_params(self) -> Vec<Value> {
        self.params
    }

    pub fn param(&mut self, v: Value) -> String {
        self.params.push(v);
        format!("?{}", self.params.len())
    }

    /// Checks `need` on `table`; returns `alias."owner" = ?uid` when the
    /// grant limits the rows to the user's own.
    pub fn access(&mut self, table: &Table, alias: &str, need: Perm) -> Result<Option<String>> {
        let Some(owner) = self.grant.owner(table, need)? else { return Ok(None) };
        let uid = match &self.uid {
            Some(p) => p.clone(),
            None => {
                let p = self.param(Value::Text(self.grant.uid.clone().unwrap_or_default()));
                self.uid = Some(p.clone());
                p
            }
        };
        Ok(Some(format!("{alias}.{} = {uid}", ident(&owner.name))))
    }

    pub fn alias(&mut self) -> String {
        self.aliases += 1;
        format!("t{}", self.aliases - 1)
    }

    fn scope(&mut self, path: &[String]) -> Option<&'a Scope> {
        let i = self.query.scopes.iter().position(|s| s.path == path)?;
        self.used[i] = true;
        Some(&self.query.scopes[i])
    }

    /// `json_object(...)` for one row of `table` (as `alias`), plus the
    /// `EXISTS` conditions that `!inner` embeds add to that row's `WHERE`.
    fn object(
        &mut self,
        table: &Table,
        alias: &str,
        items: &[SelectItem],
        path: &mut Vec<String>,
    ) -> Result<(String, Vec<String>)> {
        let mut args = Vec::new();
        let mut conds = Vec::new();
        for item in items {
            match item {
                SelectItem::Star => {
                    for c in &table.columns {
                        args.push(format!("{}, {}", literal(&c.name), column_value(table, alias, c)));
                    }
                }
                SelectItem::Column { alias: key, name } => {
                    let c = column(table, name)?;
                    let key = match key {
                        Some(k) => self.param(Value::Text(k.clone())),
                        None => literal(&c.name),
                    };
                    args.push(format!("{key}, {}", column_value(table, alias, c)));
                }
                SelectItem::Embed(e) => {
                    path.push(e.alias.clone().unwrap_or_else(|| e.relation.clone()));
                    let (value, exists) = self.embed(table, alias, e, path)?;
                    path.pop();
                    if let Some(value) = value {
                        let key = match &e.alias {
                            Some(k) => self.param(Value::Text(k.clone())),
                            None => literal(&e.relation),
                        };
                        args.push(format!("{key}, json({value})"));
                    }
                    conds.extend(exists);
                }
            }
        }
        Ok((format!("json_object({})", args.join(", ")), conds))
    }

    /// The embed's correlated subquery (`None` for `rel()`), and its `EXISTS`
    /// condition when `!inner`.
    fn embed(
        &mut self,
        parent: &Table,
        parent_alias: &str,
        e: &Embed,
        path: &mut Vec<String>,
    ) -> Result<(Option<String>, Option<String>)> {
        let rel =
            self.schema.relationship_to(&parent.name, &e.relation, e.hint.as_deref(), e.to_many).map_err(|err| {
                match err {
                    RelError::NotFound => {
                        BuildError::RelationshipNotFound { from: parent.name.clone(), to: e.relation.clone() }
                    }
                    RelError::Ambiguous(found) => BuildError::Ambiguous {
                        from: parent.name.clone(),
                        to: e.relation.clone(),
                        options: found
                            .iter()
                            .map(|r| match (r.is_self_reference(), r.kind) {
                                (false, _) => r.hint(),
                                (true, Cardinality::ManyToOne) => format!("{}!one", r.hint()),
                                (true, _) => format!("{}!many", r.hint()),
                            })
                            .collect(),
                    },
                }
            })?;
        let target = self.schema.table(&rel.foreign_table).expect("relationships point at known tables");
        let alias = self.alias();

        // FROM + correlation with the parent row.
        let mut from = format!("{} AS {alias}", ident(&target.name));
        let mut conds: Vec<String> = self.access(target, &alias, Perm::Read)?.into_iter().collect();
        match &rel.junction {
            None => {
                for (c, fc) in rel.columns.iter().zip(&rel.foreign_columns) {
                    conds.push(format!("{alias}.{} = {parent_alias}.{}", ident(fc), ident(c)));
                }
            }
            Some(j) => {
                let jalias = format!("j{}", &alias[1..]);
                let on: Vec<String> = rel
                    .foreign_columns
                    .iter()
                    .zip(&j.foreign_columns)
                    .map(|(fc, jc)| format!("{alias}.{} = {jalias}.{}", ident(fc), ident(jc)))
                    .collect();
                from = format!("{} AS {jalias} JOIN {from} ON {}", ident(&j.table), on.join(" AND "));
                let junction = self.schema.table(&j.table).expect("junctions are known tables");
                conds.extend(self.access(junction, &jalias, Perm::Read)?);
                for (c, jc) in rel.columns.iter().zip(&j.columns) {
                    conds.push(format!("{jalias}.{} = {parent_alias}.{}", ident(jc), ident(c)));
                }
            }
        }

        let (object, inner_conds) = self.object(target, &alias, &e.select, path)?;
        conds.extend(inner_conds);
        let scope = self.scope(path);
        if let Some(s) = scope {
            conds.extend(self.filters(target, &alias, &s.filters)?);
        }

        let mut body = format!(" FROM {from}");
        push_where(&mut body, &conds);
        let exists = e.inner.then(|| format!("EXISTS (SELECT 1{body})"));
        if e.select.is_empty() {
            return Ok((None, exists));
        }

        let value = if rel.kind == Cardinality::ManyToOne {
            format!("(SELECT {object}{body} LIMIT 1)")
        } else {
            let order = match scope {
                Some(s) => self.order(target, &alias, &s.order)?,
                None => String::new(),
            };
            let max = u64::from(self.limits.max);
            let limit = scope.and_then(|s| s.limit).unwrap_or(max).min(max);
            let offset = scope.and_then(|s| s.offset).unwrap_or(0).min(i64::MAX as u64);
            let limit = self.param(Value::Integer(limit as i64));
            let offset = self.param(Value::Integer(offset as i64));
            format!(
                "(SELECT json_group_array(json(j)) FROM (SELECT {object} AS j{body}{order} LIMIT {limit} OFFSET {offset}))"
            )
        };
        Ok((Some(value), exists))
    }

    pub fn filters(&mut self, table: &Table, alias: &str, filters: &[Condition]) -> Result<Vec<String>> {
        filters.iter().map(|c| self.condition(table, alias, c)).collect()
    }

    fn condition(&mut self, table: &Table, alias: &str, cond: &Condition) -> Result<String> {
        let (negated, sql) = match cond {
            Condition::Logic { negated, op, conditions } => {
                let parts = self.filters(table, alias, conditions)?;
                let sep = if *op == LogicOp::Or { " OR " } else { " AND " };
                (*negated, format!("({})", parts.join(sep)))
            }
            Condition::Filter { column: name, negated, op } => {
                let col = format!("{alias}.{}", ident(&column(table, name)?.name));
                let sql = match op {
                    Op::Cmp(cmp, value) => {
                        let (op, value) = match cmp {
                            Cmp::Eq => ("=", value.clone()),
                            Cmp::Neq => ("<>", value.clone()),
                            Cmp::Gt => (">", value.clone()),
                            Cmp::Gte => (">=", value.clone()),
                            Cmp::Lt => ("<", value.clone()),
                            Cmp::Lte => ("<=", value.clone()),
                            // SQLite's LIKE ignores ASCII case; GLOB is the case-sensitive match.
                            Cmp::Like => ("GLOB", like_to_glob(value)),
                            Cmp::Ilike => ("LIKE", value.clone()),
                        };
                        format!("{col} {op} {}", self.param(Value::Text(value)))
                    }
                    Op::Is(IsValue::Null) => format!("{col} IS NULL"),
                    Op::Is(IsValue::True) => format!("{col} IS TRUE"),
                    Op::Is(IsValue::False) => format!("{col} IS FALSE"),
                    Op::In(values) if values.is_empty() => "0".to_string(),
                    Op::In(values) => {
                        let list: Vec<String> = values.iter().map(|v| self.param(Value::Text(v.clone()))).collect();
                        format!("{col} IN ({})", list.join(", "))
                    }
                };
                (*negated, sql)
            }
        };
        Ok(if negated { format!("NOT ({sql})") } else { sql })
    }

    /// ` ORDER BY ...`, or empty. Nulls default to PostgreSQL's placement
    /// (last when ascending, first when descending).
    fn order(&mut self, table: &Table, alias: &str, terms: &[OrderTerm]) -> Result<String> {
        if terms.is_empty() {
            return Ok(String::new());
        }
        let mut sql = String::from(" ORDER BY ");
        for (i, t) in terms.iter().enumerate() {
            let c = column(table, &t.column)?;
            let nulls = t.nulls.unwrap_or(if t.descending { Nulls::First } else { Nulls::Last });
            write!(
                sql,
                "{}{alias}.{} {} NULLS {}",
                if i > 0 { ", " } else { "" },
                ident(&c.name),
                if t.descending { "DESC" } else { "ASC" },
                if nulls == Nulls::First { "FIRST" } else { "LAST" },
            )
            .unwrap();
        }
        Ok(sql)
    }
}

pub fn column<'t>(table: &'t Table, name: &str) -> Result<&'t Column> {
    table.column(name).ok_or_else(|| BuildError::ColumnNotFound { table: table.name.clone(), column: name.to_string() })
}

/// Column as a JSON value. Numbers, text and NULL map directly; JSON columns
/// stay JSON; BLOBs become PostgreSQL-style `\x` hex strings.
fn column_value(table: &Table, alias: &str, c: &Column) -> String {
    let col = format!("{alias}.{}", ident(&c.name));
    if c.json {
        format!("iif(json_valid({col}), json({col}), {col})")
    } else if c.affinity == Affinity::Blob || !table.strict {
        // Non-STRICT tables can hold a BLOB in any column.
        format!("CASE WHEN typeof({col}) = 'blob' THEN '\\x' || lower(hex({col})) ELSE {col} END")
    } else {
        col
    }
}

pub fn push_where(sql: &mut String, conds: &[String]) {
    if !conds.is_empty() {
        write!(sql, " WHERE {}", conds.join(" AND ")).unwrap();
    }
}

/// `"name"`, with embedded quotes doubled. Only ever called with schema names.
pub fn ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// `'text'`, with embedded quotes doubled. Only ever called with schema names.
pub fn literal(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// LIKE pattern (`%`, `_`) → GLOB pattern, with GLOB's own wildcards escaped.
fn like_to_glob(pattern: &str) -> String {
    let mut out = String::with_capacity(pattern.len());
    for c in pattern.chars() {
        match c {
            '%' => out.push('*'),
            '_' => out.push('?'),
            '*' => out.push_str("[*]"),
            '?' => out.push_str("[?]"),
            '[' => out.push_str("[[]"),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::parse_query;
    use crate::schema::introspect;
    use serde_json::{Value as Json, json};

    const LIMITS: Limits = Limits { default: 100, max: 1000 };

    fn db(sql: &str) -> (Connection, Schema) {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(sql).unwrap();
        let schema = introspect(&conn).unwrap();
        (conn, schema)
    }

    fn novel() -> (Connection, Schema) {
        db(include_str!("../sample/novel.sql"))
    }

    fn event() -> (Connection, Schema) {
        db(include_str!("../sample/event.sql"))
    }

    fn get(db: &(Connection, Schema), table: &str, raw: &str) -> Json {
        let q = parse_query(raw).unwrap();
        let read = build_read(&db.1, table, &q, None, false, LIMITS, &Grant::default())
            .unwrap_or_else(|e| panic!("{raw}: {e}"));
        let page = read.run(&db.0).unwrap_or_else(|e| panic!("{raw}: {e}\n{}", read.stmt.sql));
        serde_json::from_str(&page.body).unwrap()
    }

    fn build_err(db: &(Connection, Schema), table: &str, raw: &str) -> BuildError {
        build_read(&db.1, table, &parse_query(raw).unwrap(), None, false, LIMITS, &Grant::default()).unwrap_err()
    }

    fn titles(v: &Json, key: &str) -> Vec<String> {
        v.as_array().unwrap().iter().map(|r| r[key].as_str().unwrap().to_string()).collect()
    }

    // --- Done-when queries (PLAN.md M3) ---

    #[test]
    fn novel_to_part_in_one_statement() {
        let db = novel();
        let v = get(
            &db,
            "novel",
            "select=title,edition(language,volume(number,chapter(title,part(content))))\
             &title=eq.The%20Lantern%20Road&edition.order=language&edition.volume.order=number\
             &edition.volume.chapter.order=number&edition.volume.chapter.part.order=number",
        );
        let parts = |ch: &str, contents: &[&str]| json!({ "title": ch, "part": contents.iter().map(|c| json!({ "content": c })).collect::<Vec<_>>() });
        assert_eq!(
            v,
            json!([{
                "title": "The Lantern Road",
                "edition": [
                    { "language": "en", "volume": [
                        { "number": 1, "chapter": [
                            parts("A Light in the Fog", &["The road had no end that anyone could remember.", "Then a lantern swung out of the fog."]),
                            parts("The Toll Keeper", &["The toll keeper asked for a name, not a coin.", "Aren gave the wrong one."]),
                        ]},
                        { "number": 2, "chapter": [parts("Ashes at Dawn", &["By morning the village was smoke."])] },
                    ]},
                    { "language": "vi", "volume": [
                        { "number": 1, "chapter": [parts("Ánh Sáng Trong Sương", &["Con đường không có điểm cuối mà ai còn nhớ."])] },
                    ]},
                ],
            }])
        );
    }

    #[test]
    fn event_area_sub_event_artist_in_one_statement() {
        let db = event();
        let v = get(
            &db,
            "event",
            "select=name,area(name,parent:area!parent_id(name)),sub_event(title,area(name),artist(name))\
             &name=eq.Saigon%20Sound%20Fest%202026&sub_event.order=starts_at&sub_event.artist.order=name",
        );
        assert_eq!(
            v,
            json!([{
                "name": "Saigon Sound Fest 2026",
                "area": { "name": "Riverside Park", "parent": { "name": "Ho Chi Minh City" } },
                "sub_event": [
                    { "title": "Acoustic Sunset", "area": { "name": "Garden Stage" }, "artist": [{ "name": "Paper Lanterns" }] },
                    { "title": "Opening Night", "area": { "name": "Main Stage" },
                      "artist": [{ "name": "DJ Nova" }, { "name": "Kai Lam" }, { "name": "Thu Ha" }] },
                    { "title": "Closing Party", "area": { "name": "Main Stage" }, "artist": [{ "name": "DJ Nova" }, { "name": "Kai Lam" }] },
                ],
            }])
        );
    }

    // --- T3.2 flat select ---

    #[test]
    fn flat_select_filter_order_limit() {
        let db = novel();
        let v = get(&db, "novel", "select=t:title,status&status=neq.hiatus&order=title.desc&limit=10");
        assert_eq!(
            v,
            json!([
                { "t": "The Lantern Road", "status": "completed" },
                { "t": "Signal Drift", "status": "ongoing" },
            ])
        );
        assert_eq!(get(&db, "novel", "title=eq.nope"), json!([]));
        let star = get(&db, "novel", "limit=1");
        let keys: Vec<&str> = star[0].as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(keys.len(), 6);
        assert!(keys.contains(&"created_at"));
    }

    #[test]
    fn filter_operators() {
        let db = novel();
        let t = |raw: &str| {
            let mut v = titles(&get(&db, "novel", &format!("select=title&{raw}")), "title");
            v.sort();
            v
        };
        assert_eq!(t("or=(genre.eq.fantasy,status.eq.hiatus)"), ["Quiet Harbor", "The Lantern Road"]);
        assert_eq!(t("not.or=(genre.eq.fantasy,status.eq.hiatus)"), ["Signal Drift"]);
        assert_eq!(t("title=like.*Road"), ["The Lantern Road"]);
        assert_eq!(t("title=like.*road"), Vec::<String>::new()); // case-sensitive
        assert_eq!(t("title=ilike.*road"), ["The Lantern Road"]);
        assert_eq!(t("title=like.Q_iet*"), ["Quiet Harbor"]);
        assert_eq!(t("genre=in.(sci-fi,mystery)"), ["Quiet Harbor", "Signal Drift"]);
        assert_eq!(t("genre=not.in.(sci-fi,mystery)"), ["The Lantern Road"]);
        assert_eq!(t("genre=in.()"), Vec::<String>::new());
        assert_eq!(t("genre=is.null"), Vec::<String>::new());
        assert_eq!(t("genre=not.is.null&title=gte.R"), ["Signal Drift", "The Lantern Road"]);
        assert_eq!(t("and=(title.gt.A,or(genre.eq.sci-fi,genre.eq.mystery))&title=lt.R"), ["Quiet Harbor"]);
    }

    #[test]
    fn numeric_filters_use_column_affinity() {
        let db = novel();
        let v = get(&db, "part", "select=word_count&word_count=gte.10");
        assert_eq!(v.as_array().unwrap().len(), 2);
        assert!(v.as_array().unwrap().iter().all(|r| r["word_count"].as_i64().unwrap() >= 10));
    }

    #[test]
    fn order_nulls_and_values_are_bound() {
        let (conn, schema) = event();
        let db = (conn, schema);
        let v = get(&db, "sub_event", "select=title,price&order=price.desc&limit=2");
        assert_eq!(v[0]["price"], Json::Null); // nulls first on desc, like PostgreSQL
        let v = get(&db, "sub_event", "select=price&order=price.asc&limit=1");
        assert_eq!(v[0]["price"], json!(0.0));

        // An injection attempt is just a value that matches nothing.
        let q = parse_query("title=eq.x'%20OR%201=1--").unwrap();
        let stmt = build_read(&db.1, "sub_event", &q, None, false, LIMITS, &Grant::default()).unwrap().stmt;
        assert!(!stmt.sql.contains("OR 1=1"));
        assert_eq!(stmt.run(&db.0).unwrap(), "[]");
    }

    // --- T3.3 embedding ---

    #[test]
    fn many_to_one_chain() {
        let db = novel();
        let v = get(&db, "part", "select=content,chapter(title,volume(title))&word_count=gte.10&order=content");
        assert_eq!(
            v[0],
            json!({
                "content": "Con đường không có điểm cuối mà ai còn nhớ.",
                "chapter": { "title": "Ánh Sáng Trong Sương", "volume": { "title": "Dặm Đường Tối" } },
            })
        );
        assert_eq!(v.as_array().unwrap().len(), 2);
    }

    #[test]
    fn self_reference_both_directions() {
        let db = event();
        let v = get(
            &db,
            "area",
            "select=name,children:area!parent_id!many(name,capacity)&kind=eq.venue&order=name&children.order=name",
        );
        assert_eq!(
            v,
            json!([
                { "name": "Beach Arena", "children": [{ "name": "Sand Stage", "capacity": 8000 }] },
                { "name": "Opera House", "children": [{ "name": "Grand Hall", "capacity": 500 }] },
                { "name": "Riverside Park", "children": [
                    { "name": "Garden Stage", "capacity": 3000 }, { "name": "Main Stage", "capacity": 15000 },
                ]},
            ])
        );
        let v = get(&db, "area", "select=name,parent:area!parent_id(name)&kind=eq.city&limit=1");
        assert_eq!(v[0]["parent"], Json::Null);
    }

    #[test]
    fn many_to_many_reverse_and_empty() {
        let db = event();
        let v = get(&db, "artist", "select=name,sub_event(title,event(name))&country=eq.VN&name=eq.Lotus%20Quartet");
        assert_eq!(
            v,
            json!([{ "name": "Lotus Quartet", "sub_event": [{ "title": "Quartet Evening", "event": { "name": "Winter Strings" } }] }])
        );
        let v = get(&db, "artist", "select=name,sub_event(title)&name=eq.Unbooked%20Band");
        assert_eq!(v[0]["sub_event"], json!([]));
    }

    #[test]
    fn embedded_filters_limit_and_inner() {
        let db = novel();
        // Embedded filter narrows the array only.
        let v = get(&db, "novel", "select=title,edition(language)&edition.language=eq.vi&order=title");
        assert_eq!(v.as_array().unwrap().len(), 3);
        assert_eq!(v[2]["edition"], json!([{ "language": "vi" }]));
        assert_eq!(v[0]["edition"], json!([]));

        // `!inner` also drops parents without a match.
        let v = get(&db, "novel", "select=title,edition!inner(language)&edition.language=eq.vi");
        assert_eq!(titles(&v, "title"), ["The Lantern Road"]);

        // `rel!inner()` filters without embedding.
        let v = get(&db, "novel", "select=title,edition!inner()&edition.language=eq.vi");
        assert_eq!(v, json!([{ "title": "The Lantern Road" }]));

        // Nested inner through two levels.
        let v = get(&db, "novel", "select=title,edition!inner(volume!inner())&edition.volume.number=eq.2");
        assert_eq!(titles(&v, "title"), ["The Lantern Road"]);

        let v = get(
            &db,
            "novel",
            "select=title,edition(language)&title=eq.The%20Lantern%20Road&edition.order=language.desc&edition.limit=1",
        );
        assert_eq!(v[0]["edition"], json!([{ "language": "vi" }]));
    }

    // --- T3.4 types ---

    #[test]
    fn json_bool_real_and_blob_types() {
        let db = event();
        let v = get(&db, "artist", "select=name,genres&name=eq.Thu%20Ha");
        assert_eq!(v[0]["genres"], json!(["pop", "ballad"]));
        let v = get(&db, "event", "select=is_public&name=eq.Da%20Nang%20Waves");
        assert_eq!(v[0]["is_public"], json!(0));

        let db = db_blob();
        let v = get(&db, "doc", "order=id");
        assert_eq!(
            v,
            json!([
                { "id": 1, "data": "\\x00ff", "meta": { "a": 1 }, "loose": "\\x41" },
                { "id": 2, "data": null, "meta": "not json", "loose": "2.5" }, // TEXT affinity
            ])
        );
    }

    fn db_blob() -> (Connection, Schema) {
        db("CREATE TABLE doc (id INTEGER PRIMARY KEY, data BLOB, meta JSON, loose TEXT);
            INSERT INTO doc VALUES (1, x'00ff', '{\"a\":1}', x'41'), (2, NULL, 'not json', 2.5);")
    }

    // --- T3.1 validation ---

    #[test]
    fn unknown_names_are_rejected() {
        let db = event();
        assert_eq!(build_err(&db, "nope", ""), BuildError::TableNotFound("nope".into()));
        assert_eq!(
            build_err(&db, "area", "select=nope"),
            BuildError::ColumnNotFound { table: "area".into(), column: "nope".into() }
        );
        assert_eq!(
            build_err(&db, "area", "nope=eq.1"),
            BuildError::ColumnNotFound { table: "area".into(), column: "nope".into() }
        );
        assert_eq!(
            build_err(&db, "area", "order=nope"),
            BuildError::ColumnNotFound { table: "area".into(), column: "nope".into() }
        );
        assert_eq!(
            build_err(&db, "area", "or=(name.eq.a,x.eq.b)"),
            BuildError::ColumnNotFound { table: "area".into(), column: "x".into() }
        );
        assert_eq!(
            build_err(&db, "event", "select=sub_event(nope)"),
            BuildError::ColumnNotFound { table: "sub_event".into(), column: "nope".into() }
        );
        assert_eq!(
            build_err(&db, "event", "select=artist(name)"),
            BuildError::RelationshipNotFound { from: "event".into(), to: "artist".into() }
        );
        assert!(
            matches!(build_err(&db, "area", "select=area(name)"), BuildError::Ambiguous { options, .. } if options.len() == 2)
        );
        assert_eq!(build_err(&db, "event", "sub_event.title=eq.x"), BuildError::UnknownEmbed("sub_event".into()));
    }

    #[test]
    fn quoted_identifiers_are_escaped() {
        let db = db(r#"CREATE TABLE "we""ird" ("co""l" TEXT, "it's" INTEGER); INSERT INTO "we""ird" VALUES ('x', 1);"#);
        assert_eq!(get(&db, "we\"ird", "it's=eq.1"), json!([{ "co\"l": "x", "it's": 1 }]));
    }

    // --- T3.5 limits ---

    #[test]
    fn default_and_max_limits() {
        let db = novel();
        let small = Limits { default: 2, max: 3 };
        let run = |raw: &str, range: Option<Range>| {
            let read =
                build_read(&db.1, "part", &parse_query(raw).unwrap(), range, false, small, &Grant::default()).unwrap();
            serde_json::from_str::<Json>(&read.run(&db.0).unwrap().body).unwrap().as_array().unwrap().len()
        };
        assert_eq!(run("", None), 2);
        assert_eq!(run("limit=50", None), 3);
        assert_eq!(run("limit=1&offset=9", None), 1);
        assert_eq!(run("offset=9", None), 1);
        assert_eq!(run("", Some(Range { offset: 0, limit: Some(1) })), 1);
        assert_eq!(run("", Some(Range { offset: 8, limit: None })), 2);

        let stmt = build_read(
            &db.1,
            "novel",
            &parse_query("select=edition(id)").unwrap(),
            None,
            false,
            small,
            &Grant::default(),
        )
        .unwrap()
        .stmt;
        assert!(stmt.params.contains(&Value::Integer(3))); // embedded arrays are capped at max
    }

    // --- T5.1 count ---

    #[test]
    fn page_counts_rows_and_total() {
        let db = novel();
        let read = |raw: &str, range: Option<Range>, count: bool| {
            let read =
                build_read(&db.1, "part", &parse_query(raw).unwrap(), range, count, LIMITS, &Grant::default()).unwrap();
            let page = read.run(&db.0).unwrap();
            (read.offset, page.rows, page.total)
        };
        let all = read("", None, true).2.unwrap();
        assert!(all > 3);
        assert_eq!(read("", None, false), (0, all, None));
        assert_eq!(read("limit=2&offset=1", None, true), (1, 2, Some(all)));
        assert_eq!(read("", Some(Range { offset: 1, limit: Some(2) }), true), (1, 2, Some(all)));
        assert_eq!(read("word_count=gte.100000", None, true), (0, 0, Some(0)));
        assert_eq!(read(&format!("offset={all}"), None, true), (all, 0, Some(all)));

        // `!inner` conditions count too.
        let db = novel();
        let read = build_read(
            &db.1,
            "novel",
            &parse_query("select=title,edition!inner(id)&edition.language=eq.vi&limit=0").unwrap(),
            None,
            true,
            LIMITS,
            &Grant::default(),
        )
        .unwrap();
        let page = read.run(&db.0).unwrap();
        assert_eq!((page.body.as_str(), page.rows, page.total), ("[]", 0, Some(1)));
    }

    // --- M10 / M11 grants and owner rows ---

    const OWNED: &str = "
        CREATE TABLE list (id INTEGER PRIMARY KEY, owner_id TEXT, name TEXT);
        CREATE TABLE todo (id INTEGER PRIMARY KEY, list_id INTEGER REFERENCES list(id), user_id TEXT, title TEXT);
        CREATE TABLE tag (id INTEGER PRIMARY KEY, name TEXT);
        CREATE TABLE secret (id INTEGER PRIMARY KEY);
        CREATE TABLE note (id INTEGER PRIMARY KEY, body TEXT);
        INSERT INTO list VALUES (1, 'alice', 'A1'), (2, 'bob', 'B1');
        INSERT INTO todo VALUES (1, 1, 'alice', 'a'), (2, 1, 'bob', 'b-in-a'), (3, 2, 'bob', 'b');
        INSERT INTO tag VALUES (1, 't');";

    fn policy() -> Arc<Policy> {
        let owner = |c: &str, read_all| Rule { perm: Perm::Write, owner: Some(c.into()), read_all };
        let mut tables = vec![
            ("list".to_string(), owner("owner_id", false)),
            ("note".to_string(), owner("author_id", false)), // no such column: denied
            ("tag".to_string(), Rule { perm: Perm::Read, ..Rule::default() }),
            ("todo".to_string(), owner("user_id", false)),
        ];
        tables.sort_by(|a, b| a.0.cmp(&b.0));
        Arc::new(Policy { tables, default: Rule::default() })
    }

    fn user(uid: &str) -> Grant {
        Grant { policy: Some(policy()), uid: Some(uid.into()) }
    }

    fn read_as(db: &(Connection, Schema), grant: &Grant, table: &str, raw: &str) -> Result<(Json, Option<u64>)> {
        let read = build_read(&db.1, table, &parse_query(raw).unwrap(), None, true, LIMITS, grant)?;
        let page = read.run(&db.0).unwrap_or_else(|e| panic!("{raw}: {e}\n{}", read.stmt.sql));
        Ok((serde_json::from_str(&page.body).unwrap(), page.total))
    }

    #[test]
    fn grants_check_read_on_root_and_embeds() {
        let db = db(OWNED);
        let alice = user("alice");
        let denied = |t: &str| Err(BuildError::PermissionDenied(t.into()));
        assert_eq!(read_as(&db, &alice, "secret", ""), denied("secret"));
        assert_eq!(read_as(&db, &alice, "note", ""), denied("note"));
        assert_eq!(read_as(&db, &alice, "tag", "select=name").unwrap().0, json!([{ "name": "t" }]));
        assert_eq!(
            read_as(&db, &alice, "list", "select=id,secret(id)").map(|_| ()),
            Err(BuildError::RelationshipNotFound { from: "list".into(), to: "secret".into() })
        );

        let anon = Grant {
            policy: Some(Arc::new(Policy {
                tables: vec![("tag".into(), Rule { perm: Perm::Read, ..Rule::default() })],
                default: Rule::default(),
            })),
            uid: None,
        };
        assert!(read_as(&db, &anon, "tag", "").is_ok());
        assert_eq!(read_as(&db, &anon, "list", ""), denied("list"));
        // An owner rule without a uid denies.
        assert_eq!(read_as(&db, &Grant { policy: Some(policy()), uid: None }, "list", ""), denied("list"));

        // An embed of a closed table, also filter-only.
        let db2 = self::db(
            "CREATE TABLE a (id INTEGER PRIMARY KEY); CREATE TABLE b (id INTEGER PRIMARY KEY, a_id INTEGER REFERENCES a(id));",
        );
        let only_a = Grant {
            policy: Some(Arc::new(Policy {
                tables: vec![("a".into(), Rule { perm: Perm::Read, ..Rule::default() })],
                default: Rule::default(),
            })),
            uid: None,
        };
        assert!(read_as(&db2, &only_a, "a", "").is_ok());
        assert_eq!(read_as(&db2, &only_a, "a", "select=*,b(*)"), denied("b"));
        assert_eq!(read_as(&db2, &only_a, "a", "select=*,b!inner()"), denied("b"));
    }

    #[test]
    fn owner_rows_on_reads() {
        let db = db(OWNED);
        let (alice, bob) = (user("alice"), user("bob"));
        let ids = |v: &Json| v.as_array().unwrap().iter().map(|r| r["id"].as_i64().unwrap()).collect::<Vec<_>>();

        let (v, total) = read_as(&db, &alice, "list", "select=id,todo(id)&order=id").unwrap();
        assert_eq!(v, json!([{ "id": 1, "todo": [{ "id": 1 }] }])); // bob's todo in alice's list is hidden
        assert_eq!(total, Some(1));
        let (v, total) = read_as(&db, &bob, "todo", "select=id,list(id)&order=id").unwrap();
        assert_eq!(v, json!([{ "id": 2, "list": null }, { "id": 3, "list": { "id": 2 } }]));
        assert_eq!(total, Some(2));
        let (v, total) = read_as(&db, &bob, "todo", "select=id,list!inner(id)").unwrap();
        assert_eq!((ids(&v), total), (vec![3], Some(1)));

        // owner_scope "write": every row is readable.
        let mut p = (*policy()).clone();
        p.tables.iter_mut().find(|(t, _)| t == "todo").unwrap().1.read_all = true;
        let read_all = Grant { policy: Some(Arc::new(p)), uid: Some("alice".into()) };
        assert_eq!(read_as(&db, &read_all, "todo", "order=id").unwrap().1, Some(3));

        // Service role / auth off: no checks.
        assert_eq!(read_as(&db, &Grant::default(), "todo", "").unwrap().1, Some(3));
    }

    #[test]
    fn uid_is_bound_and_sql_is_per_shape() {
        let db = db(OWNED);
        let q = parse_query("select=id,todo(id)").unwrap();
        let a = build_read(&db.1, "list", &q, None, false, LIMITS, &user("alice'--")).unwrap().stmt;
        let b = build_read(&db.1, "list", &q, None, false, LIMITS, &user("bob")).unwrap().stmt;
        assert_eq!(a.sql, b.sql);
        assert!(!a.sql.contains("alice"));
        assert_eq!(a.params.iter().filter(|p| **p == Value::Text("alice'--".into())).count(), 1); // bound once, used twice
        assert_eq!(a.sql.matches("\"owner_id\" = ?1").count() + a.sql.matches("\"user_id\" = ?1").count(), 2);
    }

    // --- T3.6 statement cache ---

    #[test]
    fn same_shape_same_sql() {
        let db = novel();
        let a = build_read(
            &db.1,
            "novel",
            &parse_query("select=title&title=eq.a&limit=1").unwrap(),
            None,
            false,
            LIMITS,
            &Grant::default(),
        )
        .unwrap()
        .stmt;
        let b = build_read(
            &db.1,
            "novel",
            &parse_query("select=title&title=eq.b&limit=7").unwrap(),
            None,
            false,
            LIMITS,
            &Grant::default(),
        )
        .unwrap()
        .stmt;
        assert_eq!(a.sql, b.sql);
        assert_ne!(a.params, b.params);
    }
}
