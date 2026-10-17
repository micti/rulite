//! Table permissions from the config's `tables`: resolves `"*"` and each
//! entry into one [`Policy`] per role, and checks the names against the schema.

use serde_json::Value;

use crate::schema::Schema;
use crate::sql::{Perm, Policy, Rule};

/// The policies of the two roles that are checked (`service_role` never is).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Access {
    pub anon: Policy,
    pub authenticated: Policy,
    /// Config entries with an owner column: `(table, column)`.
    owners: Vec<(String, String)>,
}

/// One entry as written; unset fields fall back to `"*"`.
#[derive(Default)]
struct Entry {
    anon: Option<Perm>,
    authenticated: Option<Perm>,
    owner: Option<String>,
    read_all: bool,
}

impl Access {
    /// Parses the `tables` object. Refuses unknown fields and values, an
    /// owner on `"*"`, and `anon: "write"` on an owner table.
    pub fn parse(tables: &Value) -> Result<Access, String> {
        let tables = tables.as_object().ok_or("tables must be an object")?;
        let star = match tables.get("*") {
            Some(v) => entry("*", v)?,
            None => Entry::default(),
        };
        if star.owner.is_some() {
            return Err("tables.\"*\": owner must be set per table".into());
        }
        let mut access = Access {
            anon: Policy { tables: Vec::new(), default: rule(star.anon, None, false) },
            authenticated: Policy { tables: Vec::new(), default: rule(star.authenticated, None, false) },
            owners: Vec::new(),
        };
        for (name, v) in tables.iter().filter(|(name, _)| *name != "*") {
            let e = entry(name, v)?;
            let anon = e.anon.or(star.anon);
            if e.owner.is_some() && anon == Some(Perm::Write) {
                return Err(format!("tables.{name}: anon can't write an owner table (it has no user id)"));
            }
            if let Some(owner) = &e.owner {
                access.owners.push((name.clone(), owner.clone()));
            }
            // The owner check applies to signed-in users only.
            access.anon.tables.push((name.clone(), rule(anon, None, false)));
            let authenticated = rule(e.authenticated.or(star.authenticated), e.owner, e.read_all);
            access.authenticated.tables.push((name.clone(), authenticated));
        }
        // serde_json's map is sorted, so the policies are too.
        Ok(access)
    }

    /// Whether any table is open to anon or authenticated.
    pub fn granted(&self) -> bool {
        [&self.anon, &self.authenticated]
            .iter()
            .any(|p| p.default.perm != Perm::None || p.tables.iter().any(|(_, r)| r.perm != Perm::None))
    }

    /// Config names the schema doesn't have: unknown tables (they may be
    /// created later) and owner columns (the table is denied until fixed).
    pub fn warnings(&self, schema: &Schema) -> Vec<String> {
        let mut out = Vec::new();
        for (name, _) in &self.anon.tables {
            let Some(table) = schema.table(name) else {
                out.push(format!("tables.{name}: no such table"));
                continue;
            };
            if let Some((_, owner)) = self.owners.iter().find(|(t, _)| t == name)
                && table.column(owner).is_none()
            {
                out.push(format!("tables.{name}: no column {owner}, the table is closed to anon and authenticated"));
            }
        }
        out
    }
}

fn rule(perm: Option<Perm>, owner: Option<String>, read_all: bool) -> Rule {
    Rule { perm: perm.unwrap_or_default(), owner, read_all }
}

fn entry(name: &str, v: &Value) -> Result<Entry, String> {
    let fields = v.as_object().ok_or_else(|| format!("tables.{name} must be an object"))?;
    let mut e = Entry::default();
    for (field, value) in fields {
        let bad = || format!("tables.{name}.{field}: invalid value {value}");
        let text = value.as_str().ok_or_else(bad);
        match field.as_str() {
            "anon" => e.anon = Some(perm(text?).ok_or_else(bad)?),
            "authenticated" => e.authenticated = Some(perm(text?).ok_or_else(bad)?),
            "owner" => match text? {
                "" => return Err(bad()),
                column => e.owner = Some(column.to_string()),
            },
            "owner_scope" => {
                e.read_all = match text? {
                    "all" => false,
                    "write" => true,
                    _ => return Err(bad()),
                }
            }
            _ => return Err(format!("tables.{name}: unknown field {field}")),
        }
    }
    if e.read_all && e.owner.is_none() {
        return Err(format!("tables.{name}: owner_scope needs an owner column"));
    }
    Ok(e)
}

fn perm(s: &str) -> Option<Perm> {
    match s {
        "none" => Some(Perm::None),
        "read" => Some(Perm::Read),
        "write" => Some(Perm::Write),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn parse(v: Value) -> Result<Access, String> {
        Access::parse(&v)
    }

    #[test]
    fn star_defaults_and_entries() {
        let a = parse(json!({
            "*": { "authenticated": "read" },
            "tag": { "anon": "read" },
            "todo": { "authenticated": "write", "owner": "user_id" },
            "comment": { "anon": "read", "authenticated": "write", "owner": "author_id", "owner_scope": "write" },
        }))
        .unwrap();
        let r = |p: &Policy, t: &str| p.rule(t).clone();
        assert_eq!(r(&a.anon, "nope"), Rule::default()); // nobody granted: closed
        assert_eq!(r(&a.authenticated, "nope").perm, Perm::Read);
        assert_eq!((r(&a.anon, "tag").perm, r(&a.authenticated, "tag").perm), (Perm::Read, Perm::Read));
        assert_eq!(
            r(&a.authenticated, "todo"),
            Rule { perm: Perm::Write, owner: Some("user_id".into()), read_all: false }
        );
        assert_eq!(r(&a.anon, "todo"), Rule::default());
        assert_eq!(r(&a.anon, "comment"), Rule { perm: Perm::Read, owner: None, read_all: false });
        assert!(r(&a.authenticated, "comment").read_all);
        assert_eq!(parse(json!({})).unwrap().authenticated.rule("x").perm, Perm::None);
        assert!(a.granted());
        assert!(!parse(json!({})).unwrap().granted());
        assert!(!parse(json!({ "t": { "anon": "none" } })).unwrap().granted());
        assert!(parse(json!({ "*": { "anon": "read" } })).unwrap().granted());
    }

    #[test]
    fn refusals() {
        for bad in [
            json!([]),
            json!({ "t": { "anon": "admin" } }),
            json!({ "t": { "anon": true } }),
            json!({ "t": { "select": "read" } }),
            json!({ "t": { "owner": "" } }),
            json!({ "t": { "owner": "u", "owner_scope": "some" } }),
            json!({ "t": { "owner_scope": "write" } }),
            json!({ "t": { "anon": "write", "owner": "u" } }),
            json!({ "*": { "anon": "write" }, "t": { "owner": "u" } }),
            json!({ "*": { "owner": "u" } }),
        ] {
            assert!(parse(bad.clone()).is_err(), "{bad}");
        }
        assert!(parse(json!({ "*": { "anon": "write" }, "t": { "anon": "read", "owner": "u" } })).is_ok());
    }

    #[test]
    fn warnings_against_the_schema() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE todo (id INTEGER PRIMARY KEY, user_id TEXT); CREATE TABLE note (id INTEGER PRIMARY KEY);",
        )
        .unwrap();
        let schema = crate::schema::introspect(&conn).unwrap();
        let a = parse(json!({
            "todo": { "authenticated": "write", "owner": "user_id" },
            "note": { "authenticated": "write", "owner": "author_id" },
            "later": { "anon": "read" },
        }))
        .unwrap();
        assert_eq!(a.warnings(&schema).len(), 2);
    }
}
