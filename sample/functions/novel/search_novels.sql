-- Novels whose title contains q (case-insensitive), with their edition count.
-- The result filters like a table: .eq('status', 'completed').order('title').
-- @param q text
-- @param max integer = 50
-- @returns setof
SELECT n.id, n.title, n.author, n.status, count(e.id) AS editions
FROM novel AS n LEFT JOIN edition AS e ON e.novel_id = n.id
WHERE n.title LIKE '%' || :q || '%'
GROUP BY n.id
LIMIT :max;
