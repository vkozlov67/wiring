BEGIN;
GRANT SELECT (photo_rights_consent_at), UPDATE (name,name_changed_at,age,gender,looking_for,bio,job,intent,height,communication,
    seek_min_age,seek_max_age,seek_place,hide_tags,profile_setup_required,onboard_done) ON users TO wiring_chat_writer;
GRANT INSERT (user_id,kind,tag), DELETE ON user_tags TO wiring_chat_writer;
GRANT INSERT (user_id,prompt_id,answer,sort_order), DELETE ON user_prompts TO wiring_chat_writer;
GRANT INSERT (user_id,album_id,path,is_primary,sort_order,created_at) ON photos TO wiring_chat_writer;
GRANT USAGE ON SEQUENCE photos_id_seq TO wiring_chat_writer;
COMMIT;
