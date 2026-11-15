BEGIN;
CREATE FUNCTION public.wiring_remove_expired_guest(guest bigint)
RETURNS boolean
LANGUAGE sql
SECURITY DEFINER
SET search_path = pg_catalog, pg_temp
AS $$
    WITH removed AS (
        DELETE FROM public.users
        WHERE id=guest AND email LIKE 'guest-%@wiring.guest'
          AND created_at < EXTRACT(EPOCH FROM statement_timestamp())::bigint-259200
        RETURNING 1
    )
    SELECT EXISTS(SELECT 1 FROM removed);
$$;
REVOKE ALL ON FUNCTION public.wiring_remove_expired_guest(bigint) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION public.wiring_remove_expired_guest(bigint) TO wiring_purge_worker;
COMMIT;
