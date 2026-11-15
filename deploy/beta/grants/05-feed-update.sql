BEGIN;
GRANT SELECT (feed_generation, premium_until), UPDATE (feed_generation) ON users TO wiring_chat_writer;
GRANT SELECT (user_id, other_id, viewed_at, excluded_at), UPDATE (viewed_at), DELETE
    ON feed_history TO wiring_chat_writer;
COMMIT;
