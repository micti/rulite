//! Schema cache: tables, columns, unique keys, foreign keys and the relationships
//! derived from them. Introspected once and reloaded only when
//! `PRAGMA schema_version` changes.

// Most of this API is consumed by the SQL builder (M3) and writes (M4).
#![allow(dead_code)]

use std::sync::{Arc, RwLock};

use rusqlite::Connection;

#[derive(Debug)]
pub struct Schema {
    /// `PRAGMA schema_version` this snapshot was read at.
    pub version: i64,
    /// Sorted by name, so lookups are a binary search.
    pub tables: Vec<Table>,
}

#[derive(Debug)]
pub struct Table {
    pub name: String,
    pub strict: bool,
    pub without_rowid: bool,
    /// In declaration order.
    pub columns: Vec<Column>,
    /// Primary key columns in key order; empty when the table only has a rowid.
    pub primary_key: Vec<String>,
    /// Column sets that identify a row: the primary key first, then each full
    /// (non-partial, non-expression) unique index.
    pub uniques: Vec<Vec<String>>,
    pub foreign_keys: Vec<ForeignKey>,
    /// Relationships starting at this table (derived from all foreign keys).
    pub relationships: Vec<Relationship>,
}

#[derive(Debug)]
pub struct Column {
    pub name: String,
    /// Declared type as written in `CREATE TABLE` (may be empty).
    pub decl_type: String,
    pub affinity: Affinity,
    pub not_null: bool,
    /// Default value as SQL text, e.g. `'ongoing'` or `(datetime('now'))`.
    pub default: Option<String>,
    /// 1-based position in the primary key, 0 when not part of it.
    pub pk: u8,
    /// Holds JSON text: declared `JSON`/`JSONB` or checked with `json_valid(col)`.
    pub json: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Affinity {
    Integer,
    Text,
    Blob,
    Real,
    Numeric,
}

#[derive(Debug)]
pub struct ForeignKey {
    /// Columns on this (child) table.
    pub columns: Vec<String>,
    pub ref_table: String,
    /// Columns on the parent table, resolved to its primary key when the
    /// `REFERENCES` clause names none.
    pub ref_columns: Vec<String>,
    pub on_update: String,
    pub on_delete: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cardinality {
    /// Origin holds the foreign key (child → parent), embeds one object.
    ManyToOne,
    /// Target holds the foreign key (parent → children), embeds an array.
    OneToMany,
    /// Through a junction table, embeds an array.
    ManyToMany,
}

#[derive(Debug)]
pub struct Relationship {
    pub kind: Cardinality,
    pub table: String,
    pub foreign_table: String,
    /// Origin columns, paired by position with `foreign_columns` (direct
    /// relationships) or with `junction.columns` (many-to-many).
    pub columns: Vec<String>,
    /// Target columns, paired by position with `columns` (direct) or with
    /// `junction.foreign_columns` (many-to-many).
    pub foreign_columns: Vec<String>,
    pub junction: Option<Junction>,
}

#[derive(Debug)]
pub struct Junction {
    pub table: String,
    /// Junction columns referencing the origin's `columns`.
    pub columns: Vec<String>,
    /// Junction columns referencing the target's `foreign_columns`.
    pub foreign_columns: Vec<String>,
}

#[derive(Debug)]
pub enum RelError<'a> {
    /// No relationship between the tables (or none matching the hint).
    NotFound,
    /// Several relationships match; the request needs a `table!hint`.
    Ambiguous(Vec<&'a Relationship>),
}

impl Schema {
    pub fn table(&self, name: &str) -> Option<&Table> {
        self.tables.binary_search_by(|t| t.name.as_str().cmp(name)).ok().map(|i| &self.tables[i])
    }

    /// Resolves the embed `target` (or `target!hint`) requested from `from`.
    ///
    /// A hint names the foreign key column(s) (comma separated for composite
    /// keys) or, for many-to-many, the junction table. On a self reference a
    /// column hint matches both directions; the many-to-one side (the row's
    /// own foreign key) wins.
    pub fn relationship<'a>(
        &'a self,
        from: &str,
        target: &str,
        hint: Option<&str>,
    ) -> Result<&'a Relationship, RelError<'a>> {
        self.relationship_to(from, target, hint, None)
    }

    /// [`Schema::relationship`] restricted to one direction when `to_many` is
    /// given: `false` keeps many-to-one, `true` one-to-many and many-to-many.
    pub fn relationship_to<'a>(
        &'a self,
        from: &str,
        target: &str,
        hint: Option<&str>,
        to_many: Option<bool>,
    ) -> Result<&'a Relationship, RelError<'a>> {
        let table = self.table(from).ok_or(RelError::NotFound)?;
        let mut found: Vec<&Relationship> = table
            .relationships
            .iter()
            .filter(|r| r.foreign_table == target && hint.is_none_or(|h| r.matches_hint(h)))
            .filter(|r| to_many.is_none_or(|many| many == (r.kind != Cardinality::ManyToOne)))
            .collect();
        if hint.is_some() && found.len() > 1 && found.iter().all(|r| r.is_self_reference()) {
            found.retain(|r| r.kind == Cardinality::ManyToOne);
        }
        match found.len() {
            0 => Err(RelError::NotFound),
            1 => Ok(found[0]),
            _ => Err(RelError::Ambiguous(found)),
        }
    }
}

impl Table {
    pub fn column(&self, name: &str) -> Option<&Column> {
        self.columns.iter().find(|c| c.name == name)
    }

    /// `INTEGER PRIMARY KEY` on a rowid table: an alias of the rowid, filled in by SQLite.
    pub fn is_rowid_alias(&self, c: &Column) -> bool {
        !self.without_rowid && self.primary_key.len() == 1 && c.pk == 1 && c.decl_type.eq_ignore_ascii_case("INTEGER")
    }

    /// Whether `c` can hold NULL. Primary keys of STRICT and WITHOUT ROWID
    /// tables are NOT NULL even when not declared so, as is the rowid.
    pub fn nullable(&self, c: &Column) -> bool {
        !c.not_null && !(c.pk > 0 && (self.strict || self.without_rowid || self.is_rowid_alias(c)))
    }

    /// Whether an insert must give `c`: not nullable, no default, not the rowid.
    pub fn required(&self, c: &Column) -> bool {
        !self.nullable(c) && c.default.is_none() && !self.is_rowid_alias(c)
    }
}

impl Relationship {
    pub fn is_self_reference(&self) -> bool {
        self.table == self.foreign_table
    }

    /// The `table!hint` that selects this relationship.
    pub fn hint(&self) -> String {
        match (&self.kind, &self.junction) {
            (_, Some(j)) => j.table.clone(),
            (Cardinality::ManyToOne, _) => self.columns.join(","),
            _ => self.foreign_columns.join(","),
        }
    }

    fn matches_hint(&self, hint: &str) -> bool {
        let fk_columns = match (&self.kind, &self.junction) {
            (_, Some(j)) => return j.table == hint,
            (Cardinality::ManyToOne, _) => &self.columns,
            _ => &self.foreign_columns,
        };
        hint.split(',').map(str::trim).eq(fk_columns.iter().map(String::as_str))
    }
}

impl Affinity {
    /// SQLite's rules from https://www.sqlite.org/datatype3.html#determination_of_column_affinity
    pub fn from_decl_type(decl: &str) -> Affinity {
        let t = decl.to_ascii_uppercase();
        if t.contains("INT") {
            Affinity::Integer
        } else if t.contains("CHAR") || t.contains("CLOB") || t.contains("TEXT") {
            Affinity::Text
        } else if t.is_empty() || t.contains("BLOB") || t == "ANY" {
            Affinity::Blob
        } else if t.contains("REAL") || t.contains("FLOA") || t.contains("DOUB") {
            Affinity::Real
        } else {
            Affinity::Numeric
        }
    }
}

pub fn schema_version(conn: &Connection) -> rusqlite::Result<i64> {
    conn.prepare_cached("PRAGMA schema_version")?.query_row([], |r| r.get(0))
}

/// Reads the whole schema in one read transaction. The auth tables are
/// skipped, so `/rest/v1` never exposes them.
pub fn introspect(conn: &Connection) -> rusqlite::Result<Schema> {
    let tx = conn.unchecked_transaction()?;
    let version = schema_version(&tx)?;

    let mut tables = Vec::new();
    // `CREATE TABLE` text, only to spot `json_valid(col)` checks.
    let mut sqls = Vec::new();
    {
        let mut list = tx.prepare(
            "SELECT l.name, l.wr, l.strict, s.sql FROM pragma_table_list AS l \
             JOIN sqlite_schema AS s ON s.type = 'table' AND s.name = l.name \
             WHERE l.schema = 'main' AND l.type = 'table' AND l.name NOT LIKE 'sqlite\\_%' ESCAPE '\\' \
             AND l.name NOT IN ('auth_users', 'auth_sessions', 'auth_refresh_tokens') \
             ORDER BY l.name",
        )?;
        let mut rows = list.query([])?;
        while let Some(row) = rows.next()? {
            sqls.push(row.get::<_, Option<String>>(3)?.unwrap_or_default());
            tables.push(Table {
                name: row.get(0)?,
                without_rowid: row.get(1)?,
                strict: row.get(2)?,
                columns: Vec::new(),
                primary_key: Vec::new(),
                uniques: Vec::new(),
                foreign_keys: Vec::new(),
                relationships: Vec::new(),
            });
        }
    }

    let mut columns =
        tx.prepare("SELECT name, type, \"notnull\", dflt_value, pk FROM pragma_table_info(?1) ORDER BY cid")?;
    let mut indexes =
        tx.prepare("SELECT name FROM pragma_index_list(?1) WHERE \"unique\" AND NOT partial ORDER BY seq DESC")?;
    let mut index_columns = tx.prepare("SELECT name FROM pragma_index_info(?1) ORDER BY seqno")?;
    let mut fks = tx.prepare(
        "SELECT id, \"table\", \"from\", \"to\", on_update, on_delete \
         FROM pragma_foreign_key_list(?1) ORDER BY id, seq",
    )?;

    for (t, sql) in tables.iter_mut().zip(&sqls) {
        let json_checked = json_valid_columns(sql);
        t.columns = columns
            .query_map([&t.name], |r| {
                let decl_type: String = r.get::<_, Option<String>>(1)?.unwrap_or_default();
                Ok(Column {
                    name: r.get(0)?,
                    affinity: Affinity::from_decl_type(&decl_type),
                    decl_type,
                    not_null: r.get(2)?,
                    default: r.get(3)?,
                    pk: r.get(4)?,
                    json: false,
                })
            })?
            .collect::<Result<_, _>>()?;

        for c in &mut t.columns {
            let decl = c.decl_type.to_ascii_uppercase();
            c.json = decl == "JSON" || decl == "JSONB" || json_checked.iter().any(|j| j.eq_ignore_ascii_case(&c.name));
        }

        let mut pk: Vec<&Column> = t.columns.iter().filter(|c| c.pk > 0).collect();
        pk.sort_by_key(|c| c.pk);
        t.primary_key = pk.iter().map(|c| c.name.clone()).collect();
        if !t.primary_key.is_empty() {
            t.uniques.push(t.primary_key.clone());
        }

        let index_names: Vec<String> = indexes.query_map([&t.name], |r| r.get(0))?.collect::<Result<_, _>>()?;
        for index in index_names {
            // NULL name = expression column; such an index can't back `on_conflict`.
            let cols: Option<Vec<String>> =
                index_columns.query_map([&index], |r| r.get::<_, Option<String>>(0))?.collect::<Result<_, _>>()?;
            if let Some(cols) = cols
                && !t.uniques.contains(&cols)
            {
                t.uniques.push(cols);
            }
        }

        let mut rows = fks.query([&t.name])?;
        let mut last_id = None;
        while let Some(r) = rows.next()? {
            let id: i64 = r.get(0)?;
            if last_id != Some(id) {
                last_id = Some(id);
                t.foreign_keys.push(ForeignKey {
                    columns: Vec::new(),
                    ref_table: r.get(1)?,
                    ref_columns: Vec::new(),
                    on_update: r.get(4)?,
                    on_delete: r.get(5)?,
                });
            }
            let fk = t.foreign_keys.last_mut().expect("pushed above");
            fk.columns.push(r.get(2)?);
            // NULL `to` = the parent's primary key; resolved below.
            if let Some(to) = r.get::<_, Option<String>>(3)? {
                fk.ref_columns.push(to);
            }
        }
    }
    drop((columns, indexes, index_columns, fks));
    tx.finish()?;

    // Resolve implicit parent keys; drop keys pointing at tables we don't expose.
    let pks: Vec<(String, Vec<String>)> = tables.iter().map(|t| (t.name.clone(), t.primary_key.clone())).collect();
    for t in &mut tables {
        t.foreign_keys.retain_mut(|fk| {
            let Some((_, pk)) = pks.iter().find(|(name, _)| *name == fk.ref_table) else {
                return false;
            };
            if fk.ref_columns.is_empty() {
                fk.ref_columns = pk.clone();
            }
            fk.ref_columns.len() == fk.columns.len()
        });
    }

    let mut schema = Schema { version, tables };
    derive_relationships(&mut schema);
    Ok(schema)
}

/// Column names passed to `json_valid(...)` in a `CREATE TABLE` statement.
fn json_valid_columns(sql: &str) -> Vec<String> {
    let lower = sql.to_ascii_lowercase();
    lower
        .match_indices("json_valid(")
        .filter_map(|(i, m)| {
            let start = i + m.len();
            let arg = sql[start..start + lower[start..].find(')')?].trim();
            let unquoted =
                arg.strip_prefix(['"', '`', '[']).and_then(|a| a.strip_suffix(['"', '`', ']'])).unwrap_or(arg);
            Some(unquoted.to_string())
        })
        .collect()
}

fn derive_relationships(schema: &mut Schema) {
    let mut rels = Vec::new();
    for t in &schema.tables {
        for fk in &t.foreign_keys {
            rels.push(Relationship {
                kind: Cardinality::ManyToOne,
                table: t.name.clone(),
                foreign_table: fk.ref_table.clone(),
                columns: fk.columns.clone(),
                foreign_columns: fk.ref_columns.clone(),
                junction: None,
            });
            rels.push(Relationship {
                kind: Cardinality::OneToMany,
                table: fk.ref_table.clone(),
                foreign_table: t.name.clone(),
                columns: fk.ref_columns.clone(),
                foreign_columns: fk.columns.clone(),
                junction: None,
            });
        }

        // Junction table: every pair of foreign keys whose columns all sit in the
        // primary key links the two referenced tables many-to-many.
        let in_pk = |fk: &&ForeignKey| fk.columns.iter().all(|c| t.primary_key.contains(c));
        let keyed: Vec<&ForeignKey> = t.foreign_keys.iter().filter(in_pk).collect();
        for (i, a) in keyed.iter().enumerate() {
            for (j, b) in keyed.iter().enumerate() {
                if i == j {
                    continue;
                }
                rels.push(Relationship {
                    kind: Cardinality::ManyToMany,
                    table: a.ref_table.clone(),
                    foreign_table: b.ref_table.clone(),
                    columns: a.ref_columns.clone(),
                    foreign_columns: b.ref_columns.clone(),
                    junction: Some(Junction {
                        table: t.name.clone(),
                        columns: a.columns.clone(),
                        foreign_columns: b.columns.clone(),
                    }),
                });
            }
        }
    }

    for rel in rels {
        if let Ok(i) = schema.tables.binary_search_by(|t| t.name.cmp(&rel.table)) {
            schema.tables[i].relationships.push(rel);
        }
    }
}

/// Shares the current [`Schema`] between DB threads and swaps it when the
/// database's schema version moves.
pub struct SchemaCache {
    current: RwLock<Arc<Schema>>,
}

impl SchemaCache {
    pub fn new(schema: Schema) -> SchemaCache {
        SchemaCache { current: RwLock::new(Arc::new(schema)) }
    }

    pub fn get(&self) -> Arc<Schema> {
        self.current.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Returns the schema matching `conn`'s view of the database, reloading it
    /// first if `PRAGMA schema_version` changed. One cheap pragma per call.
    pub fn refresh(&self, conn: &Connection) -> rusqlite::Result<Arc<Schema>> {
        let cached = self.get();
        if schema_version(conn)? == cached.version {
            return Ok(cached);
        }
        let fresh = Arc::new(introspect(conn)?);
        *self.current.write().unwrap_or_else(|e| e.into_inner()) = fresh.clone();
        Ok(fresh)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load(sql: &str) -> (Connection, Schema) {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(sql).unwrap();
        let schema = introspect(&conn).unwrap();
        (conn, schema)
    }

    fn novel() -> Schema {
        load(include_str!("../sample/novel.sql")).1
    }

    fn event() -> Schema {
        load(include_str!("../sample/event.sql")).1
    }

    fn names(v: &[String]) -> Vec<&str> {
        v.iter().map(String::as_str).collect()
    }

    #[test]
    fn novel_tables_and_foreign_keys() {
        let s = novel();
        let tables: Vec<&str> = s.tables.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(tables, ["chapter", "edition", "novel", "part", "volume"]);
        assert_eq!(s.tables.iter().map(|t| t.foreign_keys.len()).sum::<usize>(), 4);

        let part = s.table("part").unwrap();
        let fk = &part.foreign_keys[0];
        assert_eq!((names(&fk.columns), fk.ref_table.as_str()), (vec!["chapter_id"], "chapter"));
        assert_eq!(names(&fk.ref_columns), ["id"]);
        assert_eq!(fk.on_delete, "CASCADE");
        assert!(s.table("sqlite_sequence").is_none());
    }

    #[test]
    fn auth_tables_are_hidden() {
        let (_, s) = load(
            "CREATE TABLE auth_users (id TEXT PRIMARY KEY);
             CREATE TABLE auth_sessions (id TEXT PRIMARY KEY);
             CREATE TABLE auth_refresh_tokens (id INTEGER PRIMARY KEY);
             CREATE TABLE todo (id INTEGER PRIMARY KEY, user_id TEXT REFERENCES auth_users(id));",
        );
        let tables: Vec<&str> = s.tables.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(tables, ["todo"]);
        assert!(s.table("todo").unwrap().foreign_keys.is_empty());
    }

    #[test]
    fn novel_columns_and_uniques() {
        let s = novel();
        let novel = s.table("novel").unwrap();
        assert!(novel.strict && !novel.without_rowid);
        assert_eq!(novel.primary_key, ["id"]);
        assert_eq!(novel.uniques, [vec!["id".to_string()], vec!["title".to_string()]]);

        let status = novel.column("status").unwrap();
        assert_eq!((status.decl_type.as_str(), status.affinity), ("TEXT", Affinity::Text));
        assert!(status.not_null);
        assert_eq!(status.default.as_deref(), Some("'ongoing'"));
        assert!(!novel.column("genre").unwrap().not_null);

        let edition = s.table("edition").unwrap();
        assert!(edition.uniques.contains(&vec!["novel_id".into(), "language".into()]));
        let volume = s.table("volume").unwrap();
        assert_eq!(volume.column("number").unwrap().affinity, Affinity::Integer);
    }

    #[test]
    fn json_columns() {
        let s = event();
        let artist = s.table("artist").unwrap();
        assert!(artist.column("genres").unwrap().json);
        assert!(!artist.column("name").unwrap().json);

        let (_, s) = load("CREATE TABLE t (a JSON, b TEXT CHECK (JSON_VALID( \"b\" )), c TEXT)");
        let t = s.table("t").unwrap();
        assert_eq!(t.columns.iter().map(|c| c.json).collect::<Vec<_>>(), [true, true, false]);
    }

    #[test]
    fn novel_chain_relationships() {
        let s = novel();
        let r = s.relationship("novel", "edition", None).unwrap();
        assert_eq!(r.kind, Cardinality::OneToMany);
        assert_eq!((names(&r.columns), names(&r.foreign_columns)), (vec!["id"], vec!["novel_id"]));

        let r = s.relationship("part", "chapter", None).unwrap();
        assert_eq!(r.kind, Cardinality::ManyToOne);
        assert_eq!((names(&r.columns), names(&r.foreign_columns)), (vec!["chapter_id"], vec!["id"]));

        assert!(matches!(s.relationship("novel", "part", None), Err(RelError::NotFound)));
        assert!(matches!(s.relationship("novel", "edition", Some("nope")), Err(RelError::NotFound)));
    }

    #[test]
    fn event_self_reference() {
        let s = event();
        let area = s.table("area").unwrap();
        let self_refs: Vec<_> = area.relationships.iter().filter(|r| r.is_self_reference()).collect();
        assert_eq!(self_refs.len(), 2);
        assert!(self_refs.iter().any(|r| r.kind == Cardinality::ManyToOne));
        assert!(self_refs.iter().any(|r| r.kind == Cardinality::OneToMany));

        // Two paths area → area: a hint is required.
        match s.relationship("area", "area", None) {
            Err(RelError::Ambiguous(found)) => assert_eq!(found.len(), 2),
            other => panic!("expected ambiguity, got {other:?}"),
        }
        let parent = s.relationship("area", "area", Some("parent_id")).unwrap();
        assert_eq!(parent.kind, Cardinality::ManyToOne);
        assert_eq!(names(&parent.columns), ["parent_id"]);
        let children = s.relationship_to("area", "area", Some("parent_id"), Some(true)).unwrap();
        assert_eq!(children.kind, Cardinality::OneToMany);
        assert_eq!(names(&children.foreign_columns), ["parent_id"]);
        assert_eq!(s.relationship_to("area", "area", None, Some(false)).unwrap().kind, Cardinality::ManyToOne);

        // Other tables reach `area` through exactly one foreign key each.
        assert_eq!(s.relationship("event", "area", None).unwrap().kind, Cardinality::ManyToOne);
        assert_eq!(s.relationship("area", "sub_event", None).unwrap().kind, Cardinality::OneToMany);
        let r = s.relationship("event", "area", Some("area_id")).unwrap();
        assert_eq!(names(&r.columns), ["area_id"]);
    }

    #[test]
    fn event_many_to_many_through_lineup() {
        let s = event();
        let lineup = s.table("lineup").unwrap();
        assert!(lineup.without_rowid);
        assert_eq!(lineup.primary_key, ["sub_event_id", "artist_id"]);

        for (from, to) in [("sub_event", "artist"), ("artist", "sub_event")] {
            let r = s.relationship(from, to, None).unwrap();
            assert_eq!(r.kind, Cardinality::ManyToMany);
            let j = r.junction.as_ref().unwrap();
            assert_eq!(j.table, "lineup");
            assert_eq!((names(&r.columns), names(&r.foreign_columns)), (vec!["id"], vec!["id"]));
            assert_eq!(names(&j.columns), [format!("{from}_id")]);
            assert_eq!(names(&j.foreign_columns), [format!("{to}_id")]);
            assert!(s.relationship(from, to, Some("lineup")).is_ok());
        }
        // The junction itself is still reachable directly.
        assert_eq!(s.relationship("sub_event", "lineup", None).unwrap().kind, Cardinality::OneToMany);
        assert_eq!(s.relationship("lineup", "artist", None).unwrap().kind, Cardinality::ManyToOne);
    }

    #[test]
    fn ambiguity_between_two_foreign_keys() {
        let (_, s) = load(
            "CREATE TABLE person (id INTEGER PRIMARY KEY);
             CREATE TABLE message (
               id INTEGER PRIMARY KEY,
               sender_id INTEGER REFERENCES person,
               receiver_id INTEGER REFERENCES person(id));",
        );
        assert!(matches!(s.relationship("message", "person", None), Err(RelError::Ambiguous(_))));
        assert!(matches!(s.relationship("person", "message", None), Err(RelError::Ambiguous(_))));
        let r = s.relationship("message", "person", Some("sender_id")).unwrap();
        assert_eq!(names(&r.foreign_columns), ["id"]); // implicit parent key resolved
        let r = s.relationship("person", "message", Some("receiver_id")).unwrap();
        assert_eq!((r.kind, names(&r.foreign_columns)), (Cardinality::OneToMany, vec!["receiver_id"]));
    }

    #[test]
    fn reload_on_schema_change() {
        let (conn, schema) = load(include_str!("../sample/novel.sql"));
        let cache = SchemaCache::new(schema);
        let before = cache.refresh(&conn).unwrap();
        assert!(Arc::ptr_eq(&before, &cache.get())); // unchanged: same snapshot

        conn.execute_batch("CREATE TABLE tag (id INTEGER PRIMARY KEY, name TEXT UNIQUE)").unwrap();
        let after = cache.refresh(&conn).unwrap();
        assert!(after.version > before.version);
        assert!(after.table("tag").is_some());
        assert!(Arc::ptr_eq(&after, &cache.get()));
    }

    #[test]
    fn affinity_rules() {
        use Affinity::*;
        for (decl, want) in [
            ("INTEGER", Integer),
            ("BIGINT", Integer),
            ("VARCHAR(10)", Text),
            ("TEXT", Text),
            ("", Blob),
            ("BLOB", Blob),
            ("ANY", Blob),
            ("REAL", Real),
            ("DOUBLE", Real),
            ("NUMERIC", Numeric),
            ("BOOLEAN", Numeric),
            ("DATE", Numeric),
        ] {
            assert_eq!(Affinity::from_decl_type(decl), want, "{decl}");
        }
    }
}
