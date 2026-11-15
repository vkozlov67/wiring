BEGIN;
GRANT SELECT (paused, incognito) ON users TO wiring_chat_writer;
GRANT SELECT (user_id, other_id, until_at) ON snoozes TO wiring_chat_writer;
GRANT INSERT (user_id, other_id, delivered_at) ON feed_history TO wiring_chat_writer;
COMMIT;
