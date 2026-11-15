BEGIN;
GRANT INSERT (photo, extra_photos, audio, audio_duration) ON messages TO wiring_chat_writer;
COMMIT;
