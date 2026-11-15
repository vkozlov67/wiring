BEGIN;
GRANT INSERT (email,password_hash,name,age,city,gender,looking_for,bio,photo,job,intent,height,
    communication,privacy_accepted_at,special_data_consent_at,photo_rights_consent_at,marketing_consent_at,
    email_verified_at,onboard_done,profile_setup_required,is_seed,created_at,last_seen,language,premium_until),
    UPDATE (premium_until) ON users TO wiring_chat_writer;
GRANT USAGE ON SEQUENCE users_id_seq TO wiring_chat_writer;
GRANT INSERT (user_id,created_at,channel,mode,consent_at,client_id,delivery_state) ON registration_events TO wiring_chat_writer;
GRANT SELECT (user_id) ON registration_events TO wiring_chat_writer;
GRANT INSERT (referrer_id,referred_id,created_at), SELECT (referred_id) ON referrals TO wiring_chat_writer;
GRANT INSERT (token,user_id,created_at), DELETE ON email_verifications,password_resets TO wiring_chat_writer;
COMMIT;
