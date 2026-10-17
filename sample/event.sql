-- Sample data for development: events, sub events, artists, areas
-- Build: sqlite3 sample/event.db < sample/event.sql
-- DEV ONLY: ids are random 8-char hex (32 bits) from randomblob(); not for production.
--
--   area ─< area            (self reference: city > venue > stage)
--   area ─< event           (venue of the event)
--   event ─< sub_event >─ area   (stage of the sub event)
--   sub_event ─< lineup >─ artist   (many-to-many)

PRAGMA foreign_keys = ON;

DROP TABLE IF EXISTS lineup;
DROP TABLE IF EXISTS artist;
DROP TABLE IF EXISTS sub_event;
DROP TABLE IF EXISTS event;
DROP TABLE IF EXISTS area;

CREATE TABLE area (
  id         TEXT    PRIMARY KEY NOT NULL DEFAULT (lower(hex(randomblob(4)))) CHECK (length(id) = 8),
  parent_id  TEXT    REFERENCES area(id) ON DELETE CASCADE,
  name       TEXT    NOT NULL UNIQUE,
  kind       TEXT    NOT NULL CHECK (kind IN ('city', 'venue', 'stage')),
  capacity   INTEGER
) STRICT;
CREATE INDEX area_parent_id ON area(parent_id);

CREATE TABLE event (
  id         TEXT    PRIMARY KEY NOT NULL DEFAULT (lower(hex(randomblob(4)))) CHECK (length(id) = 8),
  area_id    TEXT    NOT NULL REFERENCES area(id),
  name       TEXT    NOT NULL UNIQUE,
  starts_on  TEXT    NOT NULL,
  ends_on    TEXT    NOT NULL,
  is_public  INTEGER NOT NULL DEFAULT 1 CHECK (is_public IN (0, 1)),
  CHECK (ends_on >= starts_on)
) STRICT;
CREATE INDEX event_area_id ON event(area_id);

CREATE TABLE sub_event (
  id         TEXT    PRIMARY KEY NOT NULL DEFAULT (lower(hex(randomblob(4)))) CHECK (length(id) = 8),
  event_id   TEXT    NOT NULL REFERENCES event(id) ON DELETE CASCADE,
  area_id    TEXT    REFERENCES area(id) ON DELETE SET NULL,
  title      TEXT    NOT NULL,
  starts_at  TEXT    NOT NULL,
  ends_at    TEXT    NOT NULL,
  price      REAL,
  UNIQUE (event_id, title),
  CHECK (ends_at > starts_at)
) STRICT;
CREATE INDEX sub_event_area_id ON sub_event(area_id);

CREATE TABLE artist (
  id         TEXT    PRIMARY KEY NOT NULL DEFAULT (lower(hex(randomblob(4)))) CHECK (length(id) = 8),
  name       TEXT    NOT NULL UNIQUE,
  country    TEXT,
  genres     TEXT    NOT NULL DEFAULT '[]' CHECK (json_valid(genres))   -- JSON array
) STRICT;

-- Junction table: composite primary key, no own id.
CREATE TABLE lineup (
  sub_event_id   TEXT    NOT NULL REFERENCES sub_event(id) ON DELETE CASCADE,
  artist_id      TEXT    NOT NULL REFERENCES artist(id) ON DELETE CASCADE,
  role           TEXT    NOT NULL DEFAULT 'performer' CHECK (role IN ('headliner', 'performer', 'guest', 'host')),
  billing_order  INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (sub_event_id, artist_id)
) STRICT, WITHOUT ROWID;
CREATE INDEX lineup_artist_id ON lineup(artist_id);

-- Seed rows get random ids; children find their parent by natural key.
INSERT INTO area (name, kind, capacity) VALUES
  ('Ho Chi Minh City', 'city', NULL),
  ('Da Nang',          'city', NULL);

WITH s(parent, name, kind, capacity) AS (VALUES
  ('Ho Chi Minh City', 'Riverside Park',  'venue', 20000),
  ('Ho Chi Minh City', 'Opera House',     'venue', 500),
  ('Da Nang',          'Beach Arena',     'venue', 8000))
INSERT INTO area (parent_id, name, kind, capacity)
SELECT p.id, s.name, s.kind, s.capacity FROM s JOIN area p ON p.name = s.parent;

WITH s(parent, name, kind, capacity) AS (VALUES
  ('Riverside Park', 'Main Stage',   'stage', 15000),
  ('Riverside Park', 'Garden Stage', 'stage', 3000),
  ('Opera House',    'Grand Hall',   'stage', 500),
  ('Beach Arena',    'Sand Stage',   'stage', 8000))
INSERT INTO area (parent_id, name, kind, capacity)
SELECT p.id, s.name, s.kind, s.capacity FROM s JOIN area p ON p.name = s.parent;

WITH s(venue, name, starts_on, ends_on, is_public) AS (VALUES
  ('Riverside Park', 'Saigon Sound Fest 2026', '2026-12-05', '2026-12-06', 1),
  ('Opera House',    'Winter Strings',         '2026-12-20', '2026-12-20', 1),
  ('Beach Arena',    'Da Nang Waves',          '2027-04-30', '2027-05-02', 0))
INSERT INTO event (area_id, name, starts_on, ends_on, is_public)
SELECT a.id, s.name, s.starts_on, s.ends_on, s.is_public FROM s JOIN area a ON a.name = s.venue;

WITH s(event, stage, title, starts_at, ends_at, price) AS (VALUES
  ('Saigon Sound Fest 2026', 'Main Stage',   'Opening Night',     '2026-12-05 18:00', '2026-12-05 23:00', 45.0),
  ('Saigon Sound Fest 2026', 'Garden Stage', 'Acoustic Sunset',   '2026-12-05 16:00', '2026-12-05 18:00', 15.0),
  ('Saigon Sound Fest 2026', 'Main Stage',   'Closing Party',     '2026-12-06 19:00', '2026-12-07 01:00', 50.0),
  ('Winter Strings',         'Grand Hall',   'Quartet Evening',   '2026-12-20 19:30', '2026-12-20 21:30', 30.0),
  ('Da Nang Waves',          'Sand Stage',   'Sunrise Session',   '2027-05-01 05:00', '2027-05-01 08:00', NULL),
  ('Da Nang Waves',          NULL,           'Beach Cleanup',     '2027-05-02 07:00', '2027-05-02 09:00', 0.0))
INSERT INTO sub_event (event_id, area_id, title, starts_at, ends_at, price)
SELECT e.id, a.id, s.title, s.starts_at, s.ends_at, s.price
FROM s JOIN event e ON e.name = s.event LEFT JOIN area a ON a.name = s.stage;

INSERT INTO artist (name, country, genres) VALUES
  ('Kai Lam',         'VN', '["hip-hop"]'),
  ('Thu Ha',          'VN', '["pop", "ballad"]'),
  ('Paper Lanterns', 'AU', '["indie", "folk"]'),
  ('Lotus Quartet',   'VN', '["classical"]'),
  ('DJ Nova',         'SG', '["electronic", "house"]'),
  ('Unbooked Band',   'JP', '[]');   -- no lineup: tests empty embeds

WITH s(sub_event, artist, role, billing_order) AS (VALUES
  ('Opening Night',   'Thu Ha',          'headliner', 1),
  ('Opening Night',   'Kai Lam',         'performer', 2),
  ('Opening Night',   'DJ Nova',         'guest',     3),
  ('Acoustic Sunset', 'Paper Lanterns', 'headliner', 1),
  ('Closing Party',   'DJ Nova',         'headliner', 1),
  ('Closing Party',   'Kai Lam',         'guest',     2),
  ('Quartet Evening', 'Lotus Quartet',   'headliner', 1),
  ('Sunrise Session', 'DJ Nova',         'headliner', 1),
  ('Sunrise Session', 'Thu Ha',          'host',      2))
INSERT INTO lineup (sub_event_id, artist_id, role, billing_order)
SELECT se.id, ar.id, s.role, s.billing_order
FROM s JOIN sub_event se ON se.title = s.sub_event JOIN artist ar ON ar.name = s.artist;
