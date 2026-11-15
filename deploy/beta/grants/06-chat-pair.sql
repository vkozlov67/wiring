BEGIN;
GRANT INSERT (from_id, to_id, created_at) ON blocks TO wiring_chat_writer;
GRANT SELECT (created_at), INSERT (from_id, to_id, direction, created_at), UPDATE (direction, created_at), DELETE
    ON swipes TO wiring_chat_writer;
GRANT SELECT (user_id, other_id), DELETE ON chat_openers TO wiring_chat_writer;
GRANT SELECT (id, from_id, to_id), INSERT (from_id, to_id, reason, details, created_at) ON reports TO wiring_chat_writer;
GRANT USAGE ON SEQUENCE reports_id_seq TO wiring_chat_writer;
GRANT SELECT (report_banned_at, is_seed), UPDATE (report_banned_at) ON users TO wiring_chat_writer;
COMMIT;
