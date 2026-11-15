BEGIN;
GRANT INSERT (user_id,name,email,body,ip,created_at) ON support_tickets TO wiring_chat_writer;
GRANT USAGE ON SEQUENCE support_tickets_id_seq TO wiring_chat_writer;
COMMIT;
