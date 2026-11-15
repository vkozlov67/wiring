BEGIN;
GRANT SELECT (photo, extra_photos, audio), UPDATE (body, edited_at, deleted_at)
    ON messages TO wiring_chat_writer;
GRANT SELECT (message_id, user_id, emoji), INSERT (message_id, user_id, emoji),
    UPDATE (emoji), DELETE ON message_reactions TO wiring_chat_writer;
COMMIT;
