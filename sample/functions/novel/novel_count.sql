-- How many novels there are, optionally with one status.
-- @param status text = null
-- @returns value
SELECT count(*) FROM novel WHERE :status IS NULL OR status = :status;
