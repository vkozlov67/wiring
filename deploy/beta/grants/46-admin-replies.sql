BEGIN;
GRANT UPDATE (id) ON reports,support_tickets TO wiring_chat_writer;
GRANT SELECT (email_attempted_at), INSERT (source_kind,source_id,user_id,email,body,request_id,created_at,email_attempted_at),
    UPDATE (email_attempted_at,email_sent_at) ON support_replies TO wiring_chat_writer;
GRANT USAGE ON SEQUENCE support_replies_id_seq TO wiring_chat_writer;
COMMIT;
