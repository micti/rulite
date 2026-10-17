# Rulite — Plan

**Goal:** a Supabase/PostgREST-compatible REST API over any SQLite file, with a minimal memory and CPU footprint.
How it is built: [ARCHITECTURE.md](ARCHITECTURE.md). Auth behaviour: [AUTH.md](AUTH.md).

## Target API (examples on `sample/novel.db`)

```http
GET    /rest/v1/novel?select=id,title&status=eq.completed&order=title.asc&limit=10
GET    /rest/v1/novel?select=title,edition(language,volume(number,chapter(title,part(content))))
GET    /rest/v1/part?select=content,chapter(title,volume(title))&word_count=gte.8
GET    /rest/v1/novel?or=(genre.eq.fantasy,status.eq.hiatus)
GET    /rest/v1/event?select=name,area(name,parent:area!parent_id(name)),sub_event(title,area(name),artist(name))
GET    /rest/v1/artist?select=name,sub_event(title,event(name))&country=eq.VN
GET    /rest/v1/area?select=name,children:area!parent_id!many(name,capacity)&kind=eq.venue
POST   /rest/v1/novel                      Prefer: return=representation
POST   /rest/v1/novel?on_conflict=title    Prefer: resolution=merge-duplicates
PATCH  /rest/v1/chapter?id=eq.ff2a0f33
DELETE /rest/v1/part?chapter_id=eq.ff2a0f33
GET    /rest/v1/                           -> OpenAPI document
```

## Phase 1 — REST API (done)

- [x] **M0 Skeleton:** CLI args, 1 writer + N reader threads, axum on a `current_thread` runtime.
- [x] **M1 Schema cache:** tables, columns, unique keys, foreign keys and relationships (many-to-one,
  one-to-many, many-to-many, self reference); reloaded when `schema_version` changes.
- [x] **M2 Query parser:** `select` with nested embeds, filters, `or`/`and`/`not`, `order`, `limit`,
  `offset`, `Prefer` / `Range` / `Accept` headers.
- [x] **M3 Reads:** one SQL statement per request, JSON built by SQLite, statement cache.
- [x] **M4 Writes:** insert, bulk insert, upsert, update, delete; each request is one transaction.
- [x] **M5 Responses:** `count`, single-object mode, PostgREST errors, OpenAPI document.
- [x] **M6 Quality:** supabase-js integration tests, benchmark, parser fuzzing.

## Phase 2 — Auth & permissions (done)

- [x] **M7 Config & CLI:** JSON config, `rulite user add|passwd|delete|list`, `rulite keys`.
- [x] **M8 Tokens:** bcrypt passwords, HS256 JWT, rotating refresh tokens, same format as Supabase.
- [x] **M9 `/auth/v1`:** sign in, refresh, get/update user, sign out; sign-up is disabled.
- [x] **M10 Roles:** `anon`, `authenticated`, `service_role` with `none | read | write` per table.
- [x] **M11 Owner rows:** an owner column limits each user to their own rows.
- [x] **M12 Quality:** `sample/todo.db`, auth and access tests, fuzzing with grants, benchmark with auth.

Without a JWT secret, Rulite behaves exactly as in phase 1.

## Not planned yet

- Realtime (`update_hook`), RPC, storage, admin UI, multiple databases.
- Auth: sign-up, magic link, OTP, OAuth, MFA, policy SQL, column grants, asymmetric keys.

## Open questions

- Views: expose them as read-only resources?
