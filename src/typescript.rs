//! TypeScript types for supabase-js, laid out as `supabase gen types typescript`
//! prints them, built from the schema cache by hand.

use std::fmt::Write;

use crate::schema::{Affinity, Column, Schema, Table};

const JSON: &str = "\
export type Json =
  | string
  | number
  | boolean
  | null
  | { [key: string]: Json | undefined }
  | Json[]

export type Database = {
  // Allows to automatically instantiate createClient with right options
  // instead of createClient<Database, { PostgrestVersion: 'XX' }>(URL, KEY)
  __InternalSupabase: {
    PostgrestVersion: \"12\"
  }
  public: {
";

const NEVER: &str = "{\n      [_ in never]: never\n    }";

/// The Supabase CLI's helper types, after `Database`.
const HELPERS: &str = r#"type DatabaseWithoutInternals = Omit<Database, "__InternalSupabase">

type DefaultSchema = DatabaseWithoutInternals[Extract<keyof Database, "public">]

export type Tables<
  DefaultSchemaTableNameOrOptions extends
    | keyof (DefaultSchema["Tables"] & DefaultSchema["Views"])
    | { schema: keyof DatabaseWithoutInternals },
  TableName extends DefaultSchemaTableNameOrOptions extends {
    schema: keyof DatabaseWithoutInternals
  }
    ? keyof (DatabaseWithoutInternals[DefaultSchemaTableNameOrOptions["schema"]]["Tables"] &
        DatabaseWithoutInternals[DefaultSchemaTableNameOrOptions["schema"]]["Views"])
    : never = never,
> = DefaultSchemaTableNameOrOptions extends {
  schema: keyof DatabaseWithoutInternals
}
  ? (DatabaseWithoutInternals[DefaultSchemaTableNameOrOptions["schema"]]["Tables"] &
      DatabaseWithoutInternals[DefaultSchemaTableNameOrOptions["schema"]]["Views"])[TableName] extends {
      Row: infer R
    }
    ? R
    : never
  : DefaultSchemaTableNameOrOptions extends keyof (DefaultSchema["Tables"] &
        DefaultSchema["Views"])
    ? (DefaultSchema["Tables"] &
        DefaultSchema["Views"])[DefaultSchemaTableNameOrOptions] extends {
        Row: infer R
      }
      ? R
      : never
    : never

export type TablesInsert<
  DefaultSchemaTableNameOrOptions extends
    | keyof DefaultSchema["Tables"]
    | { schema: keyof DatabaseWithoutInternals },
  TableName extends DefaultSchemaTableNameOrOptions extends {
    schema: keyof DatabaseWithoutInternals
  }
    ? keyof DatabaseWithoutInternals[DefaultSchemaTableNameOrOptions["schema"]]["Tables"]
    : never = never,
> = DefaultSchemaTableNameOrOptions extends {
  schema: keyof DatabaseWithoutInternals
}
  ? DatabaseWithoutInternals[DefaultSchemaTableNameOrOptions["schema"]]["Tables"][TableName] extends {
      Insert: infer I
    }
    ? I
    : never
  : DefaultSchemaTableNameOrOptions extends keyof DefaultSchema["Tables"]
    ? DefaultSchema["Tables"][DefaultSchemaTableNameOrOptions] extends {
        Insert: infer I
      }
      ? I
      : never
    : never

export type TablesUpdate<
  DefaultSchemaTableNameOrOptions extends
    | keyof DefaultSchema["Tables"]
    | { schema: keyof DatabaseWithoutInternals },
  TableName extends DefaultSchemaTableNameOrOptions extends {
    schema: keyof DatabaseWithoutInternals
  }
    ? keyof DatabaseWithoutInternals[DefaultSchemaTableNameOrOptions["schema"]]["Tables"]
    : never = never,
> = DefaultSchemaTableNameOrOptions extends {
  schema: keyof DatabaseWithoutInternals
}
  ? DatabaseWithoutInternals[DefaultSchemaTableNameOrOptions["schema"]]["Tables"][TableName] extends {
      Update: infer U
    }
    ? U
    : never
  : DefaultSchemaTableNameOrOptions extends keyof DefaultSchema["Tables"]
    ? DefaultSchema["Tables"][DefaultSchemaTableNameOrOptions] extends {
        Update: infer U
      }
      ? U
      : never
    : never

export type Enums<
  DefaultSchemaEnumNameOrOptions extends
    | keyof DefaultSchema["Enums"]
    | { schema: keyof DatabaseWithoutInternals },
  EnumName extends DefaultSchemaEnumNameOrOptions extends {
    schema: keyof DatabaseWithoutInternals
  }
    ? keyof DatabaseWithoutInternals[DefaultSchemaEnumNameOrOptions["schema"]]["Enums"]
    : never = never,
> = DefaultSchemaEnumNameOrOptions extends {
  schema: keyof DatabaseWithoutInternals
}
  ? DatabaseWithoutInternals[DefaultSchemaEnumNameOrOptions["schema"]]["Enums"][EnumName]
  : DefaultSchemaEnumNameOrOptions extends keyof DefaultSchema["Enums"]
    ? DefaultSchema["Enums"][DefaultSchemaEnumNameOrOptions]
    : never

export type CompositeTypes<
  PublicCompositeTypeNameOrOptions extends
    | keyof DefaultSchema["CompositeTypes"]
    | { schema: keyof DatabaseWithoutInternals },
  CompositeTypeName extends PublicCompositeTypeNameOrOptions extends {
    schema: keyof DatabaseWithoutInternals
  }
    ? keyof DatabaseWithoutInternals[PublicCompositeTypeNameOrOptions["schema"]]["CompositeTypes"]
    : never = never,
> = PublicCompositeTypeNameOrOptions extends {
  schema: keyof DatabaseWithoutInternals
}
  ? DatabaseWithoutInternals[PublicCompositeTypeNameOrOptions["schema"]]["CompositeTypes"][CompositeTypeName]
  : PublicCompositeTypeNameOrOptions extends keyof DefaultSchema["CompositeTypes"]
    ? DefaultSchema["CompositeTypes"][PublicCompositeTypeNameOrOptions]
    : never

export const Constants = {
  public: {
    Enums: {},
  },
} as const
"#;

/// The `database.types.ts` text for `schema`.
pub fn generate(schema: &Schema) -> String {
    let mut out = String::with_capacity(8192);
    out.push_str(JSON);
    let mut tables: Vec<&Table> = schema.tables.iter().collect();
    tables.sort_by(|a, b| a.name.cmp(&b.name));
    if tables.is_empty() {
        writeln!(out, "    Tables: {NEVER}").unwrap();
    } else {
        out.push_str("    Tables: {\n");
        for t in tables {
            table(&mut out, t);
        }
        out.push_str("    }\n");
    }
    for section in ["Views", "Functions", "Enums", "CompositeTypes"] {
        writeln!(out, "    {section}: {NEVER}").unwrap();
    }
    out.push_str("  }\n}\n\n");
    out.push_str(HELPERS);
    out
}

fn table(out: &mut String, t: &Table) {
    let mut columns: Vec<&Column> = t.columns.iter().collect();
    columns.sort_by(|a, b| a.name.cmp(&b.name));
    writeln!(out, "      {}: {{", name(&t.name)).unwrap();
    for part in ["Row", "Insert", "Update"] {
        writeln!(out, "        {part}: {{").unwrap();
        for c in &columns {
            let optional = match part {
                "Row" => false,
                "Insert" => !t.required(c),
                _ => true,
            };
            let null = if t.nullable(c) { " | null" } else { "" };
            let mark = if optional { "?" } else { "" };
            writeln!(out, "          {}{mark}: {}{null}", name(&c.name), ts_type(c)).unwrap();
        }
        out.push_str("        }\n");
    }

    let mut fks: Vec<(String, &crate::schema::ForeignKey)> =
        t.foreign_keys.iter().map(|fk| (format!("{}_{}_fkey", t.name, fk.columns.join("_")), fk)).collect();
    fks.sort_by(|a, b| (&a.0, &a.1.ref_table).cmp(&(&b.0, &b.1.ref_table)));
    if fks.is_empty() {
        out.push_str("        Relationships: []\n");
    } else {
        out.push_str("        Relationships: [\n");
        for (fk_name, fk) in fks {
            let one_to_one = t.uniques.iter().any(|u| same_set(u, &fk.columns));
            writeln!(
                out,
                "          {{\n            foreignKeyName: {}\n            columns: {}\n            isOneToOne: {one_to_one}\n            referencedRelation: {}\n            referencedColumns: {}\n          }},",
                string(&fk_name),
                list(&fk.columns),
                string(&fk.ref_table),
                list(&fk.ref_columns),
            )
            .unwrap();
        }
        out.push_str("        ]\n");
    }
    out.push_str("      }\n");
}

/// The JSON type a read returns for `c` (see `sql::column_value`).
fn ts_type(c: &Column) -> &'static str {
    if c.json {
        return "Json";
    }
    if c.decl_type.is_empty() || c.decl_type.eq_ignore_ascii_case("ANY") {
        return "unknown";
    }
    match c.affinity {
        Affinity::Integer | Affinity::Real | Affinity::Numeric => "number",
        // BLOBs are read and written as `\x` hex strings.
        Affinity::Text | Affinity::Blob => "string",
    }
}

/// A property name: as is when it is a TS identifier, else a string literal.
fn name(s: &str) -> String {
    let mut chars = s.chars();
    let ident = chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_' || c == '$')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$');
    if ident { s.to_string() } else { string(s) }
}

fn string(s: &str) -> String {
    serde_json::Value::from(s).to_string()
}

fn list(items: &[String]) -> String {
    let quoted: Vec<String> = items.iter().map(|s| string(s)).collect();
    format!("[{}]", quoted.join(", "))
}

fn same_set(a: &[String], b: &[String]) -> bool {
    a.len() == b.len() && a.iter().all(|c| b.contains(c))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::introspect;

    fn types(sql: &str) -> String {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(sql).unwrap();
        generate(&introspect(&conn).unwrap())
    }

    /// The lines of `table`'s `part` block (`Row`, `Insert`, `Update`, `Relationships`).
    fn block(out: &str, table: &str, part: &str) -> Vec<String> {
        let start = out.find(&format!("\n      {table}: {{\n")).expect("table");
        let rest = &out[start..];
        let from = rest.find(&format!("        {part}: ")).expect("part");
        let mut lines = rest[from..].lines();
        if lines.next().unwrap().ends_with("[]") {
            return Vec::new();
        }
        lines.take_while(|l| *l != "        }" && *l != "        ]").map(|l| l.trim().to_string()).collect()
    }

    #[test]
    fn type_mapping_and_nullability() {
        let out = types(
            "CREATE TABLE t (id INTEGER PRIMARY KEY, n INT NOT NULL, r REAL, d NUMERIC, b BOOLEAN NOT NULL DEFAULT 0,
                             s TEXT NOT NULL, v VARCHAR(10), bl BLOB, j JSON, k TEXT CHECK (json_valid(k)), x, a ANY);",
        );
        assert_eq!(
            block(&out, "t", "Row"),
            [
                "a: unknown | null",
                "b: number",
                "bl: string | null",
                "d: number | null",
                "id: number",
                "j: Json | null",
                "k: Json | null",
                "n: number",
                "r: number | null",
                "s: string",
                "v: string | null",
                "x: unknown | null",
            ]
        );
    }

    #[test]
    fn insert_and_update_optionality() {
        let out = types(
            "CREATE TABLE t (id INTEGER PRIMARY KEY, req TEXT NOT NULL, def TEXT NOT NULL DEFAULT 'x', opt TEXT);
             CREATE TABLE s (id TEXT PRIMARY KEY, name TEXT) STRICT;
             CREATE TABLE w (k INT PRIMARY KEY, v TEXT) WITHOUT ROWID;
             CREATE TABLE big (id BIGINT PRIMARY KEY);",
        );
        assert_eq!(block(&out, "t", "Insert"), ["def?: string", "id?: number", "opt?: string | null", "req: string"]);
        assert_eq!(block(&out, "t", "Update"), ["def?: string", "id?: number", "opt?: string | null", "req?: string"]);
        // STRICT and WITHOUT ROWID primary keys are NOT NULL, and have no rowid to fall back on.
        assert_eq!(block(&out, "s", "Row"), ["id: string", "name: string | null"]);
        assert_eq!(block(&out, "s", "Insert"), ["id: string", "name?: string | null"]);
        assert_eq!(block(&out, "w", "Insert"), ["k: number", "v?: string | null"]);
        // Only `INTEGER PRIMARY KEY` is the rowid; a nullable key stays optional.
        assert_eq!(block(&out, "big", "Row"), ["id: number | null"]);
        assert_eq!(block(&out, "big", "Insert"), ["id?: number | null"]);
    }

    #[test]
    fn relationships() {
        let out = types(
            "CREATE TABLE a (id INTEGER PRIMARY KEY, parent_id INT REFERENCES a);
             CREATE TABLE b (id INTEGER PRIMARY KEY);
             CREATE TABLE profile (a_id INT PRIMARY KEY REFERENCES a(id));
             CREATE TABLE u (id INTEGER PRIMARY KEY, x INT, y INT, UNIQUE (y, x), FOREIGN KEY (x, y) REFERENCES p (k1, k2));
             CREATE TABLE p (k1 INT, k2 INT, PRIMARY KEY (k1, k2));
             CREATE TABLE ab (a_id INT REFERENCES a(id), b_id INT REFERENCES b(id), PRIMARY KEY (a_id, b_id)) WITHOUT ROWID;",
        );
        assert_eq!(
            block(&out, "ab", "Relationships"),
            [
                "{",
                "foreignKeyName: \"ab_a_id_fkey\"",
                "columns: [\"a_id\"]",
                "isOneToOne: false",
                "referencedRelation: \"a\"",
                "referencedColumns: [\"id\"]",
                "},",
                "{",
                "foreignKeyName: \"ab_b_id_fkey\"",
                "columns: [\"b_id\"]",
                "isOneToOne: false",
                "referencedRelation: \"b\"",
                "referencedColumns: [\"id\"]",
                "},",
            ]
        );
        let self_ref = block(&out, "a", "Relationships");
        assert!(self_ref.contains(&"foreignKeyName: \"a_parent_id_fkey\"".to_string()));
        assert!(self_ref.contains(&"isOneToOne: false".to_string()));
        assert!(block(&out, "profile", "Relationships").contains(&"isOneToOne: true".to_string()));
        let composite = block(&out, "u", "Relationships");
        assert!(composite.contains(&"foreignKeyName: \"u_x_y_fkey\"".to_string()));
        assert!(composite.contains(&"columns: [\"x\", \"y\"]".to_string()));
        assert!(composite.contains(&"isOneToOne: true".to_string()));
        assert!(out.contains("      b: {\n        Row: {\n          id: number\n        }\n"));
        assert!(out.contains("        Relationships: []\n      }\n      p: {"));
    }

    #[test]
    fn quoted_names_and_auth_tables() {
        let out = types(
            "CREATE TABLE \"my table\" (\"first name\" TEXT, \"1st\" INT, ok_$ INT, \"q\"\"t\" TEXT);
             CREATE TABLE auth_users (id TEXT PRIMARY KEY);
             CREATE TABLE note (id INTEGER PRIMARY KEY, user_id TEXT REFERENCES auth_users(id));",
        );
        assert!(out.contains("      \"my table\": {\n"));
        assert_eq!(
            block(&out, "\"my table\"", "Row"),
            [
                "\"1st\": number | null",
                "\"first name\": string | null",
                "ok_$: number | null",
                "\"q\\\"t\": string | null"
            ]
        );
        assert!(!out.contains("auth_users"));
        assert_eq!(block(&out, "note", "Relationships"), Vec::<String>::new());
    }

    #[test]
    fn layout_and_stable_output() {
        let empty = types("");
        assert!(empty.starts_with("export type Json =\n"));
        assert!(empty.contains("    Tables: {\n      [_ in never]: never\n    }\n    Views: {\n"));
        assert!(empty.contains("PostgrestVersion: \"12\""));
        assert!(empty.ends_with("} as const\n"));
        let sql = "CREATE TABLE z (b TEXT, a TEXT); CREATE TABLE y (id INTEGER PRIMARY KEY);";
        let out = types(sql);
        assert_eq!(out, types(sql));
        assert!(out.find("      y: {").unwrap() < out.find("      z: {").unwrap());
        assert_eq!(block(&out, "z", "Row"), ["a: string | null", "b: string | null"]);
    }

    #[test]
    fn golden_files() {
        for name in ["novel", "event", "todo"] {
            let conn = rusqlite::Connection::open_in_memory().unwrap();
            conn.execute_batch(crate::auth::TABLES).unwrap();
            let sql = std::fs::read_to_string(format!("{}/sample/{name}.sql", env!("CARGO_MANIFEST_DIR"))).unwrap();
            conn.execute_batch(&sql).unwrap();
            let out = generate(&introspect(&conn).unwrap());
            let path = format!("{}/sample/{name}.types.ts", env!("CARGO_MANIFEST_DIR"));
            if std::env::var_os("RULITE_BLESS").is_some() {
                std::fs::write(&path, &out).unwrap();
            }
            assert_eq!(out, std::fs::read_to_string(&path).unwrap(), "{path} (RULITE_BLESS=1 cargo test rewrites it)");
        }
    }
}
