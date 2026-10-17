# Rulite

A Supabase-compatible REST API for any SQLite database, written in Rust.

## Features

- REST API generated from the SQLite schema, compatible with [`supabase-js`](https://github.com/supabase/supabase-js)
- Supabase-style auth (email + password, GoTrue-compatible tokens), `anon` / `authenticated` / `service_role`
  roles, per-table `none | read | write` and owner rows; always on, see [AUTH.md](AUTH.md)
- TypeScript types as `supabase gen types typescript` prints them, so `createClient<Database>()` type-checks
  `from()`, `select()` with embeds, `insert()` and `update()`
- CORS for browser apps on other origins; optional built-in HTTPS (`--features tls`)

See [PLAN.md](PLAN.md) for the plan, [AUTH.md](AUTH.md) for auth & permissions and [ARCHITECTURE.md](ARCHITECTURE.md) for how it is built.

## Design rule

**Minimal resource footprint:** low memory usage and CPU overhead. Every dependency and feature must justify its cost.

- **Stack:** `rusqlite` (`bundled`) + `axum` (`default-features = false`) on a `current_thread` tokio runtime.
- **No heavy query builders/ORMs:** hand-rolled (or `winnow`) query-string parser, hand-written SQL with bound parameters.
- **Connections:** 1 writer + 1–2 readers on dedicated threads (channel-fed), no pool.
- **JSON:** built inside SQLite (`json_group_array(json_object(...))`) and passed through as-is, with no serde on the hot path.
- **SQLite PRAGMAs:** `journal_mode=WAL`, `synchronous=NORMAL`, `cache_size=-2000`, `temp_store=FILE`.
- **Caching:** `prepare_cached` for statements; schema introspected once, reloaded only when `PRAGMA schema_version` changes.
- **Bounded work:** default and max row limits on every request (e.g. 100 / 1000).
- **Optional features:** built-in TLS (`tls`) and, later, realtime (`update_hook`) behind Cargo features, off by default.
- **Release profile:** `lto = "fat"`, `codegen-units = 1`, `panic = "abort"`, `strip = true`.

## Usage

```sh
rulite keys --config app.json                     # anon and service_role keys
rulite app.db --config app.json                   # serve on 127.0.0.1:3000
```

Every server runs with auth: it needs a `jwt_secret` (in the config or `RULITE_JWT_SECRET`), and `anon` /
`authenticated` only see the tables the config grants them. See [AUTH.md](AUTH.md) for the config file.

### TypeScript types

The same `Database` type as `supabase gen types typescript`, from the running server (with the
`service_role` key: the types reveal the whole schema) or straight from the file:

```sh
curl -H "apikey: $SERVICE_KEY" http://127.0.0.1:3000/pg/generators/typescript > database.types.ts
rulite gen types app.db > database.types.ts
```

```ts
import { createClient } from '@supabase/supabase-js'
import type { Database } from './database.types'

const supabase = createClient<Database>('http://127.0.0.1:3000', ANON_KEY)
const { data } = await supabase.from('novel').select('title, edition(language)')  // typed rows
```

Per table: `Row`, `Insert` (optional when nullable, with a default or the rowid), `Update` and `Relationships`
(one per foreign key). INTEGER / REAL / NUMERIC columns are `number`, TEXT and BLOB (`\x` hex) `string`, JSON
columns `Json`, columns without a declared type `unknown`. The auth tables are left out.

### Deploy

**Recommended: a reverse proxy with HTTPS in front**, e.g. [Caddy](https://caddyserver.com), which gets and
renews certificates (ACME), speaks HTTP/2 and keeps TLS out of Rulite's process:

```
api.example.com {
	reverse_proxy 127.0.0.1:3000
}
```

```sh
rulite app.db --config app.json      # stays on 127.0.0.1:3000; set external_url to https://api.example.com
```

**CORS:** a browser app on another origin needs `--cors-origins` (or `cors_origins` in the config, or
`RULITE_CORS_ORIGINS`): exact origins, comma separated, or `*`. Preflights are answered before auth, and
`Content-Range` is exposed so `count` works.

```sh
rulite app.db --config app.json --cors-origins http://localhost:5173,https://app.example.com
```

**Built-in TLS** (supported, not recommended): for a LAN, a self-signed certificate or a quick deploy without
a proxy. Build with the `tls` feature (rustls with `ring`, +0.8 MB; the default build doesn't change) and pass
a PEM certificate chain and key (`tls_cert` / `tls_key` in the config, relative to it):

```sh
cargo build --release --features tls
rulite app.db --config app.json --bind 0.0.0.0:443 --tls-cert cert.pem --tls-key key.pem
```

It serves TLS 1.2 / 1.3 over HTTP/1.1. It does not get or renew certificates (restart after renewing them),
has no HTTP/2 and no client certificates; a proxy does all of that better. Without TLS, binding to a
non-loopback address logs a warning.

## Development

### Sample databases

```sh
sqlite3 sample/novel.db < sample/novel.sql   # then: rulite sample/novel.db --config sample/novel.json
sqlite3 sample/event.db < sample/event.sql   # then: rulite sample/event.db --config sample/event.json
sqlite3 sample/todo.db < sample/todo.sql     # then: rulite sample/todo.db --config sample/todo.json
rulite keys --config sample/todo.json        # one jwt_secret: the same keys work on all three
```

`novel.json` and `event.json` grant `anon` read and write on every table. `sample/*.types.ts` are their
TypeScript types (golden files for `cargo test`; `RULITE_BLESS=1 cargo test` rewrites them).

| Database | Tables | Relationship cases |
|---|---|---|
| `novel.db` | `novel → edition → volume → chapter → part` | Deep one-to-many chain (5 levels) |
| `event.db` | `area`, `event`, `sub_event`, `artist`, `lineup` | Self reference (`area.parent_id`: city → venue → stage), two tables referencing `area`, many-to-many (`sub_event ↔ artist` via `lineup`), nullable FK with `ON DELETE SET NULL`, JSON column (`artist.genres`), boolean as `0/1` |
| `todo.db` | `list`, `todo`, `tag`, `comment`, `audit` | Auth ([`todo.json`](sample/todo.json)): owner rows (`list.owner_id`, `todo.user_id` → `auth_users`), public `tag`, `comment` with `owner_scope: "write"`, closed `audit`. The 2 s refresh-token reuse interval is shortened for the tests. |

Dev only: ids are random 8-char hex. The `lineup` junction table uses a composite primary key instead.

### Testing

- **Unit tests (Rust):** `cargo test` for the query parser and SQL builder.
- **Integration tests (Node.js + `supabase-js`):** Rulite is tested through the official Supabase client, so passing tests mean real compatibility.

```sh
cargo build --release
cd tests
npm install
npm test        # node --test on fresh copies of sample/novel.db and sample/event.db
```

`tests/run.mjs` rebuilds the three databases from their `.sql` files in a temp directory (with the built-in `node:sqlite`, so Node ≥ 22.13), adds the users alice, bob and carol (password `secret1`) to `todo.db` with `rulite user add`, reads the keys from `rulite keys`, starts one `target/release/rulite` per database on a free port with its `sample/<name>.json` (novel also with `--cors-origins`), runs every `*.test.mjs` one file at a time and stops the servers afterwards. Tests get `RULITE_NOVEL_URL`, `RULITE_EVENT_URL`, `RULITE_TODO_URL` (`RULITE_URL` is the novel server), `RULITE_ANON_KEY`, `RULITE_SERVICE_KEY` (valid on all three), `RULITE_TODO_CONFIG`, `RULITE_CORS_ORIGINS` and `RULITE_BIN`, which also overrides the binary. Extra arguments go to `node --test`, e.g. `npm test -- --test-reporter=spec`. `auth.test.mjs` covers `/auth/v1`, `access.test.mjs` roles, table permissions and owner rows, `startup.test.mjs` the required secret and closed tables, `types.test.mjs` the type export and `tsc --noEmit` on `tests/types/usage.ts`, `cors.test.mjs` CORS. `tls.test.mjs` runs a build with the `tls` feature over HTTPS (the test certificate in `tests/tls/` is trusted through `NODE_EXTRA_CA_CERTS`, set by `npm test`); build it first, or it is skipped:

```sh
cargo build --release --features tls --target-dir target/tls     # RULITE_TLS_BIN overrides the path
```

- **Fuzzing (nightly + [`cargo-fuzz`](https://github.com/rust-fuzz/cargo-fuzz)):** two targets in `fuzz/`. `parse` feeds the query-string and header parsers (no panics); `build_read` builds SQL for parsed queries against both sample schemas under a random grant (second input byte: permissions, owner columns, uid) and asserts every quoted identifier and literal in it is a schema name, that a built read had `read` on its root and bound the uid, and that SQLite prepares it.

```sh
cd fuzz
mkdir -p corpus/parse corpus/build_read
cargo +nightly fuzz run -a parse      corpus/parse      seeds/parse      -- -dict=query.dict -max_len=512
cargo +nightly fuzz run -a build_read corpus/build_read seeds/build_read -- -dict=query.dict -max_len=512
```

`-a` turns on overflow checks; the first runs found `Range: 0-18446744073709551615` overflowing in `parse_range`.

Example test:

```js
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { createClient } from '@supabase/supabase-js'

// sample/novel.json grants anon read + write on every table.
const supabase = createClient(process.env.RULITE_URL, process.env.RULITE_ANON_KEY)

test('nested select novel -> part', async () => {
  const { data, error } = await supabase
    .from('novel')
    .select('title, edition(language, volume(number, chapter(title, part(content))))')
    .eq('title', 'The Lantern Road')
    .single()

  assert.equal(error, null)
  assert.equal(data.edition.length, 2)
})
```

### Budget

`tests/bench.sh [seconds] [readers] [auth|tls]` (needs `oha`): release build on a fresh `novel.db`, the nested `novel → edition → volume → chapter → part` query (1.2 KB response), 10 s per run. The server always runs with a config and every request carries the anon key, so each one verifies an HS256 JWT and checks the grant on all five tables. With `auth` the requests carry a signed-in user's access token instead; with `tls` they go over HTTPS to the `--features tls` build (`target/tls`). The script prints the size of both binaries.

Measured 2026-10-17 on an Apple M3 (8 cores), macOS, `oha` on the same machine (phase 1 columns from before auth; phase 3 runs 5 s each):

| | 1 reader (default) | 2 readers | Phase 2, auth off | Phase 2, authenticated | Phase 3, anon key | Phase 3, `tls` build over HTTPS |
|---|---|---|---|---|---|---|
| Binary size | 2.7 MB | | 2.9 MB | 2.9 MB | 3.0 MB | 3.8 MB |
| Idle RSS | 3.7 MB | | 3.9 MB | 4.5 MB ¹ | 4.1 MB | 4.7 MB |
| Peak RSS under load | 7.3 MB | 8.5 MB | 7.4 MB | 8.7 MB | 7.9 MB | 10.3 MB |
| 1 connection: req/s, p50 / p99 | 8,630, 0.11 / 0.15 ms | 8,780, 0.11 / 0.13 ms | 8,690, 0.11 / 0.14 ms | 8,650, 0.11 / 0.13 ms | 8,720, 0.11 / 0.15 ms | 8,240, 0.12 / 0.14 ms |
| 10 connections: req/s, p50 / p99 | 13,090, 0.77 / 0.88 ms | 10,370, 0.97 / 1.71 ms | 12,960, 0.77 / 0.82 ms | 12,910, 0.78 / 0.81 ms | 13,020, 0.77 / 0.82 ms | 12,600, 0.80 / 0.88 ms |
| 50 connections: req/s, p50 / p99 | 12,920, 3.87 / 4.07 ms | 10,360, 4.79 / 9.67 ms | 12,800, 3.91 / 4.04 ms | 12,690, 3.94 / 4.61 ms | 12,860, 3.91 / 4.04 ms | 12,630, 3.98 / 4.10 ms |

¹ After the sign-in that fetched the token: the blocking thread that ran bcrypt lingers for 10 s.

The default build grew by 49 KB in phase 3 (type generator, CORS); the TLS crates add 0.8 MB to the `tls` build only. Keep-alive connections pay the TLS handshake once, so HTTPS costs ~3% of throughput here.

At this size the single-threaded HTTP runtime is the bottleneck, not SQLite, so a second reader only adds RSS and contention; keep the default of 1 unless queries get heavy. Verifying a JWT per request costs under 1% of throughput.
