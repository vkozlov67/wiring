BEGIN;
GRANT SELECT (email, photo) ON users TO wiring_chat_writer;
GRANT SELECT (user_id) ON photos TO wiring_chat_writer;
COMMIT;
