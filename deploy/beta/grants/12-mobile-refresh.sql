BEGIN;
GRANT SELECT (password_hash) ON users TO wiring_chat_writer;
GRANT SELECT (token_hash, user_id, family, pw, device, expires_at, used_at, revoked_at),
    INSERT (token_hash, user_id, family, pw, device, created_at, expires_at),
    UPDATE (used_at, revoked_at) ON mobile_refresh_tokens TO wiring_chat_writer;
COMMIT;
