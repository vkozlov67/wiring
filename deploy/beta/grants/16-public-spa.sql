BEGIN;
GRANT SELECT (user_id, visitor_key, device_class, created_at),
    INSERT (user_id, visitor_key, device_class, os_name, created_at) ON device_visits TO wiring_chat_writer;
GRANT USAGE ON SEQUENCE device_visits_id_seq TO wiring_chat_writer;
COMMIT;
