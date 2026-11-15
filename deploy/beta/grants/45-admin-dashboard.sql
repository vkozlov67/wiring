BEGIN;
GRANT SELECT (id,name,email,age,city,created_at,is_seed,premium_until,deleted_at,report_banned_at) ON users TO wiring_chat_writer;
GRANT SELECT (from_id,to_id,direction,created_at) ON swipes TO wiring_chat_writer;
GRANT SELECT (from_id,to_id,deleted_at) ON messages TO wiring_chat_writer;
GRANT SELECT (id,from_id,to_id,reason,details,created_at) ON reports TO wiring_chat_writer;
GRANT SELECT (from_id) ON blocks TO wiring_chat_writer;
GRANT SELECT (user_id,fingerprint,kind,value,created_at) ON filter_events TO wiring_chat_writer;
GRANT SELECT (user_id,visitor_key,device_class,os_name,created_at) ON device_visits TO wiring_chat_writer;
GRANT SELECT (delivery_state) ON registration_events TO wiring_chat_writer;
GRANT SELECT (code,days,max_uses,uses,benefit) ON promo_codes TO wiring_chat_writer;
GRANT SELECT (id,user_id,name,email,body,created_at) ON support_tickets TO wiring_chat_writer;
GRANT SELECT (id,title,category,score,status,source_type,source_author,source_chat,raw_text,impact,reach,effort,confidence,duplicate_of,updated_at)
    ON feature_tasks TO wiring_chat_writer;
GRANT SELECT (id,source_kind,source_id,user_id,email,body,request_id,email_sent_at) ON support_replies TO wiring_chat_writer;
COMMIT;
