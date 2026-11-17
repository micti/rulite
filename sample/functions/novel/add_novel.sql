-- Adds a novel and returns it; a taken title fails with 23505.
-- @param title text
-- @param author text
-- @param genre text = null
-- @returns row
INSERT INTO novel (title, author, genre) VALUES (:title, :author, :genre)
RETURNING id, title, author, genre, status;
