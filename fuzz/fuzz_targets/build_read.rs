//! T6.5 / T12.5: parsed queries that pass validation build SQL that
//!   * names only tables and columns of the schema (every `"identifier"` and
//!   `'literal'` in the SQL is a schema name; request values are bound),
//!   * respects the grant: a built read means the root table is readable, and
//!     an owner filter binds the uid instead of writing it into the SQL, and
//!   * SQLite accepts and runs.
//!
//! Input: first byte picks the table across `novel.db` and `event.db`, the
//! second the grant (0 = no checks), the rest is the query string.

#![no_main]

use std::cell::RefCell;
use std::collections::HashSet;
use std::sync::Arc;

use libfuzzer_sys::fuzz_target;
use rusqlite::Connection;
use rusqlite::types::Value;

// rulite is a binary crate: the modules under test are compiled in directly.
#[allow(dead_code)]
#[path = "../../src/query.rs"]
mod query;
#[allow(dead_code)]
#[path = "../../src/schema.rs"]
mod schema;
#[allow(dead_code)]
#[path = "../../src/sql.rs"]
mod sql;

struct Db {
    conn: Connection,
    schema: schema::Schema,
    /// Every table and column name.
    names: HashSet<String>,
}

fn load(sql: &str) -> Db {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(sql).unwrap();
    let schema = schema::introspect(&conn).unwrap();
    let mut names = HashSet::new();
    for t in &schema.tables {
        names.insert(t.name.clone());
        names.extend(t.columns.iter().map(|c| c.name.clone()));
    }
    Db { conn, schema, names }
}

thread_local! {
    static DBS: RefCell<Vec<Db>> = RefCell::new(vec![
        load(include_str!("../../sample/novel.sql")),
        load(include_str!("../../sample/event.sql")),
    ]);
}

const LIMITS: sql::Limits = sql::Limits { default: 100, max: 1000 };
/// Never a schema name; quotes would show up as foreign identifiers if inlined.
const UID: &str = "uid'\"--";

/// A grant from one byte: per table a permission and maybe an owner column
/// (a real one or a missing one); bit 6 gives a uid, bit 7 `owner_scope: write`.
fn grant(schema: &schema::Schema, g: u8) -> sql::Grant {
    if g == 0 {
        return sql::Grant::default();
    }
    let tables = schema
        .tables
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let bits = g.rotate_left(i as u32);
            let perm = match bits & 3 {
                0 => sql::Perm::None,
                1 => sql::Perm::Read,
                _ => sql::Perm::Write,
            };
            let owner = match (bits >> 2) & 3 {
                0 | 1 => None,
                2 => Some(t.columns[(g as usize + i) % t.columns.len()].name.clone()),
                _ => Some("no_such_owner".to_string()),
            };
            (t.name.clone(), sql::Rule { perm, owner, read_all: g & 0x80 != 0 })
        })
        .collect();
    let policy = sql::Policy { tables, default: sql::Rule::default() };
    sql::Grant { policy: Some(Arc::new(policy)), uid: (g & 0x40 != 0).then(|| UID.to_string()) }
}

/// The contents of every `q`-quoted token in `sql` (doubled quotes unescaped).
fn quoted(sql: &str, q: char) -> Vec<String> {
    let (mut out, mut chars) = (Vec::new(), sql.chars().peekable());
    let other = if q == '"' { '\'' } else { '"' };
    while let Some(c) = chars.next() {
        if c != q && c != other {
            continue;
        }
        let mut token = String::new();
        loop {
            match chars.next() {
                Some(x) if x == c && chars.peek() == Some(&c) => {
                    chars.next();
                    token.push(c);
                }
                Some(x) if x == c => break,
                Some(x) => token.push(x),
                None => panic!("unterminated {c} in {sql}"),
            }
        }
        if c == q {
            out.push(token);
        }
    }
    out
}

fuzz_target!(|data: &[u8]| {
    let [pick, g, rest @ ..] = data else { return };
    let (pick, g) = (*pick, *g);
    let Ok(raw) = std::str::from_utf8(rest) else { return };
    let Ok(query) = query::parse_query(raw) else { return };

    DBS.with_borrow(|dbs| {
        let tables: Vec<(&Db, &str)> =
            dbs.iter().flat_map(|db| db.schema.tables.iter().map(move |t| (db, t.name.as_str()))).collect();
        let (db, table) = tables[pick as usize % tables.len()];

        let grant = grant(&db.schema, g);
        let Ok(read) = sql::build_read(&db.schema, table, &query, None, pick & 0x80 != 0, LIMITS, &grant) else {
            return;
        };
        let sql = &read.stmt.sql;
        for name in quoted(sql, '"').into_iter().chain(quoted(sql, '\'')) {
            assert!(db.names.contains(&name), "{name:?} is not a schema name\nquery: {raw}\nsql: {sql}");
        }
        let root = db.schema.table(table).unwrap();
        let owner = grant.owner(root, sql::Perm::Read).expect("built, so the root is readable");
        if owner.is_some() {
            assert!(read.stmt.params.contains(&Value::Text(UID.into())), "uid not bound\nquery: {raw}\nsql: {sql}");
        }
        if let Err(e) = db.conn.prepare(sql) {
            panic!("SQLite rejected the SQL: {e}\nquery: {raw}\nsql: {sql}");
        }
        let _ = read.run(&db.conn);
    });
});
