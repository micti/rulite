-- Novel counts per status, as one JSON object built by SQLite.
-- @returns json
SELECT json_object('novels', count(*), 'by_status', json_group_object(status, n))
FROM (SELECT status, count(*) AS n FROM novel GROUP BY status);
