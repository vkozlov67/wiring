BEGIN;
GRANT SELECT (user_id,fingerprint,created_at), INSERT (user_id,source,fingerprint,kind,value,created_at)
    ON filter_events TO wiring_chat_writer;
GRANT USAGE ON SEQUENCE filter_events_id_seq TO wiring_chat_writer;
COMMIT;
