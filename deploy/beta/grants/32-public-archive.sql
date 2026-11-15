BEGIN;
GRANT SELECT (created_at), DELETE ON blocks TO wiring_chat_writer;
GRANT DELETE ON snoozes TO wiring_chat_writer;
COMMIT;
