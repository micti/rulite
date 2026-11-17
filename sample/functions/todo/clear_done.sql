-- Deletes the user's done todos and, with drop_empty, their lists left empty.
-- Both statements run in one transaction.
-- @param drop_empty boolean = false
-- @returns void
DELETE FROM todo WHERE user_id = :auth_uid AND done = 1;
DELETE FROM list
WHERE owner_id = :auth_uid AND :drop_empty
  AND NOT EXISTS (SELECT 1 FROM todo WHERE todo.list_id = list.id);
