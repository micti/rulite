# Rulite — agent guide

PostgREST/Supabase-compatible REST API over any SQLite file, in Rust. Phase 1 (M0–M6) and phase 2
(M7–M12: auth, roles, table permissions, owner rows) are done.

Read first: [ARCHITECTURE.md](ARCHITECTURE.md) (stack, threads, request flow, invariants),
[README.md](README.md) (design rule, testing, budget), [PLAN.md](PLAN.md) (phases & tasks), [AUTH.md](AUTH.md) (auth & permissions spec).

## Commands

```sh
cargo fmt && cargo clippy --all-targets                        # format (rustfmt.toml) + lint (Cargo.toml [lints])
cargo test                                                     # unit tests (parser, builders, errors)
cargo build --release && (cd tests && npm install && npm test) # supabase-js integration tests
cargo run -- sample/novel.db                                   # serve on 127.0.0.1:3000
cargo run -- sample/todo.db --config sample/todo.json          # serve with auth (sqlite3 sample/todo.db < sample/todo.sql)
tests/bench.sh [secs] [readers] [auth]                         # RSS + latency budget (needs oha)
```

## Rules

- **Minimal resource footprint.** No new crate or feature without a clear cost/benefit; no ORM or query builder.
- **Never put request text into SQL.** Resolve names through `Schema`, emit them with `ident()`/`literal()`;
  bind every value as `?N`. Keep SQL text identical for identical query shapes (statement cache).
- Reads: one statement, JSON built by SQLite. Writes: one `IMMEDIATE` transaction on the writer thread.
- `src/query.rs`, `src/schema.rs`, `src/sql.rs` must not import other crate modules (fuzz targets
  compile them standalone via `#[path]`). Grants (`Grant`, `Policy`, `Rule`, `Perm`) are plain data in `sql.rs`.
- Access checks go through `Grant::owner` / `Builder::access`; the uid is always a bound `?N`.
- Behaviour follows PostgREST; errors use its `{code, message, details, hint}` shape and codes.
- Every user-visible feature gets a Rust unit test **and** a `tests/*.test.mjs` case through supabase-js.
- Match surrounding style: short `///` docs stating what, not how; no extra comments.
