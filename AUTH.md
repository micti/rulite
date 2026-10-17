# Rulite — Auth & permissions

Supabase-style auth for a dev/test server: supabase-js signs in with email + password and every
`/rest/v1` request runs as one of three roles. Each table grants each role `none`, `read` or `write`,
and can limit logged-in users to their own rows through an owner column.

Auth is always on: `rulite <db>` needs a JWT secret to start, every request needs a key, and `anon` and
`authenticated` see only the tables the config grants them.

## Quick start

```sh
# 1. secret (≥ 32 chars) and table permissions
cat > rulite.json <<'EOF'
{
  "jwt_secret": "change-me-to-a-long-random-string-0123456789",
  "tables": {
    "todo": { "authenticated": "write", "owner": "user_id" }
  }
}
EOF

# 2. keys for clients (anon = public key, service_role = private key)
rulite keys --config rulite.json

# 3. users (prompts for email and password; the account is active at once)
rulite user add app.db

# 4. serve
rulite app.db --config rulite.json
```

```js
import { createClient } from '@supabase/supabase-js'

const supabase = createClient('http://127.0.0.1:3000', ANON_KEY)
await supabase.auth.signInWithPassword({ email: 'alice@example.com', password: 'secret1' })
const { data } = await supabase.from('todo').select('*')   // only alice's todos
```

## Roles and keys

| Role | Sent by the client | Who uses it | Access |
|---|---|---|---|
| `anon` | the **anon key** (public, safe in a browser) | visitors not signed in | what the table grants `anon` |
| `authenticated` | the user's access token after sign-in | signed-in users | what the table grants `authenticated`, own rows only on owner tables |
| `service_role` | the **service_role key** (private, server side only) | seed scripts, admin jobs, tests | every table, every row, no checks |

The OpenAPI document (`GET /rest/v1/`) and the TypeScript types (`GET /pg/generators/typescript`, see
[README](README.md#typescript-types)) reveal the whole schema, so they are served to `service_role` only; `anon`
gets `401` and `authenticated` `403`, both with code `42501`.

`rulite keys` prints both keys. Like Supabase's (legacy) API keys they are HS256 JWTs signed with `jwt_secret`,
with claims `{"iss": "supabase", "ref": "rulite", "role": "anon" | "service_role", "iat", "exp"}` and valid
for 10 years. Changing the secret invalidates every key and token. supabase-js sends the key you pass to `createClient` as `apikey`.
After sign-in it sends the user's access token as `Authorization: Bearer`, which decides the role.

## Users (command line)

There is no sign-up endpoint. The operator manages users on the server:

```sh
rulite user add <db>              # asks email, password (twice, hidden); account is confirmed
rulite user passwd <db> <email>   # asks a new password; signs the user out everywhere
rulite user delete <db> <email>   # removes the user and their sessions (asks to confirm on a terminal)
rulite user list <db>             # id, email, created_at, last_sign_in_at
```

The commands work while the server runs. In scripts, pipe the answers:
`printf 'alice@example.com\nsecret1\nsecret1\n' | rulite user add app.db` (prompts are shown only on a
terminal; piped, `user delete` doesn't ask). Pass `--config` so `user add` / `passwd` use your
`password_min_length`. Emails are stored lowercase and matched without case.

Users live in the same database file, in `auth_users` (plus `auth_sessions`, `auth_refresh_tokens`), with
the same columns as GoTrue's `auth.users`, `auth.sessions`, `auth.refresh_tokens` (a subset). These
tables are created at server start (or by the first `rulite user` command) and are not exposed by `/rest/v1`. A user's `id` is a UUID string, so
app tables can reference it: `user_id TEXT REFERENCES auth_users(id) ON DELETE CASCADE`.

## Client auth API

| supabase-js | Works | Notes |
|---|---|---|
| `auth.signInWithPassword({ email, password })` | yes | wrong email or password → `invalid_credentials` |
| `auth.getUser()`, `auth.getSession()` | yes | |
| `auth.refreshSession()` / auto refresh | yes | refresh tokens rotate on every use |
| `auth.updateUser({ password, email, data })` | yes | applied directly (no confirmation mail) |
| `auth.signOut({ scope })` | yes | `global` (default), `local`, `others` |
| `auth.signUp()` | no | `signup_disabled`; add users with `rulite user add` |
| magic link, OTP, phone, OAuth, MFA, anonymous sign-in | no | |

### Tokens

Both tokens follow Supabase (GoTrue), so code and tools that decode them work unchanged.

The session is the same object GoTrue returns:
`{access_token, token_type: "bearer", expires_in, expires_at, refresh_token, user}`.

**`access_token`** is an HS256 JWT signed with `jwt_secret`, header `{"alg": "HS256", "typ": "JWT"}`:

```json
{
  "iss": "http://127.0.0.1:3000/auth/v1",
  "sub": "8a4e…-uuid",
  "aud": "authenticated",
  "exp": 1760000000,
  "iat": 1759996400,
  "email": "alice@example.com",
  "phone": "",
  "app_metadata": { "provider": "email", "providers": ["email"] },
  "user_metadata": {},
  "role": "authenticated",
  "aal": "aal1",
  "amr": [{ "method": "password", "timestamp": 1759996400 }],
  "session_id": "3f1c…-uuid",
  "is_anonymous": false
}
```

**`refresh_token`** is an opaque random string of 12 lowercase letters and digits (e.g. `k3v9x2m7qa1d`),
not a JWT. As in GoTrue:
- Each refresh returns a new one and revokes the old one.
- A revoked token used again within `refresh_token_reuse_interval` (10 s) returns the current token, so
  parallel tabs don't sign each other out.
- Used again later, the whole session is revoked (`refresh_token_already_used`).
- A session lives until sign-out, password change or user deletion.

A signed-out session's access token stays valid until it expires (`jwt_exp`, default 1 hour), as in Supabase.

## Config file

`rulite <db> --config rulite.json` (or `RULITE_CONFIG`). CLI flags and env vars override the file;
`RULITE_JWT_SECRET` overrides `jwt_secret`, so the secret can stay out of the file. Without a config,
`RULITE_JWT_SECRET` alone starts a server where only the `service_role` key works.

```json
{
  "jwt_secret": "change-me-to-a-long-random-string-0123456789",
  "jwt_exp": 3600,
  "refresh_token_reuse_interval": 10,
  "password_min_length": 6,
  "tables": {
    "*":       { "authenticated": "read" },
    "tag":     { "anon": "read" },
    "list":    { "authenticated": "write", "owner": "owner_id" },
    "todo":    { "authenticated": "write", "owner": "user_id" },
    "comment": { "anon": "read", "authenticated": "write", "owner": "author_id", "owner_scope": "write" }
  }
}
```

| Key | Default | Meaning |
|---|---|---|
| `jwt_secret` | — | HS256 secret, at least 32 characters. Required: the server refuses to start without it. |
| `jwt_exp` | `3600` | Access token lifetime in seconds. |
| `external_url` | `http://<bind>` | Public base URL; the token `iss` is `<external_url>/auth/v1` (Supabase's `API_EXTERNAL_URL`). |
| `refresh_token_reuse_interval` | `10` | Seconds an already-used refresh token is still accepted (parallel tabs refreshing at once). |
| `password_min_length` | `6` | Applies to `user add`, `user passwd` and `updateUser({ password })`. |
| `tables` | `{}` | Permissions per table, see below. Without any grant only `service_role` works (logged at startup). |
| `cors_origins` | none | Origins a browser app may call from: `["http://localhost:5173", …]` or `"*"` (`--cors-origins`, `RULITE_CORS_ORIGINS`). See [README › Deploy](README.md#deploy). |
| `tls_cert`, `tls_key` | none | PEM certificate chain and private key for built-in HTTPS, relative to the config file (`--tls-cert` / `--tls-key`, `RULITE_TLS_CERT` / `RULITE_TLS_KEY`). Needs a build with `--features tls`. |

## Table permissions

Each entry in `tables` may set:

| Field | Values | Default |
|---|---|---|
| `anon` | `none` · `read` · `write` | from `"*"`, else `none` |
| `authenticated` | `none` · `read` · `write` | from `"*"`, else `none` |
| `owner` | column name holding the owner's user id | no owner column |
| `owner_scope` | `all` (own rows for read and write) · `write` (read all rows, write own) | `all` |

- `"*"` sets the defaults for tables not listed or for roles a table entry leaves out. **A table nobody
  granted is closed** to `anon` and `authenticated`.
  `"*"` takes only `anon` and `authenticated`; `owner` and `owner_scope` are per table.
- `write` includes `read`. It allows `insert`, `upsert`, `update` and `delete`.
- `service_role` ignores this section.

| Permission | `select` | `insert` / `upsert` / `update` / `delete` |
|---|---|---|
| `none` | `42501` error | `42501` error |
| `read` | allowed | `42501` error |
| `write` | allowed | allowed |

Embedded resources are checked too: `select('*, todo(*)')` needs `read` on `todo` as well (and on the
junction table of a many-to-many embed).

## Owner rows

With `"owner": "user_id"`, a signed-in user only works on rows where `user_id` equals their user id:

| Operation | Behaviour |
|---|---|
| `select` | only own rows; also inside embeds, `!inner` and `count`. Other rows are not errors, they just don't appear. With `owner_scope: "write"`, all rows are readable. |
| `insert` | `user_id` missing from a row (or `null`) → filled with the user's id. Set to another id → `42501`, nothing written. |
| `update` | only own rows are touched; others are skipped (`0 rows`, no error). Changing `user_id` to another id → `42501`. |
| `delete` | only own rows are deleted; others are skipped. |
| `upsert` | a conflicting row owned by someone else → `42501`, nothing written. |
| `.select()` after a write | returns only rows the user may read. |

Rules for the owner column:

- Use a `TEXT` column holding `auth_users.id`, ideally `REFERENCES auth_users(id)`.
- The owner check applies to `authenticated` only. `anon` has no user id, so on an owner table it may get
  `read` (all rows, e.g. public comments) but not `write`. That combination is refused at startup.
- `service_role` sees and writes every row and can set `user_id` to anyone, e.g. to seed data for a user.

Typical setups:

| Use case | Entry |
|---|---|
| Private data per user (todos, notes) | `{ "authenticated": "write", "owner": "user_id" }` |
| Public posts, authors edit their own | `{ "anon": "read", "authenticated": "write", "owner": "author_id", "owner_scope": "write" }` |
| Shared reference data (tags, countries) | `{ "anon": "read", "authenticated": "read" }` — edit with the service_role key |
| Open table for quick tests | `{ "anon": "write", "authenticated": "write" }` |

## Errors

REST errors use PostgREST's `{code, message, details, hint}`; auth errors use GoTrue's
`{code, error_code, msg}`. supabase-js exposes both as `error.code`.

| When | HTTP | Code |
|---|---|---|
| No key or token (`/rest/v1`) | 401 | `PGRST302` |
| Malformed token or bad signature | 401 | `PGRST301` |
| Expired token | 401 | `PGRST303` |
| Role lacks the permission (anon / authenticated), incl. OpenAPI and types | 401 / 403 | `42501` |
| Owner column set to another user | 403 | `42501` |
| Wrong email or password | 400 | `invalid_credentials` |
| Unknown, used or revoked refresh token | 400 | `refresh_token_not_found` / `refresh_token_already_used` |
| Session signed out, user deleted | 403 | `session_not_found` / `user_not_found` |
| New password too short | 422 | `weak_password` |
| `signUp` | 422 | `signup_disabled` |

## Limits

- One HS256 secret; no asymmetric keys, no key rotation without signing everyone out.
- Permissions are per table and role, not per column. The only row rule is the owner column.
- Config is read at startup; restart after changing it.
- Built for development and testing. Put it behind an HTTPS proxy (Caddy, see [README › Deploy](README.md#deploy))
  before exposing it on a VPS; built-in TLS (`--features tls`) works but is not the recommended setup.
