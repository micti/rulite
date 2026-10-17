//! SQL builder (write): INSERT, upsert, UPDATE and DELETE, each run as one
//! transaction on the writer thread.
//!
//! The JSON body is never turned into Rust values: it is scanned once for its
//! keys (to validate columns), then bound as a single parameter that SQLite
//! reads with `json_each` / `->>`. A bulk insert is one statement whatever
//! the row count, so it is cached like any other shape.
//!
//! `return=representation` re-reads the written rows by key with the read
//! builder, so `select` (embeds included) works on writes too.
//!
//! On an owner table the uid fills a missing owner column, UPDATE / DELETE
//! only touch own rows, and the written rows are checked before commit.

use std::fmt::{self, Write as _};

use rusqlite::types::Value;
use rusqlite::{Connection, TransactionBehavior};
use serde::de::{self, DeserializeSeed, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};

use crate::query::{Prefer, Query, Resolution, Return};
use crate::schema::{Affinity, Column, Schema, Table};
use crate::sql::{
    self, BuildError, Builder, Grant, Limits, Perm, Rows, Statement, ident, literal, push_where, row_key,
};

type Result<T> = std::result::Result<T, BuildError>;

/// A write body, scanned but not parsed into values.
#[derive(Debug)]
pub struct Payload {
    /// The body as a JSON array of objects (a single object gets wrapped).
    json: String,
    /// Every key seen, in order of first appearance.
    keys: Vec<String>,
    rows: usize,
    /// The body was a bare object rather than an array.
    object: bool,
}

#[derive(Debug)]
pub enum Action {
    Insert(Payload),
    Update(Payload),
    Delete,
}

/// One write request, owned so it can move to the writer thread.
#[derive(Debug)]
pub struct Write {
    pub table: String,
    pub query: Query,
    pub prefer: Prefer,
    /// `Accept: application/vnd.pgrst.object+json`: with a body, exactly one row.
    pub single: bool,
    pub action: Action,
}

/// What a write answers with: the rows as JSON when `return=representation`.
#[derive(Debug)]
pub struct Output {
    pub body: Option<String>,
    /// Rows written (inserted, updated or deleted).
    pub rows: u64,
}

/// Scans `body`: an object, or an array of objects.
pub fn parse_payload(body: &str) -> Result<Payload> {
    let invalid = |e: serde_json::Error| BuildError::InvalidBody(e.to_string());
    let mut de = serde_json::Deserializer::from_str(body);
    let mut keys = Vec::new();
    let (rows, object) = de.deserialize_any(Body(&mut keys)).map_err(invalid)?;
    de.end().map_err(invalid)?;
    let json = if object { format!("[{body}]") } else { body.to_string() };
    Ok(Payload { json, keys, rows, object })
}

/// Top level: `{...}` → `(1, true)`, `[{...}, ...]` → `(n, false)`.
struct Body<'k>(&'k mut Vec<String>);

impl<'de> Visitor<'de> for Body<'_> {
    type Value = (usize, bool);

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("an object or an array of objects")
    }

    fn visit_map<A: MapAccess<'de>>(self, map: A) -> std::result::Result<Self::Value, A::Error> {
        Row(self.0).visit_map(map)?;
        Ok((1, true))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> std::result::Result<Self::Value, A::Error> {
        let mut rows = 0;
        while seq.next_element_seed(Row(self.0))?.is_some() {
            rows += 1;
        }
        Ok((rows, false))
    }
}

/// One object: records new keys, skips values.
struct Row<'k>(&'k mut Vec<String>);

impl<'de> DeserializeSeed<'de> for Row<'_> {
    type Value = ();

    fn deserialize<D: Deserializer<'de>>(self, d: D) -> std::result::Result<(), D::Error> {
        d.deserialize_map(self)
    }
}

impl<'de> Visitor<'de> for Row<'_> {
    type Value = ();

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("an object")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> std::result::Result<(), A::Error> {
        while map.next_key_seed(Key(&mut *self.0))?.is_some() {
            map.next_value::<IgnoredAny>()?;
        }
        Ok(())
    }
}

/// A key, compared without allocating; only new keys are copied.
struct Key<'k>(&'k mut Vec<String>);

impl<'de> DeserializeSeed<'de> for Key<'_> {
    type Value = ();

    fn deserialize<D: Deserializer<'de>>(self, d: D) -> std::result::Result<(), D::Error> {
        d.deserialize_str(self)
    }
}

impl<'de> Visitor<'de> for Key<'_> {
    type Value = ();

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a key")
    }

    fn visit_str<E: de::Error>(self, k: &str) -> std::result::Result<(), E> {
        if !self.0.iter().any(|seen| seen == k) {
            self.0.push(k.to_string());
        }
        Ok(())
    }
}

/// Runs `w` in one `IMMEDIATE` transaction. Build errors roll it back and
/// come back as `Ok(Err(_))`; SQLite errors as `Err(_)`. A single-object
/// request that wrote other than one row is rolled back too.
pub fn execute(
    conn: &mut Connection,
    schema: &Schema,
    w: &Write,
    limits: Limits,
    grant: &Grant,
) -> rusqlite::Result<Result<Output>> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let out = match run(&tx, schema, w, limits, grant) {
        Ok(out) => out,
        Err(Error::Build(e)) => return Ok(Err(e)),
        Err(Error::Sqlite(e)) => return Err(e),
    };
    if w.single && out.body.is_some() && out.rows != 1 {
        return Ok(Err(BuildError::NotSingular(out.rows)));
    }
    tx.commit()?;
    Ok(Ok(out))
}

enum Error {
    Build(BuildError),
    Sqlite(rusqlite::Error),
}

impl From<BuildError> for Error {
    fn from(e: BuildError) -> Error {
        Error::Build(e)
    }
}

impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Error {
        Error::Sqlite(e)
    }
}

fn run(
    conn: &Connection,
    schema: &Schema,
    w: &Write,
    limits: Limits,
    grant: &Grant,
) -> std::result::Result<Output, Error> {
    let table = sql::find_table(schema, &w.table)?;
    let owner = grant.owner(table, Perm::Write)?;
    let represent = w.prefer.return_ == Some(Return::Representation);
    // Owner tables always return keys: the written rows are checked.
    let keyed = represent || owner.is_some();
    let stmt = match &w.action {
        Action::Insert(p) if p.rows == 0 => return Ok(Output { body: represent.then(|| "[]".to_string()), rows: 0 }),
        Action::Insert(p) => build_insert(table, &w.query, p, &w.prefer, keyed, owner.map(|c| (c, grant)))?,
        Action::Update(p) => match build_update(schema, table, &w.query, p, keyed, limits, grant)? {
            Some(stmt) => stmt,
            None => return Ok(Output { body: represent.then(|| "[]".to_string()), rows: 0 }),
        },
        Action::Delete => {
            let stmt = build_delete(schema, table, &w.query, limits, grant)?;
            // Read what is about to go, then delete it.
            let body = match represent {
                true => Some(sql::build_select(schema, &table.name, &w.query, Rows::All, limits, grant)?.run(conn)?),
                false => None,
            };
            let rows = stmt.execute(conn)? as u64;
            return Ok(Output { body, rows });
        }
    };
    if !keyed {
        let rows = stmt.execute(conn)? as u64;
        return Ok(Output { body: None, rows });
    }
    let (n, keys) = stmt.returning(conn)?;
    if let Some(owner) = owner {
        // A merge that skipped a row hit another user's row.
        let merge = w.prefer.resolution == Some(Resolution::MergeDuplicates);
        let skipped = matches!(&w.action, Action::Insert(p) if merge && n < p.rows);
        if skipped || (n > 0 && foreign_owner(conn, table, owner, &keys, grant)?) {
            return Err(BuildError::RowPolicy(table.name.clone()).into());
        }
    }
    if !represent {
        return Ok(Output { body: None, rows: n as u64 });
    }
    if n == 0 {
        return Ok(Output { body: Some("[]".to_string()), rows: 0 });
    }
    let body = sql::build_select(schema, &table.name, &w.query, Rows::Keys(keys), limits, grant)?.run(conn)?;
    Ok(Output { body: Some(body), rows: n as u64 })
}

/// Whether any row in `keys` (what a write returned) is not owned by the uid.
fn foreign_owner(
    conn: &Connection,
    table: &Table,
    owner: &Column,
    keys: &str,
    grant: &Grant,
) -> rusqlite::Result<bool> {
    let key = row_key(table);
    let values: Vec<String> = (0..key.len()).map(|i| format!("value ->> {i}")).collect();
    let sql = format!(
        "SELECT EXISTS (SELECT 1 FROM {} WHERE ({}) IN (SELECT {} FROM json_each(?1)) AND {} IS NOT ?2)",
        ident(&table.name),
        key.join(", "),
        values.join(", "),
        ident(&owner.name),
    );
    conn.prepare_cached(&sql)?.query_row((keys, grant.uid.as_deref()), |r| r.get(0))
}

/// `INSERT INTO t (cols) SELECT ... FROM json_each(?1) WHERE 1 [ON CONFLICT ...]`.
///
/// Columns come from `columns=` when given (other keys are ignored, as in
/// PostgREST), else from the body's keys. A key missing from a row is NULL,
/// or the column's default with `Prefer: missing=default`. A missing owner
/// is set to the uid (`?2`); a merge only updates own rows.
fn build_insert(
    table: &Table,
    query: &Query,
    p: &Payload,
    prefer: &Prefer,
    represent: bool,
    owner: Option<(&Column, &Grant)>,
) -> Result<Statement> {
    let names = query.columns.as_ref().unwrap_or(&p.keys);
    let mut cols = names.iter().map(|n| payload_column(table, n)).collect::<Result<Vec<_>>>()?;
    let mut params = vec![Value::Text(p.json.clone())];
    if let Some((c, grant)) = owner {
        if !cols.iter().any(|x| x.name == c.name) {
            cols.push(c);
        }
        params.push(Value::Text(grant.uid.clone().unwrap_or_default()));
    }
    // `{}`: insert one column's default, which SQLite treats as DEFAULT VALUES.
    let all_default = cols.is_empty();
    if all_default {
        cols.push(
            table
                .columns
                .first()
                .ok_or_else(|| BuildError::Unsupported(format!("table '{}' has no columns", table.name)))?,
        );
    }

    let values: Vec<String> = cols
        .iter()
        .map(|c| match &c.default {
            _ if owner.is_some_and(|(o, _)| o.name == c.name) => format!("coalesce({}, ?2)", value("value", c)),
            _ if all_default => c.default.as_ref().map_or("NULL".to_string(), |d| format!("({d})")),
            Some(d) if prefer.missing_default => {
                format!("CASE WHEN json_type(value, {}) IS NULL THEN ({d}) ELSE {} END", path(c), value("value", c))
            }
            _ => value("value", c),
        })
        .collect();
    let names: Vec<String> = cols.iter().map(|c| ident(&c.name)).collect();
    let mut sql = format!(
        "INSERT INTO {} ({}) SELECT {} FROM json_each(?1) WHERE 1",
        ident(&table.name),
        names.join(", "),
        values.join(", ")
    );

    if let Some(resolution) = prefer.resolution.filter(|_| !all_default) {
        let target = conflict_target(table, query.on_conflict.as_deref())?;
        let quoted: Vec<String> = target.iter().map(|c| ident(c)).collect();
        write!(sql, " ON CONFLICT ({}) DO ", quoted.join(", ")).unwrap();
        if resolution == Resolution::IgnoreDuplicates {
            sql.push_str("NOTHING");
        } else {
            let mut set: Vec<String> =
                names.iter().filter(|n| !quoted.contains(n)).map(|n| format!("{n} = excluded.{n}")).collect();
            if set.is_empty() {
                // Only key columns: a no-op update still returns the row.
                set = quoted.iter().map(|n| format!("{n} = excluded.{n}")).collect();
            }
            write!(sql, "UPDATE SET {}", set.join(", ")).unwrap();
            if let Some((c, _)) = owner {
                write!(sql, " WHERE {}.{} = ?2", ident(&table.name), ident(&c.name)).unwrap();
            }
        }
    }
    if represent {
        returning(&mut sql, table);
    }
    Ok(Statement { sql, params })
}

/// `UPDATE t AS t0 SET col = ?1 ->> '$."col"', ... WHERE <filters>`, or
/// `None` for an empty body (nothing to change).
fn build_update(
    schema: &Schema,
    table: &Table,
    query: &Query,
    p: &Payload,
    represent: bool,
    limits: Limits,
    grant: &Grant,
) -> Result<Option<Statement>> {
    if !p.object {
        return Err(BuildError::InvalidBody("PATCH takes a single object".into()));
    }
    let mut b = Builder::new(schema, query, limits, grant);
    let alias = b.alias();
    let conds = write_filters(&mut b, table, &alias, query, "UPDATE")?;
    if p.keys.is_empty() {
        return Ok(None);
    }
    let body = b.param(Value::Text(p.json[1..p.json.len() - 1].to_string()));
    let set = p
        .keys
        .iter()
        .map(|k| payload_column(table, k).map(|c| format!("{} = {}", ident(&c.name), value(&body, c))))
        .collect::<Result<Vec<_>>>()?;

    let mut sql = format!("UPDATE {} AS {alias} SET {}", ident(&table.name), set.join(", "));
    push_where(&mut sql, &conds);
    if represent {
        returning(&mut sql, table);
    }
    Ok(Some(Statement { sql, params: b.into_params() }))
}

/// `DELETE FROM t AS t0 WHERE <filters>`.
fn build_delete(schema: &Schema, table: &Table, query: &Query, limits: Limits, grant: &Grant) -> Result<Statement> {
    let mut b = Builder::new(schema, query, limits, grant);
    let alias = b.alias();
    let conds = write_filters(&mut b, table, &alias, query, "DELETE")?;
    let mut sql = format!("DELETE FROM {} AS {alias}", ident(&table.name));
    push_where(&mut sql, &conds);
    Ok(Statement { sql, params: b.into_params() })
}

/// Root filters of an UPDATE / DELETE, plus the owner condition; refuses no
/// filter (like Supabase's safeupdate) and `limit` / `offset`, which would
/// silently widen the write.
fn write_filters(
    b: &mut Builder,
    table: &Table,
    alias: &str,
    query: &Query,
    verb: &'static str,
) -> Result<Vec<String>> {
    let root = query.root();
    if root.limit.is_some() || root.offset.is_some() {
        return Err(BuildError::Unsupported(format!("limit and offset are not supported on {verb}")));
    }
    if root.filters.is_empty() {
        return Err(BuildError::MissingFilter(verb));
    }
    let mut conds: Vec<String> = b.access(table, alias, Perm::Write)?.into_iter().collect();
    conds.extend(b.filters(table, alias, &root.filters)?);
    Ok(conds)
}

/// The unique key `on_conflict` names (any column order), else the primary key.
fn conflict_target<'t>(table: &'t Table, on_conflict: Option<&[String]>) -> Result<&'t [String]> {
    let wanted = on_conflict.unwrap_or(&table.primary_key);
    for name in wanted {
        sql::column(table, name)?;
    }
    table
        .uniques
        .iter()
        .find(|u| !wanted.is_empty() && u.len() == wanted.len() && wanted.iter().all(|w| u.contains(w)))
        .map(Vec::as_slice)
        .ok_or_else(|| BuildError::NoUniqueKey { table: table.name.clone(), columns: wanted.to_vec() })
}

fn payload_column<'t>(table: &'t Table, name: &str) -> Result<&'t Column> {
    table.column(name).ok_or_else(|| BuildError::PayloadColumn { table: table.name.clone(), column: name.to_string() })
}

/// `'$."col"'` as a SQL literal: the JSON path of a column's key.
fn path(c: &Column) -> String {
    literal(&format!("$.\"{}\"", c.name.replace('\\', "\\\\").replace('"', "\\\"")))
}

/// A column's value read from the JSON object `src`. Scalars map directly,
/// objects and arrays stay JSON text, and a `\x...` string becomes a BLOB in
/// BLOB columns (the inverse of how reads render them).
fn value(src: &str, c: &Column) -> String {
    let v = format!("{src} ->> {}", path(c));
    if c.affinity == Affinity::Blob {
        format!("CASE WHEN {v} LIKE '\\x%' THEN coalesce(unhex(substr({v}, 3)), {v}) ELSE {v} END")
    } else {
        v
    }
}

/// ` RETURNING json_array(<row key>)`, collected by [`Statement::returning`].
fn returning(sql: &mut String, table: &Table) {
    write!(sql, " RETURNING json_array({})", row_key(table).join(", ")).unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::{parse_prefer, parse_query};
    use crate::schema::introspect;
    use crate::sql::{Policy, Rule};
    use serde_json::{Value as Json, json};
    use std::sync::Arc;

    const LIMITS: Limits = Limits { default: 100, max: 1000 };

    fn db(sql: &str) -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        conn.execute_batch(sql).unwrap();
        conn
    }

    fn novel() -> Connection {
        db(include_str!("../sample/novel.sql"))
    }

    fn event() -> Connection {
        db(include_str!("../sample/event.sql"))
    }

    /// Runs one write; `Ok(None)` for `return=minimal`.
    fn send(conn: &mut Connection, table: &str, raw: &str, prefer: &str, action: Action) -> Result<Option<Json>> {
        let schema = introspect(conn).unwrap();
        let w = Write {
            table: table.into(),
            query: parse_query(raw).unwrap(),
            prefer: parse_prefer([prefer]),
            single: false,
            action,
        };
        let out =
            execute(conn, &schema, &w, LIMITS, &Grant::default()).unwrap_or_else(|e| panic!("{table}?{raw}: {e}"))?;
        Ok(out.body.map(|b| serde_json::from_str(&b).unwrap()))
    }

    fn insert(conn: &mut Connection, table: &str, raw: &str, prefer: &str, body: Json) -> Result<Option<Json>> {
        send(conn, table, raw, prefer, Action::Insert(parse_payload(&body.to_string())?))
    }

    fn update(conn: &mut Connection, table: &str, raw: &str, prefer: &str, body: Json) -> Result<Option<Json>> {
        send(conn, table, raw, prefer, Action::Update(parse_payload(&body.to_string())?))
    }

    fn delete(conn: &mut Connection, table: &str, raw: &str, prefer: &str) -> Result<Option<Json>> {
        send(conn, table, raw, prefer, Action::Delete)
    }

    fn count(conn: &Connection, table: &str) -> i64 {
        conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0)).unwrap()
    }

    const REP: &str = "return=representation";

    // --- Done when (PLAN.md M4) ---

    #[test]
    fn create_chain_then_cascade_delete() {
        let mut c = novel();
        let before = ["novel", "edition", "volume", "chapter", "part"].map(|t| count(&c, t));

        let n = insert(&mut c, "novel", "select=id", REP, json!({ "title": "Tide Glass", "author": "Ana Ruiz" }))
            .unwrap()
            .unwrap();
        let novel_id = n[0]["id"].as_str().unwrap().to_string();
        let e = insert(
            &mut c,
            "edition",
            "select=id",
            REP,
            json!({ "novel_id": novel_id, "name": "First", "language": "en" }),
        )
        .unwrap()
        .unwrap();
        let v = insert(
            &mut c,
            "volume",
            "select=id",
            REP,
            json!({ "edition_id": e[0]["id"], "number": 1, "title": "Shore" }),
        )
        .unwrap()
        .unwrap();
        let ch = insert(
            &mut c,
            "chapter",
            "select=id",
            REP,
            json!({ "volume_id": v[0]["id"], "number": 1, "title": "Low Tide" }),
        )
        .unwrap()
        .unwrap();
        let parts = json!([
            { "chapter_id": ch[0]["id"], "number": 1, "content": "Glass in the sand.", "word_count": 4 },
            { "chapter_id": ch[0]["id"], "number": 2, "content": "It hummed.", "word_count": 2 },
        ]);
        assert_eq!(insert(&mut c, "part", "", "", parts).unwrap(), None);

        let tree = insert(
            &mut c,
            "novel",
            "select=title,status,edition(language,volume(title,chapter(title,part(content))))&edition.volume.chapter.part.order=number\
             &on_conflict=title",
            &format!("{REP},resolution=merge-duplicates"),
            json!({ "title": "Tide Glass", "author": "Ana Ruiz", "status": "completed" }),
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            tree,
            json!([{
                "title": "Tide Glass", "status": "completed",
                "edition": [{ "language": "en", "volume": [{ "title": "Shore", "chapter": [{
                    "title": "Low Tide", "part": [{ "content": "Glass in the sand." }, { "content": "It hummed." }],
                }]}]}],
            }])
        );

        let gone = delete(&mut c, "novel", &format!("select=title&id=eq.{novel_id}"), REP).unwrap().unwrap();
        assert_eq!(gone, json!([{ "title": "Tide Glass" }]));
        assert_eq!(["novel", "edition", "volume", "chapter", "part"].map(|t| count(&c, t)), before);
    }

    // --- T4.1 insert ---

    #[test]
    fn insert_defaults_and_bulk() {
        let mut c = novel();
        let v = insert(&mut c, "novel", "select=title,status,genre", REP, json!({ "title": "A", "author": "x" }))
            .unwrap()
            .unwrap();
        assert_eq!(v, json!([{ "title": "A", "status": "ongoing", "genre": null }])); // single object: absent keys take defaults

        // Bulk: union of keys, absent → NULL...
        let rows = json!([{ "title": "B", "author": "x", "genre": "g" }, { "title": "C", "author": "y" }]);
        let v = insert(&mut c, "novel", "select=title,genre&order=title", REP, rows).unwrap().unwrap();
        assert_eq!(v, json!([{ "title": "B", "genre": "g" }, { "title": "C", "genre": null }]));

        // ...or the column default with missing=default.
        let rows = json!([{ "title": "D", "author": "x", "status": "hiatus" }, { "title": "E", "author": "y" }]);
        let v = insert(&mut c, "novel", "select=title,status&order=title", &format!("{REP},missing=default"), rows)
            .unwrap()
            .unwrap();
        assert_eq!(v, json!([{ "title": "D", "status": "hiatus" }, { "title": "E", "status": "ongoing" }]));

        // `columns=` limits what is inserted.
        let v = insert(
            &mut c,
            "novel",
            "columns=title,author&select=title,genre",
            REP,
            json!([{ "title": "F", "author": "z", "genre": "skip" }]),
        )
        .unwrap()
        .unwrap();
        assert_eq!(v, json!([{ "title": "F", "genre": null }]));

        assert_eq!(insert(&mut c, "novel", "", REP, json!([])).unwrap(), Some(json!([])));
        assert_eq!(count(&c, "novel"), 3 + 6);
    }

    #[test]
    fn insert_json_bool_blob_and_default_values() {
        let mut c = event();
        let v = insert(&mut c, "artist", "select=name,genres", REP, json!({ "name": "New", "genres": ["a", "b"] }))
            .unwrap()
            .unwrap();
        assert_eq!(v, json!([{ "name": "New", "genres": ["a", "b"] }]));
        let area: String = c.query_row("SELECT id FROM area LIMIT 1", [], |r| r.get(0)).unwrap();
        let v = insert(
            &mut c,
            "event",
            "select=is_public",
            REP,
            json!({ "area_id": area, "name": "E", "starts_on": "2026-01-01", "ends_on": "2026-01-02", "is_public": false }),
        )
        .unwrap()
        .unwrap();
        assert_eq!(v, json!([{ "is_public": 0 }]));

        let mut c = db("CREATE TABLE doc (id INTEGER PRIMARY KEY, data BLOB, note TEXT DEFAULT 'n');");
        let v = insert(&mut c, "doc", "", REP, json!([{ "data": "\\x00ff" }, { "data": "plain" }])).unwrap().unwrap();
        assert_eq!(v, json!([{ "id": 1, "data": "\\x00ff", "note": "n" }, { "id": 2, "data": "plain", "note": "n" }]));
        let kind: String = c.query_row("SELECT typeof(data) FROM doc WHERE id = 1", [], |r| r.get(0)).unwrap();
        assert_eq!(kind, "blob");
        let v = insert(&mut c, "doc", "", REP, json!({})).unwrap().unwrap();
        assert_eq!(v, json!([{ "id": 3, "data": null, "note": "n" }]));
    }

    #[test]
    fn insert_errors_roll_back() {
        let mut c = novel();
        let err = insert(&mut c, "novel", "", "", json!({ "title": "A", "nope": 1 })).unwrap_err();
        assert_eq!(err, BuildError::PayloadColumn { table: "novel".into(), column: "nope".into() });
        assert!(matches!(parse_payload("[1]"), Err(BuildError::InvalidBody(_))));
        assert!(matches!(parse_payload("{\"a\":1} x"), Err(BuildError::InvalidBody(_))));
        assert!(matches!(parse_payload(""), Err(BuildError::InvalidBody(_))));

        // Bad `select` after the write: the insert is rolled back.
        let err = insert(&mut c, "novel", "select=nope", REP, json!({ "title": "A", "author": "x" })).unwrap_err();
        assert!(matches!(err, BuildError::ColumnNotFound { .. }));
        assert_eq!(count(&c, "novel"), 3);

        // Constraint failure: the whole batch is rolled back.
        let schema = introspect(&c).unwrap();
        let rows = json!([{ "title": "Z", "author": "x" }, { "title": "Z", "author": "y" }]);
        let w = Write {
            table: "novel".into(),
            query: parse_query("").unwrap(),
            prefer: Prefer::default(),
            single: false,
            action: Action::Insert(parse_payload(&rows.to_string()).unwrap()),
        };
        assert!(execute(&mut c, &schema, &w, LIMITS, &Grant::default()).is_err());
        assert_eq!(count(&c, "novel"), 3);
    }

    #[test]
    fn payload_keys_are_scanned_once() {
        let p = parse_payload(r#"[{"a":1,"b":{"x":[1]}},{"b":2,"c\"d":null}]"#).unwrap();
        assert_eq!(p.keys, ["a", "b", "c\"d"]);
        assert_eq!((p.rows, p.object), (2, false));
        let p = parse_payload(r#"{"a":1}"#).unwrap();
        assert_eq!((p.json.as_str(), p.rows, p.object), (r#"[{"a":1}]"#, 1, true));
    }

    // --- T4.2 upsert ---

    #[test]
    fn upsert_merge_and_ignore() {
        let mut c = novel();
        let rows = json!([
            { "title": "Signal Drift", "author": "New Author" },
            { "title": "Brand New", "author": "B" },
        ]);
        let v = insert(
            &mut c,
            "novel",
            "on_conflict=title&select=title,author&order=title",
            &format!("{REP},resolution=merge-duplicates"),
            rows.clone(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            v,
            json!([{ "title": "Brand New", "author": "B" }, { "title": "Signal Drift", "author": "New Author" }])
        );
        assert_eq!(count(&c, "novel"), 4);

        let rows = json!([{ "title": "Signal Drift", "author": "Ignored" }, { "title": "Newer", "author": "N" }]);
        let v = insert(
            &mut c,
            "novel",
            "on_conflict=title&select=title,author",
            &format!("{REP},resolution=ignore-duplicates"),
            rows,
        )
        .unwrap()
        .unwrap();
        assert_eq!(v, json!([{ "title": "Newer", "author": "N" }])); // only inserted rows come back
        let author: String =
            c.query_row("SELECT author FROM novel WHERE title = 'Signal Drift'", [], |r| r.get(0)).unwrap();
        assert_eq!(author, "New Author");

        // Composite key, any column order; default target is the primary key.
        let mut c = event();
        let (se, ar): (String, String) = c
            .query_row("SELECT sub_event_id, artist_id FROM lineup LIMIT 1", [], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        let row = json!({ "sub_event_id": se, "artist_id": ar, "role": "host" });
        let v = insert(
            &mut c,
            "lineup",
            "on_conflict=artist_id,sub_event_id&select=role",
            &format!("{REP},resolution=merge-duplicates"),
            row.clone(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(v, json!([{ "role": "host" }]));
        let v = insert(
            &mut c,
            "lineup",
            "select=role",
            &format!("{REP},resolution=merge-duplicates"),
            json!({ "sub_event_id": se, "artist_id": ar, "role": "guest" }),
        )
        .unwrap()
        .unwrap();
        assert_eq!(v, json!([{ "role": "guest" }]));

        let err =
            insert(&mut c, "artist", "on_conflict=country", "resolution=merge-duplicates", json!({ "name": "x" }))
                .unwrap_err();
        assert!(matches!(err, BuildError::NoUniqueKey { .. }));
    }

    // --- T4.3 update ---

    #[test]
    fn update_with_filters() {
        let mut c = novel();
        let v = update(&mut c, "novel", "status=eq.ongoing&select=title,status", REP, json!({ "status": "hiatus" }))
            .unwrap()
            .unwrap();
        assert_eq!(v, json!([{ "title": "Signal Drift", "status": "hiatus" }])); // rows no longer match the filter, still returned
        assert_eq!(update(&mut c, "novel", "title=eq.nope", "", json!({ "genre": "x" })).unwrap(), None);
        assert_eq!(update(&mut c, "novel", "title=eq.nope", REP, json!({ "genre": "x" })).unwrap(), Some(json!([])));

        let v = update(
            &mut c,
            "novel",
            "or=(genre.eq.fantasy,genre.eq.mystery)&select=title&order=title",
            REP,
            json!({ "genre": null }),
        )
        .unwrap()
        .unwrap();
        assert_eq!(v, json!([{ "title": "Quiet Harbor" }, { "title": "The Lantern Road" }]));
        assert_eq!(
            c.query_row("SELECT count(*) FROM novel WHERE genre IS NULL", [], |r| r.get::<_, i64>(0)).unwrap(),
            2
        );
    }

    #[test]
    fn update_refusals() {
        let mut c = novel();
        assert_eq!(
            update(&mut c, "novel", "", "", json!({ "genre": "x" })).unwrap_err(),
            BuildError::MissingFilter("UPDATE")
        );
        assert!(matches!(
            update(&mut c, "novel", "title=eq.A&limit=1", "", json!({ "genre": "x" })),
            Err(BuildError::Unsupported(_))
        ));
        assert!(matches!(
            update(&mut c, "novel", "title=eq.A", "", json!([{ "genre": "x" }])),
            Err(BuildError::InvalidBody(_))
        ));
        assert!(matches!(
            update(&mut c, "novel", "title=eq.A", "", json!({ "nope": 1 })),
            Err(BuildError::PayloadColumn { .. })
        ));
        assert!(matches!(
            update(&mut c, "novel", "nope=eq.A", "", json!({ "genre": 1 })),
            Err(BuildError::ColumnNotFound { .. })
        ));
    }

    // --- T4.4 delete ---

    #[test]
    fn delete_with_filters() {
        let mut c = event();
        assert_eq!(delete(&mut c, "area", "", "").unwrap_err(), BuildError::MissingFilter("DELETE"));
        let lineups = count(&c, "lineup");
        assert_eq!(delete(&mut c, "artist", "name=eq.DJ%20Nova", "").unwrap(), None);
        assert!(count(&c, "lineup") < lineups); // cascaded

        // ON DELETE SET NULL: sub events on the removed stage keep existing.
        let v = delete(&mut c, "area", "name=eq.Garden%20Stage&select=name,sub_event(title)", REP).unwrap().unwrap();
        assert_eq!(v, json!([{ "name": "Garden Stage", "sub_event": [{ "title": "Acoustic Sunset" }] }]));
        let area: Option<String> =
            c.query_row("SELECT area_id FROM sub_event WHERE title = 'Acoustic Sunset'", [], |r| r.get(0)).unwrap();
        assert_eq!(area, None);
    }

    // --- T5.1 / T5.2 rows and single object ---

    #[test]
    fn rows_written_and_single_object() {
        let mut c = novel();
        let schema = introspect(&c).unwrap();
        let run = |c: &mut Connection, raw: &str, prefer: &str, single: bool, action: Action| {
            let w = Write {
                table: "novel".into(),
                query: parse_query(raw).unwrap(),
                prefer: parse_prefer([prefer]),
                single,
                action,
            };
            execute(c, &schema, &w, LIMITS, &Grant::default()).unwrap()
        };
        let body = |v: Json| Action::Insert(parse_payload(&v.to_string()).unwrap());
        let out =
            run(&mut c, "", "", false, body(json!([{ "title": "A", "author": "x" }, { "title": "B", "author": "x" }])))
                .unwrap();
        assert_eq!((out.body, out.rows), (None, 2));
        let patch = || Action::Update(parse_payload(r#"{"genre":"g"}"#).unwrap());
        assert_eq!(run(&mut c, "author=eq.x", "", true, patch()).unwrap().rows, 2); // no body: single is not checked

        // Two rows with a single object requested: refused and rolled back.
        assert_eq!(
            run(&mut c, "author=eq.x&select=title", REP, true, patch()).unwrap_err(),
            BuildError::NotSingular(2)
        );
        let err = run(&mut c, "author=eq.x", REP, true, Action::Delete).unwrap_err();
        assert_eq!(err, BuildError::NotSingular(2));
        assert_eq!(count(&c, "novel"), 5);

        let out = run(&mut c, "title=eq.A&select=title", REP, true, Action::Delete).unwrap();
        assert_eq!((out.body.as_deref(), out.rows), (Some(r#"[{"title":"A"}]"#), 1));
        assert_eq!(count(&c, "novel"), 4);
    }

    // --- M10 / M11 write permissions and owner rows ---

    const OWNED: &str = "
        CREATE TABLE todo (id INTEGER PRIMARY KEY, user_id TEXT, title TEXT UNIQUE);
        CREATE TABLE tag (id INTEGER PRIMARY KEY, name TEXT);
        CREATE TABLE comment (id INTEGER PRIMARY KEY, author_id TEXT, body TEXT);
        INSERT INTO todo VALUES (1, 'alice', 'a'), (2, 'bob', 'b');
        INSERT INTO comment VALUES (1, 'alice', 'hi'), (2, 'bob', 'yo');";

    fn user(uid: &str) -> Grant {
        let owner = |c: &str, read_all| Rule { perm: Perm::Write, owner: Some(c.into()), read_all };
        let tables = vec![
            ("comment".to_string(), owner("author_id", true)),
            ("tag".to_string(), Rule { perm: Perm::Read, ..Rule::default() }),
            ("todo".to_string(), owner("user_id", false)),
        ];
        Grant { policy: Some(Arc::new(Policy { tables, default: Rule::default() })), uid: Some(uid.into()) }
    }

    /// Runs one write as `grant`; `Ok(None)` for `return=minimal`.
    fn send_as(
        c: &mut Connection,
        grant: &Grant,
        table: &str,
        raw: &str,
        prefer: &str,
        action: Action,
    ) -> Result<Option<Json>> {
        let schema = introspect(c).unwrap();
        let w = Write {
            table: table.into(),
            query: parse_query(raw).unwrap(),
            prefer: parse_prefer([prefer]),
            single: false,
            action,
        };
        let out = execute(c, &schema, &w, LIMITS, grant).unwrap()?;
        Ok(out.body.map(|b| serde_json::from_str(&b).unwrap()))
    }

    fn body(v: Json) -> Payload {
        parse_payload(&v.to_string()).unwrap()
    }

    fn owners(c: &Connection) -> Vec<(i64, String, String)> {
        let mut stmt = c.prepare("SELECT id, user_id, title FROM todo ORDER BY id").unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get::<_, Option<String>>(2)?.unwrap_or_default())))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    }

    #[test]
    fn write_needs_write() {
        let mut c = db(OWNED);
        let alice = user("alice");
        let denied = |t: &str| Err(BuildError::PermissionDenied(t.into()));
        assert_eq!(send_as(&mut c, &alice, "tag", "", "", Action::Insert(body(json!({ "name": "x" })))), denied("tag"));
        assert_eq!(send_as(&mut c, &alice, "tag", "id=eq.1", "", Action::Delete), denied("tag"));
        assert_eq!(send_as(&mut c, &alice, "tag", "", "", Action::Update(body(json!({ "name": "x" })))), denied("tag"));
        assert_eq!(count(&c, "tag"), 0);
    }

    #[test]
    fn insert_fills_and_checks_the_owner() {
        let mut c = db(OWNED);
        let alice = user("alice");
        let v = send_as(
            &mut c,
            &alice,
            "todo",
            "select=user_id,title",
            REP,
            Action::Insert(body(json!({ "title": "mine" }))),
        )
        .unwrap();
        assert_eq!(v, Some(json!([{ "user_id": "alice", "title": "mine" }])));
        let v = send_as(
            &mut c,
            &alice,
            "todo",
            "",
            "",
            Action::Insert(body(json!([{ "title": "x", "user_id": "alice" }]))),
        )
        .unwrap();
        assert_eq!(v, None);
        let v = send_as(&mut c, &alice, "todo", "", REP, Action::Insert(body(json!({})))).unwrap().unwrap();
        assert_eq!(v[0]["user_id"], "alice");

        // Filled per row, also when other rows set it.
        let rows = json!([{ "title": "m1" }, { "title": "m2", "user_id": "alice" }]);
        let v = send_as(&mut c, &alice, "todo", "select=user_id", REP, Action::Insert(body(rows))).unwrap().unwrap();
        assert_eq!(v, json!([{ "user_id": "alice" }, { "user_id": "alice" }]));

        let before = owners(&c);
        let theirs = json!([{ "title": "y" }, { "title": "z", "user_id": "bob" }]);
        let err = send_as(&mut c, &alice, "todo", "", "", Action::Insert(body(theirs)));
        assert_eq!(err, Err(BuildError::RowPolicy("todo".into())));
        assert_eq!(owners(&c), before); // rolled back

        // Service role sets any owner.
        let v = send_as(
            &mut c,
            &Grant::default(),
            "todo",
            "select=user_id",
            REP,
            Action::Insert(body(json!({ "title": "s", "user_id": "bob" }))),
        );
        assert_eq!(v.unwrap(), Some(json!([{ "user_id": "bob" }])));
    }

    #[test]
    fn update_and_delete_touch_own_rows_only() {
        let mut c = db(OWNED);
        let alice = user("alice");
        let v = send_as(
            &mut c,
            &alice,
            "todo",
            "id=in.(1,2)&select=id",
            REP,
            Action::Update(body(json!({ "title": "A" }))),
        )
        .unwrap();
        assert_eq!(v, Some(json!([{ "id": 1 }])));
        assert_eq!(
            send_as(&mut c, &alice, "todo", "id=eq.2", REP, Action::Update(body(json!({ "title": "B" })))).unwrap(),
            Some(json!([]))
        );
        assert_eq!(owners(&c)[1].2, "b");

        // Giving a row away is refused.
        let err = send_as(&mut c, &alice, "todo", "id=eq.1", "", Action::Update(body(json!({ "user_id": "bob" }))));
        assert_eq!(err, Err(BuildError::RowPolicy("todo".into())));
        assert_eq!(owners(&c)[0].1, "alice");

        assert_eq!(send_as(&mut c, &alice, "todo", "id=eq.2&select=id", REP, Action::Delete).unwrap(), Some(json!([])));
        assert_eq!(
            send_as(&mut c, &alice, "todo", "id=gt.0&select=id", REP, Action::Delete).unwrap(),
            Some(json!([{ "id": 1 }]))
        );
        assert_eq!(owners(&c).len(), 1);

        // owner_scope "write": all rows readable, own rows writable.
        let v = send_as(&mut c, &alice, "comment", "id=gt.0&select=id", REP, Action::Delete).unwrap();
        assert_eq!(v, Some(json!([{ "id": 1 }])));
        assert_eq!(count(&c, "comment"), 1);
    }

    #[test]
    fn upsert_onto_another_users_row() {
        let mut c = db(OWNED);
        let alice = user("alice");
        let merge = &format!("{REP},resolution=merge-duplicates");
        let v = send_as(
            &mut c,
            &alice,
            "todo",
            "on_conflict=title&select=title",
            merge,
            Action::Insert(body(json!({ "title": "a", "user_id": "alice" }))),
        );
        assert_eq!(v.unwrap(), Some(json!([{ "title": "a" }])));
        for row in [
            json!({ "title": "b" }),
            json!({ "title": "b", "user_id": "alice" }),
            json!([{ "title": "new" }, { "title": "b" }]),
        ] {
            let err = send_as(&mut c, &alice, "todo", "on_conflict=title", merge, Action::Insert(body(row)));
            assert_eq!(err, Err(BuildError::RowPolicy("todo".into())));
        }
        assert_eq!(owners(&c), [(1, "alice".into(), "a".into()), (2, "bob".into(), "b".into())]);
        let ignore = "resolution=ignore-duplicates";
        assert_eq!(
            send_as(&mut c, &alice, "todo", "on_conflict=title", ignore, Action::Insert(body(json!({ "title": "b" }))))
                .unwrap(),
            None
        );
        assert_eq!(owners(&c)[1].1, "bob");
    }

    #[test]
    fn same_shape_same_sql() {
        let c = novel();
        let schema = introspect(&c).unwrap();
        let table = schema.table("novel").unwrap();
        let q = parse_query("").unwrap();
        let a = build_insert(
            table,
            &q,
            &parse_payload(r#"[{"title":"a","author":"b"}]"#).unwrap(),
            &Prefer::default(),
            false,
            None,
        )
        .unwrap();
        let b = build_insert(
            table,
            &q,
            &parse_payload(r#"[{"title":"c","author":"d"},{"title":"e","author":"f"}]"#).unwrap(),
            &Prefer::default(),
            false,
            None,
        )
        .unwrap();
        assert_eq!(a.sql, b.sql);
    }
}
