BEGIN;
GRANT UPDATE (email_verified_at, password_hash) ON users TO wiring_chat_writer;
GRANT SELECT (token, user_id, used_at, created_at), UPDATE (used_at) ON email_verifications, password_resets TO wiring_chat_writer;
GRANT SELECT (code_hash, user_id, challenge, created_at), UPDATE (created_at), DELETE ON oauth_native_codes TO wiring_chat_writer;
COMMIT;
