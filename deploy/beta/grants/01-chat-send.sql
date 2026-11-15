BEGIN;
GRANT USAGE ON SCHEMA public TO wiring_chat_writer;
GRANT SELECT (id, deleted_at) ON users TO wiring_chat_writer;
GRANT SELECT (from_id, to_id) ON blocks TO wiring_chat_writer;
GRANT SELECT (from_id, to_id, direction) ON swipes TO wiring_chat_writer;
GRANT SELECT (id, from_id, to_id, deleted_at, client_id),
  INSERT (from_id, to_id, body, created_at, reply_to_id, client_id)
  ON messages TO wiring_chat_writer;
GRANT INSERT (user_id, other_id, last_read_id, deleted_at), UPDATE (last_read_id, deleted_at)
  ON reads TO wiring_chat_writer;
GRANT SELECT (user_id, other_id, last_read_id, deleted_at) ON reads TO wiring_chat_writer;
GRANT SELECT (id, user_id, from_id, kind, "read"),
  INSERT (user_id, kind, from_id, body, created_at, "read"),
  UPDATE (body, created_at) ON notifications TO wiring_chat_writer;
GRANT USAGE ON SEQUENCE messages_id_seq, notifications_id_seq TO wiring_chat_writer;
COMMIT;
