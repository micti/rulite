-- Adds a todo to one of the user's own lists; null when the list isn't theirs.
-- @param list_id text
-- @param title text
-- @returns row
INSERT INTO todo (list_id, user_id, title)
SELECT :list_id, :auth_uid, :title
WHERE EXISTS (SELECT 1 FROM list WHERE id = :list_id AND owner_id = :auth_uid)
RETURNING id, list_id, title, done;
