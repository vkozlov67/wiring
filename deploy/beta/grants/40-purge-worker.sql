BEGIN;
GRANT USAGE ON SCHEMA public TO wiring_purge_worker;
GRANT SELECT (id,email,created_at,deleted_at), UPDATE (email,password_hash,name,age,city,gender,looking_for,bio,photo,job,intent,
    height,communication,language,last_seen,premium_until,referral_code,seek_place,hide_tags,email_verified_at)
    ON users TO wiring_purge_worker;
GRANT SELECT (user_id), DELETE ON sendpulse_contacts TO wiring_purge_worker;
GRANT SELECT (user_id,delivery_state), UPDATE (client_id,consent_at,delivery_state) ON registration_events TO wiring_purge_worker;
GRANT SELECT (user_id), DELETE ON message_reactions,user_tags,user_prompts,photos,albums,promo_redemptions,password_resets,
    email_verifications,oauth_identities,oauth_native_codes,device_visits,filter_events,session_handoffs,push_subscriptions,mobile_devices,
    mobile_refresh_tokens TO wiring_purge_worker;
GRANT SELECT (from_id,to_id,photo,extra_photos,audio), DELETE ON messages TO wiring_purge_worker;
GRANT SELECT (from_id,to_id), DELETE ON swipes,blocks TO wiring_purge_worker;
GRANT SELECT (user_id,other_id), DELETE ON feed_history,reads,notes,snoozes,chat_openers TO wiring_purge_worker;
GRANT SELECT (user_id,source_kind,source_id), DELETE ON support_replies TO wiring_purge_worker;
GRANT SELECT (id,from_id,to_id), DELETE ON reports TO wiring_purge_worker;
GRANT SELECT (user_id,from_id), DELETE ON notifications TO wiring_purge_worker;
GRANT SELECT (referrer_id,referred_id), DELETE ON referrals TO wiring_purge_worker;
GRANT SELECT (id,user_id), DELETE ON support_tickets TO wiring_purge_worker;
COMMIT;
