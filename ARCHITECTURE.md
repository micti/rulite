# Rulite — Architecture (end of phase 2)

Recap of what phase 1 (M0–M6) and phase 2 (M7–M12 in [PLAN.md](PLAN.md):
auth, roles, table permissions, owner rows) built, how it fits together, and the rules to keep when
extending it. Audience: developers and AI coding agents. User-facing auth behaviour: [AUTH.md](AUTH.md).

Rulite serves a PostgREST/Supabase-compatible REST API (`/rest/v1/{table}`) generated from any
SQLite file, so `@supabase/supabase-js` works against it unchanged. The guiding constraint is the
[Design rule](README.md#design-rule): **minimal resource footprint**. Measured: 2.9 MB binary, 3.9 MB idle
RSS, ~13k req/s on a 5-level nested query, with or without a JWT per request (see [README › Budget](README.md#budget)).

## 1. Tech stack

| Concern | Choice | Notes |
|---|---|---|
| Language | Rust, edition 2024 | Single **binary** crate (no `lib.rs`) |
| HTTP | `axum` 0.8, `default-features = false` (`http1`, `json`, `query`, `tokio`) | No HTTP/2, no tower middleware stack |
| Runtime | `tokio` `current_thread` (`rt`, `net`, `macros`, `sync`, `signal`) | One OS thread for all HTTP I/O |
| Database | `rusqlite` 0.40 with `bundled` SQLite | One connection per DB thread, no pool |
| JSON | Built **inside SQLite** (`json_object`, `json_group_array`, `json_each`, `->>`) | `serde_json` only for error bodies, OpenAPI and scanning write-body keys |
| Config | Hand-rolled CLI/env parser + JSON config file validated by hand (`src/config.rs`) | No config/clap crate, no serde derive |
| Auth crypto | `hmac` + `sha2` (HS256, written by hand), `bcrypt` (cost 10), `getrandom` | No `jsonwebtoken`/`ring`; base64url hand-written |
| Release profile | `lto = "fat"`, `codegen-units = 1`, `panic = "abort"`, `strip = true` | |
| Integration tests | Node ≥ 22.13, `node:test`, `@supabase/supabase-js`, `node:sqlite` | `tests/` |
| Fuzzing | `cargo-fuzz` (nightly), libFuzzer | `fuzz/` |
| Benchmark | `oha` via `tests/bench.sh` | |

## 2. Source map

```
src/
  main.rs      entry: Command → serve (db::open → current_thread runtime → server::run) | user | keys
  config.rs    subcommands; CLI args > env vars (RULITE_*) > JSON config file > defaults; validation
  access.rs    config `tables` → one Policy per role (anon, authenticated); warnings vs. the schema
  jwt.rs       HS256 sign/verify, base64url, API keys, refresh tokens, UUIDs
  auth.rs      auth_* tables, sessions + refresh rotation, /auth/v1 routes, Role from headers → Grant
  cli.rs       `rulite user add|passwd|delete|list`, `rulite keys`
  db.rs        DB threads (1 writer + 1–2 readers), job channels, Db::read / Db::write
  schema.rs    introspection → Schema (tables, columns, uniques, FKs, relationships); SchemaCache
  query.rs     PURE parser: query string + Prefer/Range/Accept headers → Query AST, Headers
  sql.rs       read SQL builder: (Schema, Query, Grant) → one Statement; Builder shared with writes;
               Grant / Policy / Rule / Perm (plain data, so the fuzz crate can build it)
  write.rs     write SQL builder + executor: INSERT / upsert / UPDATE / DELETE in one transaction
  openapi.rs   OpenAPI 3 document from Schema (hand-written serde_json)
  server.rs    axum routes, header parsing, status codes, PostgREST error mapping, shutdown
tests/         Node integration suite (run.mjs, client.mjs, read.test.mjs, write.test.mjs, bench.sh)
fuzz/          fuzz targets `parse` and `build_read`, seeds, dictionary
sample/        novel.sql / event.sql / todo.sql (dev schemas), todo.json (auth config), built .db files
```

Approximate size: ~6.2k lines of Rust (incl. 98 unit tests), ~1.1k lines of JS tests (94 cases).

## 3. Runtime architecture

```
                 ┌──────────────── HTTP thread (tokio current_thread) ───────────────┐
 client ──HTTP──►│ axum router ─► handler: parse_query + parse_headers (query.rs)    │
                 │                    │                                              │
                 │                    ▼  Db::read / Db::write  (boxed FnOnce job)    │
                 │        bounded mpsc (depth 32 per thread) ── backpressure         │
                 └────────────────────┼───────────────────────────▲─────────────────┘
                                      ▼                           │ oneshot result
          ┌──────── rulite-reader-0..1 (READ_ONLY conn) ──┐   ┌──┴── rulite-writer (RW conn) ───┐
          │ SchemaCache::refresh (1 PRAGMA)               │   │ SchemaCache::refresh             │
          │ sql::build_read → prepare_cached → query_row  │   │ write::execute in IMMEDIATE tx    │
          └───────────────────────────────────────────────┘   └──────────────────────────────────┘
                                      │  one TEXT value = JSON body (no serde)
                                      ▼
                         server.rs: status, Content-Range, Content-Type → response
```

Key points:

- **Threads:** 1 HTTP thread + 1 writer + `--readers` (1–2, default 1) reader threads. DB threads use
  256 KB stacks and own their `Connection` (`SQLITE_OPEN_NO_MUTEX`). Readers are picked round-robin.
- **Jobs:** `Db::run` boxes a closure `FnOnce(&mut Connection, &Schema)`, sends it on the thread's
  bounded channel and awaits a `oneshot`. A full queue makes the HTTP task wait (backpressure).
- **Build on the DB thread:** SQL is built *inside* the job because it needs the schema snapshot
  that matches that connection. Parsing happens on the HTTP thread (no DB needed).
- **PRAGMAs:** writer sets `journal_mode=WAL` once; every connection sets `foreign_keys=ON`,
  `synchronous=NORMAL`, `cache_size=-2000` (2 MB), `temp_store=FILE`.
- **Shutdown:** Ctrl-C / SIGTERM → axum graceful shutdown → last `Db` handle dropped → channels
  close → DB threads drain and exit → `Threads::join`.
- **Benchmark finding:** at this size the single HTTP thread is the bottleneck, not SQLite; a second
  reader adds RSS and contention. Keep 1 reader unless queries get heavy.
- **bcrypt** (sign-in, password change) runs on tokio's blocking pool behind a 1-permit semaphore, so
  the HTTP thread never blocks and guesses are serialised. JWT verification (~µs) stays on the HTTP thread.

## 4. Request lifecycle

### Role — every `/rest/v1` request (auth on)

`Caller::of` (server.rs) takes the token from `Authorization: Bearer`, else `apikey`, and
`Auth::role` verifies it → `Role::{Anon, Authenticated(uid), Service}` or `401 PGRST302/301/303`.
`Auth::grant` turns the role into a `Grant { policy: Option<Arc<Policy>>, uid }` that moves into the
DB job. `service_role` and auth off get `Grant::default()` (no policy: nothing is checked).

### Read — `GET /rest/v1/{table}`

1. `query::parse_query(raw)` → `Query { select, scopes, on_conflict, columns }`.
   `scopes[0]` is the root (filters, order, limit, offset); embedded scopes are keyed by path
   (`edition.language=eq.vi` → scope `["edition"]`).
2. `Headers::parse` → `Prefer` (return / count / resolution / missing=default), `Range`, single-object `Accept`.
3. On a reader: `sql::build_read(schema, table, query, range, count, limits, grant)` validates every
   name, checks `read` on the root and every embedded (and junction) table, ANDs `"owner" = ?uid` into
   each owner table's `WHERE` (so `!inner` and `count` see it too), and emits **one** statement:
   ```sql
   SELECT json_group_array(json(j)), count(*), <total|NULL>
   FROM (SELECT json_object(...) AS j FROM "t" AS t0 WHERE ... ORDER BY ... LIMIT ?n OFFSET ?m)
   ```
4. `Read::run` returns `Page { body, rows, total }`; `server::read_response` picks `200` / `206` /
   `416`, sets `Content-Range`, and for `vnd.pgrst.object+json` strips the outer `[ ]` (or `406`).

### Write — `POST` / `PATCH` / `DELETE`

1. `write::parse_payload(body)` scans the JSON **once** with a serde `Visitor` to collect keys and
   row count; values are never materialised in Rust. The raw body is bound as one parameter.
2. On the writer: `write::execute` opens an `IMMEDIATE` transaction and builds:
   - **Insert:** `INSERT INTO t (cols) SELECT <col ->> '$."col"'...> FROM json_each(?1) WHERE 1`
     — one statement for 1 or N rows. `columns=` (sent by supabase-js) wins over body keys;
     `Prefer: missing=default` uses column defaults for absent keys.
   - **Upsert:** `Prefer: resolution=merge-duplicates|ignore-duplicates` + optional `on_conflict=cols`
     (must match the PK or a full unique index) → `ON CONFLICT (...) DO UPDATE SET ... | DO NOTHING`.
   - **Update:** `UPDATE t AS t0 SET col = ?1 ->> '$."col"' ... WHERE <filters>`.
   - **Delete:** `DELETE FROM t AS t0 WHERE <filters>`.
   - UPDATE/DELETE **refuse** an empty filter (`21000`) and `limit`/`offset`.
3. `return=representation`: writes use `RETURNING json_array(<row key>)` (rowid, or the PK of a
   `WITHOUT ROWID` table), then re-read those rows with `sql::build_select(.., Rows::Keys(..))`, so
   `select=` with embeds works on writes. DELETE reads the rows (`Rows::All`) *before* deleting.
4. With a grant: `write` is required on the target. On an owner table UPDATE / DELETE AND
   `"owner" = ?uid` into their filter; INSERT fills a missing owner with `coalesce(value, ?2)` and a merge
   adds `DO UPDATE ... WHERE t."owner" = ?2`. Owner tables always use `RETURNING`, then
   `SELECT EXISTS (... key IN (<keys>) AND "owner" IS NOT ?2)` checks the written rows, and a merge that
   returned fewer rows than the body hit another user's row; either → rollback, `403 42501`. The
   representation re-read uses the same grant (DELETE's pre-read uses the write owner filter).
5. Build errors and single-object mismatches roll the transaction back.
   Status: insert `201`, others `200` with body or `204` without.

### Auth — `/auth/v1` (auth on only)

Every route first checks that `apikey` is a valid anon / service_role key. Sign-in reads the user on a
reader, runs bcrypt off-thread (an unknown email is checked against a dummy hash), then creates the
session and refresh token in one `IMMEDIATE` transaction on the writer. Refresh rotation, `PUT /user`
and logout are writer transactions too. Access-token claims and the user object are built by SQLite
(`json_object` over `auth_users` / `auth_sessions`); Rust only signs the claims text. Responses carry
`X-Supabase-Api-Version: 2024-01-01`; errors are GoTrue's `{code, error_code, msg}`.

## 5. Module details

### schema.rs — schema cache

- `introspect` reads everything in one read transaction via `pragma_table_list`, `pragma_table_info`,
  `pragma_index_list`/`_info` (full, non-partial unique indexes), `pragma_foreign_key_list`.
  Only `type = 'table'` in `main`, skipping `sqlite_*` and the auth tables (`auth_users`,
  `auth_sessions`, `auth_refresh_tokens`); FKs to them are dropped — **views are not exposed yet**.
- `Column.json` is true for declared `JSON`/`JSONB` or a `CHECK (json_valid(col))` in the `CREATE TABLE` text.
- `Affinity::from_decl_type` follows SQLite's affinity rules.
- `derive_relationships`: each FK yields a `ManyToOne` (child → parent) and a `OneToMany`
  (parent → children). A table whose FKs all sit inside its PK (e.g. `lineup`) is a **junction**:
  every ordered pair of those FKs yields a `ManyToMany` between the referenced tables.
- `Schema::relationship_to(from, target, hint, to_many)` resolves an embed. A hint is the FK
  column list (`area!parent_id`) or the junction table name; `!one`/`!many` force direction on self
  references. 2+ matches → `RelError::Ambiguous` → HTTP `300 PGRST201` with suggested hints.
- `SchemaCache` = `RwLock<Arc<Schema>>`. Every DB job calls `refresh`, which runs one cached
  `PRAGMA schema_version` and re-introspects only when it changed. Tables are sorted by name
  (binary-search lookup).

### query.rs — parser (pure)

- Supports `select` (`*`, `col`, `alias:col`, `[alias:]rel[!hint][!inner][!one|!many](...)` at any
  depth, `rel()` for filter-only embeds), filters `eq neq gt gte lt lte like ilike is in` with
  `not.`, logic trees `or=()`/`and=()` nested and negated, `order` (multi-column, `nullsfirst|last`),
  `limit`, `offset`, all optionally prefixed by an embed path; `on_conflict`, `columns`.
- Percent-decoding treats `+` as space (what `URLSearchParams` sends).
- **No `crate::` imports** — the fuzz crate compiles this file standalone (see §7).

### sql.rs — read builder

- `Builder` tracks bound params (`?N`), table aliases (`t0, t1, …`; junctions `jN`) and which
  embedded scopes were consumed (a filter on an embed not in `select` → `PGRST108`).
- Embeds become correlated subqueries: many-to-one `(SELECT json_object(..) .. LIMIT 1)`;
  one-to-many / many-to-many `(SELECT json_group_array(json(j)) FROM (SELECT .. ORDER .. LIMIT .. OFFSET ..))`;
  `!inner` adds `EXISTS (...)` to the parent's `WHERE`.
- Type mapping (`column_value`): INTEGER/REAL → number, TEXT → string, NULL → null,
  JSON columns via `json()`, BLOB → `\x` hex string (PostgreSQL `bytea` style). Writes invert this.
- `like` → `GLOB` (case-sensitive, as PostgreSQL), `ilike` → SQLite `LIKE`. Null ordering defaults
  to PostgreSQL's (last on ASC, first on DESC).
- `count=exact` adds `(SELECT count(*) FROM ... WHERE ...)` to the **same** statement, reusing params.
  `planned`/`estimated` are treated as exact (SQLite has no cheap estimate).
- Limits: root uses query `limit` > `Range` header > `--default-limit`; everything (including each
  embedded array) is clamped to `--max-limit`.
- Access: `Grant::owner(table, need)` returns `PermissionDenied` when the role's `Perm` is below
  `need` (or a configured owner column is missing from the table, or there is no uid), else the owner
  column that limits rows (none for reads with `read_all`). `Builder::access` turns it into
  `alias."owner" = ?N`, binding the uid once per statement. SQL text depends on the query shape and
  the role, never on the user.

### access.rs / auth.rs / jwt.rs — auth

- `Access::parse` resolves `"*"` and each entry into a sorted `Policy` per role; the owner rule is only
  on the `authenticated` policy. Refused at startup: unknown fields / values, `owner` on `"*"`,
  `owner_scope` without `owner`, `anon: "write"` on an owner table, a secret under 32 bytes.
  `Auth::check_schema` logs unknown tables / owner columns once per schema version.
- Tables are created with `CREATE TABLE IF NOT EXISTS` at server start (auth on) or by `rulite user`.
  Refresh tokens rotate (`parent` = old token, old one `revoked = 1`); a revoked token inside
  `refresh_token_reuse_interval` returns the session's newest active token, later it deletes the session.
- JWT: only `alg = HS256`, MAC compared in constant time (`verify_slice`), `exp` with 30 s leeway.

### server.rs — HTTP & errors

Routes: `GET /rest/v1[/]` → OpenAPI (`service_role` only when auth is on); `GET|POST|PATCH|DELETE
/rest/v1/{table}`; with auth on, `/auth/v1/*` (auth.rs); anything else → `404 PGRST125`.
Error body is always PostgREST's `{code, message, details, hint}`.

| Source | HTTP | `code` |
|---|---|---|
| Parse error | 400 | `PGRST100` |
| Unknown table | 404 | `PGRST205` |
| Unknown column | 400 | `42703` |
| No relationship / ambiguous | 400 / 300 | `PGRST200` / `PGRST201` |
| Filter on embed not selected | 400 | `PGRST108` |
| Bad body / unknown body column | 400 | `PGRST102` / `PGRST204` |
| `on_conflict` matches no unique key | 400 | `42P10` |
| UPDATE/DELETE without filter | 400 | `21000` |
| Single object, rows ≠ 1 | 406 | `PGRST116` |
| Offset past counted total | 416 | `PGRST103` |
| UNIQUE / PK violation | 409 | `23505` |
| FK violation | 409 | `23503` |
| NOT NULL / CHECK / STRICT type | 400 | `23502` / `23514` / `22P02` |
| Busy / locked | 503 | `55P03` |
| No key / bad token / expired token (auth on) | 401 | `PGRST302` / `PGRST301` / `PGRST303` |
| Role lacks the permission; owner set to another user | 401 (anon) / 403 | `42501` |
| DB thread gone | 503 | `PGRST000` |
| Anything else | 500 | `XX000` |

### openapi.rs

Built on demand from the `Schema`, cached in `App.openapi` as `(schema_version, Bytes)` and rebuilt
only when the version moves.

## 6. Invariants — do not break these

1. **No request identifier reaches SQL verbatim.** Every table/column/relationship is looked up in
   `Schema`, and SQL is written with the *schema's* name via `ident()` / `literal()`. Request values,
   aliases, limits and offsets are always bound `?N` params. The `build_read` fuzz target asserts this.
2. **Equal query shapes → identical SQL text**, so `prepare_cached` hits. Never inline values into SQL.
   (Note: rusqlite's default statement-cache capacity is 16 per connection; not tuned yet.)
3. **One statement per read**, JSON assembled by SQLite; no serde on the read hot path.
4. **Every write = one `IMMEDIATE` transaction on the single writer thread.** Bulk insert = one statement.
5. **Bounded work everywhere:** clamped limits, bounded channels, fixed thread count.
6. **Unfiltered UPDATE/DELETE are refused.**
7. **`query.rs`, `schema.rs`, `sql.rs` stay compilable by the fuzz crate:** they may only depend on
   each other, `rusqlite` and `std` (the fuzz targets mount them with `#[path]`).
8. **Every new dependency or feature must justify its RAM/CPU cost** (Design rule). Check binary size
   and idle RSS with `tests/bench.sh` when adding one.
9. **supabase-js compatibility is the spec.** Behaviour questions are settled by what PostgREST does
   and verified by a Node test through the real client.
10. **Access is checked in the builders, from a `Grant`.** Every table a statement touches goes through
    `Grant::owner` / `Builder::access`; the uid is a bound parameter; no SQL text comes from the config
    (owner columns are emitted with the schema's name). Without a JWT secret, behaviour is phase 1's.

## 7. Testing & tooling

| Layer | Command | What it covers |
|---|---|---|
| Unit (Rust) | `cargo test` | 98 tests: config, parser, SQL builder, grants / owner rows, writes, schema/relationships, JWT, sessions / refresh, error mapping, status codes |
| Integration | `cargo build --release && cd tests && npm install && npm test` | `run.mjs` rebuilds `novel.db`/`event.db`/`todo.db` from `.sql` in a temp dir, adds users with `rulite user add`, starts one release binary per DB on a free port (`todo.db` with auth), runs `*.test.mjs` |
| Fuzz | `cd fuzz && cargo +nightly fuzz run -a parse ...` / `build_read ...` | Parser never panics; built SQL only names schema identifiers under random grants, binds the uid, and SQLite prepares it |
| Budget | `tests/bench.sh [seconds] [readers] [auth]` | Binary size, idle/peak RSS, req/s, p50/p99 with `oha`; `auth` adds a JWT per request |

Sample schemas cover the relationship cases: `novel.db` is a 5-level one-to-many chain;
`event.db` has a self reference (`area.parent_id`), two tables referencing `area`, a many-to-many
(`sub_event ↔ artist` via `lineup`), a nullable FK with `ON DELETE SET NULL`, a JSON column and 0/1 booleans.

Adding a feature: unit-test the parser/builder in the module's `mod tests`, add a supabase-js case
in `tests/*.test.mjs`, add fuzz seeds/dict tokens if new syntax, re-run the bench if it touches the hot path.

## 8. Known gaps & phase 2 candidates

Out of scope so far (PLAN): realtime (`update_hook`, planned behind an off-by-default Cargo
feature), RPC / views-as-functions, storage, admin UI, multi-database; for auth: sign-up, magic link,
OTP, OAuth, MFA, policy SQL, column grants, asymmetric keys, config hot reload.

Gaps noticed in the current code:

- **Views** are not introspected (open question in PLAN: expose read-only?).
- **Auth is off without a JWT secret:** then any `apikey` / `Authorization` is accepted; bind defaults
  to `127.0.0.1:3000`. No CORS headers yet (browsers on another origin need a proxy).
- `count=planned|estimated` = exact; `return=headers-only` behaves like `minimal`.
- No request body size limit beyond axum's default; no request timeout.
- Statement cache capacity is rusqlite's default (16) — may thrash with many query shapes.
- `schema_version` is checked per job — DDL from another process is picked up, but a long-running
  read keeps its old snapshot until the next job.
- Embedded arrays get no default limit, only `--max-limit`.
- Single HTTP thread caps throughput (~13k req/s on the bench query); a multi-thread runtime would
  trade RSS for throughput.

Where later features would plug in:

| Feature | Likely touch points |
|---|---|
| Realtime | `db.rs` writer thread (`update_hook`), new bounded broadcast channel, new route; Cargo feature |
| Views | `schema.rs` introspect (`type = 'view'`, read-only flag), `write.rs` refuse, `openapi.rs` |
| RPC | new route + module; reuse `Builder` params and error mapping |
