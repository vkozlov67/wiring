BEGIN;
GRANT INSERT (excluded_at), UPDATE (excluded_at) ON feed_history TO wiring_chat_writer;
GRANT SELECT (created_at), INSERT (user_id, other_id, until_at, created_at),
    UPDATE (until_at, created_at) ON snoozes TO wiring_chat_writer;
COMMIT;
