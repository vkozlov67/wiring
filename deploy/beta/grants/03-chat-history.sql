BEGIN;
GRANT SELECT (body, audio_duration, edited_at, created_at, reply_to_id, transcript)
    ON messages TO wiring_chat_writer;
GRANT UPDATE ("read") ON notifications TO wiring_chat_writer;
COMMIT;
