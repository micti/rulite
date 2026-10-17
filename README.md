# Rulite

A Supabase-compatible REST API for any SQLite database, written in Rust.

## Features

- REST API generated from the SQLite schema, compatible with [`supabase-js`](https://github.com/supabase/supabase-js)
- Supabase-style auth (email + password, GoTrue-compatible tokens), `anon` / `authenticated` / `service_role`
  roles, per-table `none | read | write` and owner rows; see [AUTH.md](AUTH.md)

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
- **Optional features:** realtime (`update_hook`) behind a Cargo feature, off by default, with a small bounded channel.
- **Release profile:** `lto = "fat"`, `codegen-units = 1`, `panic = "abort"`, `strip = true`.

## Development

### Sample databases

```sh
sqlite3 sample/novel.db < sample/novel.sql
sqlite3 sample/event.db < sample/event.sql
sqlite3 sample/todo.db < sample/todo.sql     # then: rulite sample/todo.db --config sample/todo.json
```

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

`tests/run.mjs` rebuilds the three databases from their `.sql` files in a temp directory (with the built-in `node:sqlite`, so Node ≥ 22.13), adds the users alice, bob and carol (password `secret1`) to `todo.db` with `rulite user add`, reads the keys from `rulite keys`, starts one `target/release/rulite` per database on a free port (`todo.db` with `--config sample/todo.json`, the others without auth), runs every `*.test.mjs` one file at a time and stops the servers afterwards. Tests get `RULITE_NOVEL_URL`, `RULITE_EVENT_URL`, `RULITE_TODO_URL` (`RULITE_URL` is the novel server), `RULITE_ANON_KEY`, `RULITE_SERVICE_KEY` and `RULITE_TODO_CONFIG`; `RULITE_BIN` overrides the binary. Extra arguments go to `node --test`, e.g. `npm test -- --test-reporter=spec`. `auth.test.mjs` covers `/auth/v1`, `access.test.mjs` roles, table permissions and owner rows.

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

// Phase 1 has no auth: any key works.
const supabase = createClient(process.env.RULITE_URL, 'dev-key')

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

`tests/bench.sh [seconds] [readers] [auth]` (needs `oha`): release build on a fresh `novel.db`, the nested `novel → edition → volume → chapter → part` query (1.2 KB response), 10 s per run. With `auth`, the server runs with a JWT secret and every request carries a signed-in user's access token, so each one verifies an HS256 JWT and checks the grant on all five tables.

Measured 2026-10-17 on an Apple M3 (8 cores), macOS, `oha` on the same machine (phase 1 columns from before auth):

| | 1 reader (default) | 2 readers | Phase 2, auth off | Phase 2, authenticated |
|---|---|---|---|---|
| Binary size | 2.7 MB | | 2.9 MB | 2.9 MB |
| Idle RSS | 3.7 MB | | 3.9 MB | 4.5 MB ¹ |
| Peak RSS under load | 7.3 MB | 8.5 MB | 7.4 MB | 8.7 MB |
| 1 connection: req/s, p50 / p99 | 8,630, 0.11 / 0.15 ms | 8,780, 0.11 / 0.13 ms | 8,690, 0.11 / 0.14 ms | 8,650, 0.11 / 0.13 ms |
| 10 connections: req/s, p50 / p99 | 13,090, 0.77 / 0.88 ms | 10,370, 0.97 / 1.71 ms | 12,960, 0.77 / 0.82 ms | 12,910, 0.78 / 0.81 ms |
| 50 connections: req/s, p50 / p99 | 12,920, 3.87 / 4.07 ms | 10,360, 4.79 / 9.67 ms | 12,800, 3.91 / 4.04 ms | 12,690, 3.94 / 4.61 ms |

¹ After the sign-in that fetched the token: the blocking thread that ran bcrypt lingers for 10 s.

At this size the single-threaded HTTP runtime is the bottleneck, not SQLite, so a second reader only adds RSS and contention; keep the default of 1 unless queries get heavy. Verifying a JWT per request costs under 1% of throughput.
