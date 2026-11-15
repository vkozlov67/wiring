BEGIN;
GRANT INSERT (code,days,max_uses,uses) ON promo_codes TO wiring_chat_writer;
GRANT SELECT (id,status,approved_at,approved_by), UPDATE (status,updated_at,approved_at,approved_by)
    ON feature_tasks TO wiring_chat_writer;
GRANT SELECT (id,name,email,body,created_at) ON support_tickets TO wiring_chat_writer;
GRANT SELECT (source_type,source_id,dedupe_key), INSERT (source_type,source_id,source_chat,source_author,source_created_at,raw_text,
    title,category,impact,reach,effort,confidence,score,threshold,status,duplicate_of,created_at,updated_at,dedupe_key)
    ON feature_tasks TO wiring_chat_writer;
GRANT USAGE ON SEQUENCE feature_tasks_id_seq TO wiring_chat_writer;
COMMIT;
