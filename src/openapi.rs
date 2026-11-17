//! OpenAPI 3 document for `GET /rest/v1/`, built from the schema cache by
//! hand with `serde_json`. Rarely requested, so it is built on demand and
//! kept until the schema version moves.

use serde_json::{Map, Value, json};

use crate::rpc::{ArgType, Function, Functions, Kind, Returns};
use crate::schema::{Affinity, Column, Schema, Table};

/// The document as JSON text.
pub fn document(schema: &Schema, functions: &Functions) -> String {
    let mut paths = Map::new();
    paths.insert(
        "/".into(),
        json!({ "get": {
            "tags": ["Introspection"],
            "summary": "OpenAPI description (this document)",
            "responses": { "200": { "description": "OK" } },
        }}),
    );
    let mut schemas = Map::new();
    let mut parameters = shared_parameters();
    for t in &schema.tables {
        paths.insert(format!("/{}", t.name), table_path(t));
        schemas.insert(t.name.clone(), table_schema(t));
        for c in &t.columns {
            parameters.insert(
                format!("rowFilter.{}.{}", t.name, c.name),
                json!({ "name": c.name, "in": "query", "required": false, "schema": { "type": "string" },
                        "description": "Filter, e.g. eq.<value>, in.(a,b), is.null" }),
            );
        }
    }
    for f in functions.iter() {
        paths.insert(format!("/rpc/{}", f.name), function_path(f));
    }

    json!({
        "openapi": "3.0.3",
        "info": {
            "title": "Rulite",
            "description": "PostgREST-style REST API generated from the SQLite schema",
            "version": env!("CARGO_PKG_VERSION"),
        },
        "servers": [{ "url": "/rest/v1" }],
        "paths": paths,
        "components": {
            "schemas": schemas,
            "parameters": parameters,
            "responses": { "Error": error_response() },
        },
    })
    .to_string()
}

fn table_path(t: &Table) -> Value {
    let name = &t.name;
    let item = json!({ "$ref": format!("#/components/schemas/{name}") });
    let rows = json!({ "type": "array", "items": item });
    let filters: Vec<Value> = t.columns.iter().map(|c| param_ref(&format!("rowFilter.{name}.{}", c.name))).collect();
    let with = |names: &[&str]| -> Vec<Value> {
        let mut out: Vec<Value> = names.iter().map(|n| param_ref(n)).collect();
        out.extend(filters.iter().cloned());
        out
    };
    let representation = json!({
        "description": "Rows written (Prefer: return=representation)",
        "content": { "application/json": { "schema": rows } },
    });
    let none = json!({ "description": "No content (Prefer: return=minimal)" });
    let error = json!({ "$ref": "#/components/responses/Error" });

    let get = json!({
        "tags": [name],
        "parameters": with(&["select", "order", "limit", "offset", "range", "preferCount"]),
        "responses": {
            "200": { "description": "OK", "content": {
                "application/json": { "schema": rows },
                "application/vnd.pgrst.object+json": { "schema": item },
            }},
            "206": { "description": "Partial content", "content": { "application/json": { "schema": rows } } },
            "default": error,
        },
    });
    if t.view {
        return json!({ "get": get });
    }
    json!({
        "get": get,
        "post": {
            "tags": [name],
            "parameters": [param_ref("select"), param_ref("columns"), param_ref("onConflict"), param_ref("preferInsert")],
            "requestBody": { "required": true, "content": { "application/json": { "schema": { "oneOf": [item, rows] } } } },
            "responses": { "201": representation, "default": error },
        },
        "patch": {
            "tags": [name],
            "parameters": with(&["select", "preferWrite"]),
            "requestBody": { "required": true, "content": { "application/json": { "schema": item } } },
            "responses": { "200": representation, "204": none, "default": error },
        },
        "delete": {
            "tags": [name],
            "parameters": with(&["select", "preferWrite"]),
            "responses": { "200": representation, "204": none, "default": error },
        },
    })
}

/// `POST /rpc/{name}` with the arguments as a JSON object; `GET` with them
/// in the query string too, for a read-only function.
fn function_path(f: &Function) -> Value {
    let arg_type = |ty| match ty {
        ArgType::Text => json!({ "type": "string" }),
        ArgType::Integer => json!({ "type": "integer" }),
        ArgType::Real => json!({ "type": "number" }),
        ArgType::Boolean => json!({ "type": "boolean" }),
        ArgType::Json => json!({}),
    };
    let mut properties = Map::new();
    let mut required = Vec::new();
    let mut query: Vec<Value> = Vec::new();
    for a in &f.args {
        properties.insert(a.name.clone(), arg_type(a.ty));
        if a.default.is_none() {
            required.push(a.name.clone());
        }
        query.push(json!({ "name": a.name, "in": "query", "required": a.default.is_none(), "schema": arg_type(a.ty) }));
    }
    let tag = format!("(rpc) {}", f.name);
    let ok = match f.returns {
        Returns::Void => json!({ "204": { "description": "No content" } }),
        _ => json!({ "200": { "description": "OK", "content": { "application/json": {} } } }),
    };
    let mut responses = ok;
    responses["default"] = json!({ "$ref": "#/components/responses/Error" });
    let body = json!({ "type": "object", "properties": properties, "required": required });
    let mut path = json!({ "post": {
        "tags": [tag],
        "summary": f.description,
        "requestBody": { "required": false, "content": { "application/json": { "schema": body } } },
        "responses": responses,
    }});
    if f.kind == Kind::Query {
        if matches!(f.returns, Returns::Setof | Returns::Row) {
            query.extend(["select", "order", "limit", "offset", "range", "preferCount"].map(param_ref));
        }
        path["get"] = json!({ "tags": [tag], "summary": f.description, "parameters": query, "responses": responses });
    }
    path
}

fn table_schema(t: &Table) -> Value {
    let mut properties = Map::new();
    let mut required = Vec::new();
    for c in &t.columns {
        let mut p = Map::new();
        if let Some(ty) = json_type(c) {
            p.insert("type".into(), ty.into());
        }
        if !c.decl_type.is_empty() {
            p.insert("format".into(), c.decl_type.to_ascii_lowercase().into());
        }
        if t.nullable(c) {
            p.insert("nullable".into(), true.into());
        }
        if let Some(d) = &c.default {
            p.insert("default".into(), d.clone().into());
        }
        let mut notes = Vec::new();
        if c.pk > 0 {
            notes.push("This is a Primary Key.".to_string());
        }
        for fk in &t.foreign_keys {
            if let Some(i) = fk.columns.iter().position(|fc| fc == &c.name) {
                notes.push(format!("This is a Foreign Key to `{}.{}`.", fk.ref_table, fk.ref_columns[i]));
            }
        }
        if !notes.is_empty() {
            p.insert("description".into(), format!("Note:\n{}", notes.join("\n")).into());
        }
        properties.insert(c.name.clone(), p.into());
        if t.required(c) {
            required.push(c.name.clone());
        }
    }
    let mut s = json!({ "type": "object", "properties": properties });
    if !required.is_empty() {
        s["required"] = required.into();
    }
    s
}

/// `None` for JSON columns, which can hold any JSON value.
fn json_type(c: &Column) -> Option<&'static str> {
    if c.json {
        return None;
    }
    Some(match c.affinity {
        Affinity::Integer => "integer",
        Affinity::Real => "number",
        Affinity::Numeric if c.decl_type.to_ascii_uppercase().contains("BOOL") => "integer",
        Affinity::Numeric => "number",
        // BLOBs are read and written as `\x` hex strings.
        Affinity::Text | Affinity::Blob => "string",
    })
}

fn param_ref(name: &str) -> Value {
    json!({ "$ref": format!("#/components/parameters/{name}") })
}

fn shared_parameters() -> Map<String, Value> {
    let query = |name: &str, description: &str| json!({ "name": name, "in": "query", "required": false, "schema": { "type": "string" }, "description": description });
    let header = |name: &str, values: &[&str]| json!({ "name": name, "in": "header", "required": false, "schema": { "type": "string", "enum": values } });
    let mut m = Map::new();
    m.insert("select".into(), query("select", "Columns and embedded resources, e.g. id,title,edition(language)"));
    m.insert("order".into(), query("order", "Ordering, e.g. title.asc,id.desc.nullslast"));
    m.insert(
        "limit".into(),
        json!({ "name": "limit", "in": "query", "required": false, "schema": { "type": "integer" } }),
    );
    m.insert(
        "offset".into(),
        json!({ "name": "offset", "in": "query", "required": false, "schema": { "type": "integer" } }),
    );
    m.insert("columns".into(), query("columns", "Columns to insert from the body, e.g. title,author"));
    m.insert("onConflict".into(), query("on_conflict", "Unique columns that identify a duplicate, e.g. title"));
    m.insert(
        "range".into(),
        json!({ "name": "Range", "in": "header", "required": false, "schema": { "type": "string" },
                                     "description": "Rows to return, e.g. 0-9" }),
    );
    m.insert("preferCount".into(), header("Prefer", &["count=exact"]));
    m.insert(
        "preferInsert".into(),
        header(
            "Prefer",
            &[
                "return=minimal",
                "return=representation",
                "resolution=merge-duplicates",
                "resolution=ignore-duplicates",
                "missing=default",
            ],
        ),
    );
    m.insert("preferWrite".into(), header("Prefer", &["return=minimal", "return=representation"]));
    m
}

fn error_response() -> Value {
    json!({
        "description": "Error",
        "content": { "application/json": { "schema": {
            "type": "object",
            "properties": {
                "code": { "type": "string" },
                "message": { "type": "string" },
                "details": { "type": "string", "nullable": true },
                "hint": { "type": "string", "nullable": true },
            },
        }}},
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::introspect;
    use rusqlite::Connection;

    fn doc(sql: &str) -> Value {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(sql).unwrap();
        serde_json::from_str(&document(&introspect(&conn).unwrap(), &Functions::default())).unwrap()
    }

    #[test]
    fn novel_document() {
        let d = doc(include_str!("../sample/novel.sql"));
        assert_eq!(d["openapi"], "3.0.3");
        for t in ["novel", "edition", "volume", "chapter", "part"] {
            let path = &d["paths"][format!("/{t}")];
            for verb in ["get", "post", "patch", "delete"] {
                assert!(path[verb].is_object(), "{t} {verb}");
            }
            assert_eq!(
                path["get"]["responses"]["200"]["content"]["application/json"]["schema"]["items"]["$ref"],
                format!("#/components/schemas/{t}")
            );
        }
        let edition = &d["components"]["schemas"]["edition"];
        assert!(
            edition["properties"]["novel_id"]["description"].as_str().unwrap().contains("Foreign Key to `novel.id`")
        );
        assert!(edition["required"].as_array().unwrap().contains(&json!("novel_id")));
        assert!(d["components"]["parameters"]["rowFilter.novel.title"].is_object());

        // Every $ref resolves.
        let text = d.to_string();
        for r in text.split("\"$ref\":\"#/components/").skip(1) {
            let r = &r[..r.find('"').unwrap()];
            let (kind, name) = r.split_once('/').unwrap();
            assert!(d["components"][kind][name].is_object(), "{r}");
        }
    }

    #[test]
    fn views_and_functions() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(include_str!("../sample/novel.sql")).unwrap();
        let parse = |name: &str, text: &str| crate::rpc::parse(name, text).unwrap();
        let functions = crate::rpc::check(
            &conn,
            vec![
                parse("find", "-- Novels by title.\n-- @param q text\n-- @param n integer = 5\nSELECT title FROM novel WHERE title = :q LIMIT :n"),
                parse("wipe", "-- @returns void\nDELETE FROM part"),
            ],
        )
        .unwrap();
        let d: Value = serde_json::from_str(&document(&introspect(&conn).unwrap(), &functions)).unwrap();
        let view = d["paths"]["/novel_stats"].as_object().unwrap();
        assert_eq!(view.keys().collect::<Vec<_>>(), ["get"]);
        let find = &d["paths"]["/rpc/find"];
        assert_eq!(find["post"]["summary"], "Novels by title.");
        assert_eq!(find["post"]["requestBody"]["content"]["application/json"]["schema"]["required"], json!(["q"]));
        assert_eq!(
            find["get"]["parameters"][1],
            json!({ "name": "n", "in": "query", "required": false, "schema": { "type": "integer" } })
        );
        assert!(d["paths"]["/rpc/wipe"]["get"].is_null());
        assert!(d["paths"]["/rpc/wipe"]["post"]["responses"]["204"].is_object());
    }

    #[test]
    fn column_types() {
        let d = doc(
            "CREATE TABLE t (id INTEGER PRIMARY KEY, n REAL NOT NULL, b BOOLEAN NOT NULL DEFAULT 0, data BLOB, j JSON, s TEXT NOT NULL);",
        );
        let p = &d["components"]["schemas"]["t"]["properties"];
        assert_eq!(
            (&p["id"]["type"], &p["n"]["type"], &p["b"]["type"]),
            (&json!("integer"), &json!("number"), &json!("integer"))
        );
        assert_eq!((&p["data"]["type"], &p["s"]["format"]), (&json!("string"), &json!("text")));
        assert!(p["j"].get("type").is_none());
        assert_eq!(p["data"]["nullable"], true);
        assert_eq!(d["components"]["schemas"]["t"]["required"], json!(["n", "s"])); // id is the rowid, b has a default
    }
}
