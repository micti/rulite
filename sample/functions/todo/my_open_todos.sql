-- The signed-in user's open todos with their list name. Functions see every
-- row (owner rules don't apply inside them), so :auth_uid limits the rows.
-- @returns setof
SELECT t.id, t.title, l.name AS list, t.created_at
FROM todo AS t JOIN list AS l ON l.id = t.list_id
WHERE t.user_id = :auth_uid AND t.done = 0;
