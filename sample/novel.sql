-- Sample data for development: novel -> edition -> volume -> chapter -> part
-- Build: sqlite3 sample/novel.db < sample/novel.sql
-- DEV ONLY: ids are random 8-char hex (32 bits) from randomblob(); not for production.

PRAGMA foreign_keys = ON;

DROP TABLE IF EXISTS part;
DROP TABLE IF EXISTS chapter;
DROP TABLE IF EXISTS volume;
DROP TABLE IF EXISTS edition;
DROP TABLE IF EXISTS novel;

CREATE TABLE novel (
  id          TEXT    PRIMARY KEY NOT NULL DEFAULT (lower(hex(randomblob(4)))) CHECK (length(id) = 8),
  title       TEXT    NOT NULL UNIQUE,
  author      TEXT    NOT NULL,
  genre       TEXT,
  status      TEXT    NOT NULL DEFAULT 'ongoing' CHECK (status IN ('ongoing', 'completed', 'hiatus')),
  created_at  TEXT    NOT NULL DEFAULT (datetime('now'))
) STRICT;

CREATE TABLE edition (
  id            TEXT    PRIMARY KEY NOT NULL DEFAULT (lower(hex(randomblob(4)))) CHECK (length(id) = 8),
  novel_id      TEXT    NOT NULL REFERENCES novel(id) ON DELETE CASCADE,
  name          TEXT    NOT NULL,
  language      TEXT    NOT NULL,
  published_on  TEXT,
  UNIQUE (novel_id, language)
) STRICT;

CREATE TABLE volume (
  id          TEXT    PRIMARY KEY NOT NULL DEFAULT (lower(hex(randomblob(4)))) CHECK (length(id) = 8),
  edition_id  TEXT    NOT NULL REFERENCES edition(id) ON DELETE CASCADE,
  number      INTEGER NOT NULL,
  title       TEXT    NOT NULL,
  UNIQUE (edition_id, number)
) STRICT;

CREATE TABLE chapter (
  id          TEXT    PRIMARY KEY NOT NULL DEFAULT (lower(hex(randomblob(4)))) CHECK (length(id) = 8),
  volume_id   TEXT    NOT NULL REFERENCES volume(id) ON DELETE CASCADE,
  number      INTEGER NOT NULL,
  title       TEXT    NOT NULL,
  UNIQUE (volume_id, number)
) STRICT;

CREATE TABLE part (
  id          TEXT    PRIMARY KEY NOT NULL DEFAULT (lower(hex(randomblob(4)))) CHECK (length(id) = 8),
  chapter_id  TEXT    NOT NULL REFERENCES chapter(id) ON DELETE CASCADE,
  number      INTEGER NOT NULL,
  content     TEXT    NOT NULL,
  word_count  INTEGER NOT NULL DEFAULT 0,
  UNIQUE (chapter_id, number)
) STRICT;
-- Foreign keys are covered by the leading column of each UNIQUE index.

-- Seed rows get random ids; children find their parent by natural key.
INSERT INTO novel (title, author, genre, status) VALUES
  ('The Lantern Road', 'Mira Hale',   'fantasy', 'completed'),
  ('Signal Drift',     'Tomas Reyes', 'sci-fi',  'ongoing'),
  ('Quiet Harbor',     'Lin Aoki',    'mystery', 'hiatus');

WITH s(novel, name, language, published_on) AS (VALUES
  ('The Lantern Road', 'Original',       'en', '2021-03-01'),
  ('The Lantern Road', 'Vietnamese',     'vi', '2023-07-15'),
  ('Signal Drift',     'Web serial',     'en', '2024-01-10'),
  ('Quiet Harbor',     'First printing', 'en', '2022-11-20'))
INSERT INTO edition (novel_id, name, language, published_on)
SELECT n.id, s.name, s.language, s.published_on
FROM s JOIN novel n ON n.title = s.novel;

WITH s(novel, language, number, title) AS (VALUES
  ('The Lantern Road', 'en', 1, 'The Dark Mile'),
  ('The Lantern Road', 'en', 2, 'Embers'),
  ('The Lantern Road', 'vi', 1, 'Dặm Đường Tối'),
  ('Signal Drift',     'en', 1, 'First Contact'),
  ('Quiet Harbor',     'en', 1, 'Low Tide'))
INSERT INTO volume (edition_id, number, title)
SELECT e.id, s.number, s.title
FROM s
JOIN novel   n ON n.title = s.novel
JOIN edition e ON e.novel_id = n.id AND e.language = s.language;

WITH s(novel, language, vol, number, title) AS (VALUES
  ('The Lantern Road', 'en', 1, 1, 'A Light in the Fog'),
  ('The Lantern Road', 'en', 1, 2, 'The Toll Keeper'),
  ('The Lantern Road', 'en', 2, 1, 'Ashes at Dawn'),
  ('The Lantern Road', 'vi', 1, 1, 'Ánh Sáng Trong Sương'),
  ('Signal Drift',     'en', 1, 1, 'Static'),
  ('Signal Drift',     'en', 1, 2, 'Echoes'),
  ('Quiet Harbor',     'en', 1, 1, 'The Empty Pier'))
INSERT INTO chapter (volume_id, number, title)
SELECT v.id, s.number, s.title
FROM s
JOIN novel   n ON n.title = s.novel
JOIN edition e ON e.novel_id = n.id AND e.language = s.language
JOIN volume  v ON v.edition_id = e.id AND v.number = s.vol;

WITH s(novel, language, vol, ch, number, content, word_count) AS (VALUES
  ('The Lantern Road', 'en', 1, 1, 1, 'The road had no end that anyone could remember.', 9),
  ('The Lantern Road', 'en', 1, 1, 2, 'Then a lantern swung out of the fog.', 8),
  ('The Lantern Road', 'en', 1, 2, 1, 'The toll keeper asked for a name, not a coin.', 10),
  ('The Lantern Road', 'en', 1, 2, 2, 'Aren gave the wrong one.', 5),
  ('The Lantern Road', 'en', 2, 1, 1, 'By morning the village was smoke.', 6),
  ('The Lantern Road', 'vi', 1, 1, 1, 'Con đường không có điểm cuối mà ai còn nhớ.', 10),
  ('Signal Drift',     'en', 1, 1, 1, 'At 03:14 the array heard something breathe.', 7),
  ('Signal Drift',     'en', 1, 1, 2, 'Nobody on the night shift believed it.', 7),
  ('Signal Drift',     'en', 1, 2, 1, 'The second signal repeated the first, backwards.', 7),
  ('Quiet Harbor',     'en', 1, 1, 1, 'The boats were all there. The fishermen were not.', 9))
INSERT INTO part (chapter_id, number, content, word_count)
SELECT c.id, s.number, s.content, s.word_count
FROM s
JOIN novel   n ON n.title = s.novel
JOIN edition e ON e.novel_id = n.id AND e.language = s.language
JOIN volume  v ON v.edition_id = e.id AND v.number = s.vol
JOIN chapter c ON c.volume_id = v.id AND c.number = s.ch;
