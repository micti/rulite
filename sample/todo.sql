-- Dev schema for phase 2 (auth & permissions), served with sample/todo.json
-- Build: sqlite3 sample/todo.db < sample/todo.sql
-- Serve: rulite sample/todo.db --config sample/todo.json
--   list, todo   private per user (owner columns owner_id / user_id)
--   tag          public reference data
--   comment      readable by everyone, authors edit their own (owner_scope "write")
--   audit        granted to nobody: closed to anon and authenticated
-- auth_users is created by rulite (`rulite user add` or the server with auth on),
-- so list / todo / comment rows are added through the API once users exist.

PRAGMA foreign_keys = ON;

CREATE TABLE list (
  id         TEXT PRIMARY KEY DEFAULT (lower(hex(randomblob(4)))),
  owner_id   TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
  name       TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
) STRICT;

CREATE TABLE todo (
  id         TEXT PRIMARY KEY DEFAULT (lower(hex(randomblob(4)))),
  list_id    TEXT NOT NULL REFERENCES list(id) ON DELETE CASCADE,
  user_id    TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
  title      TEXT NOT NULL,
  done       INTEGER NOT NULL DEFAULT 0 CHECK (done IN (0, 1)),
  created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
) STRICT;

CREATE TABLE tag (
  id   TEXT PRIMARY KEY DEFAULT (lower(hex(randomblob(4)))),
  name TEXT NOT NULL UNIQUE
) STRICT;

CREATE TABLE comment (
  id         TEXT PRIMARY KEY DEFAULT (lower(hex(randomblob(4)))),
  tag_id     TEXT NOT NULL REFERENCES tag(id) ON DELETE CASCADE,
  author_id  TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
  body       TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
) STRICT;

CREATE TABLE audit (
  id     INTEGER PRIMARY KEY,
  tag_id TEXT REFERENCES tag(id) ON DELETE SET NULL,
  note   TEXT NOT NULL
) STRICT;

INSERT INTO tag (id, name) VALUES ('a1b2c3d4', 'home'), ('b2c3d4e5', 'work'), ('c3d4e5f6', 'urgent');
INSERT INTO audit (tag_id, note) VALUES ('a1b2c3d4', 'seeded');
