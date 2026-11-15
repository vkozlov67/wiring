BEGIN;
GRANT SELECT (name, age, city, gender, looking_for, bio, job, intent, height, communication, last_seen,
    name_changed_at, theme, language, profile_setup_required, special_data_consent_at, premium_until, paused, incognito,
    referral_code, seek_min_age, seek_max_age, seek_place, hide_tags, jev_feed_enabled, jev_recommendations_unlocked),
    UPDATE (referral_code) ON users TO wiring_chat_writer;
GRANT SELECT (user_id, kind, tag) ON user_tags TO wiring_chat_writer;
GRANT SELECT (user_id, prompt_id, answer, sort_order) ON user_prompts TO wiring_chat_writer;
GRANT SELECT (id, user_id, title, sort_order) ON albums TO wiring_chat_writer;
GRANT SELECT (id, user_id, path, is_primary, album_id, sort_order) ON photos TO wiring_chat_writer;
GRANT SELECT (referrer_id) ON referrals TO wiring_chat_writer;
COMMIT;
