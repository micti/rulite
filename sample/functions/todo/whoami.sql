-- The caller's user id (null for anon) and role.
-- @returns row
SELECT :auth_uid AS id, :auth_role AS role;
