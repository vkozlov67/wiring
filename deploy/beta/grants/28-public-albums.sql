BEGIN;
GRANT INSERT (user_id, title, sort_order, created_at), UPDATE (title), DELETE ON albums TO wiring_chat_writer;
GRANT USAGE ON SEQUENCE albums_id_seq TO wiring_chat_writer;
GRANT UPDATE (album_id) ON photos TO wiring_chat_writer;
COMMIT;
