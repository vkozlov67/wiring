BEGIN;
GRANT UPDATE (incognito, paused, jev_recommendations_unlocked) ON users TO wiring_chat_writer;
GRANT SELECT (code, days, max_uses, uses, benefit), UPDATE (uses) ON promo_codes TO wiring_chat_writer;
GRANT SELECT (code, user_id), INSERT (code, user_id, created_at) ON promo_redemptions TO wiring_chat_writer;
COMMIT;
