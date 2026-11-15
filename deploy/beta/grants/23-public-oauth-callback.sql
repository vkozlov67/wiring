BEGIN;
GRANT SELECT (mode,referral,native_state,callback_payload,registration_managed,analytics_client_id,analytics_consent),
    UPDATE (callback_payload) ON oauth_flows TO wiring_chat_writer;
GRANT SELECT (onboard_done) ON users TO wiring_chat_writer;
GRANT SELECT (provider,subject,user_id), INSERT (provider,subject,user_id) ON oauth_identities TO wiring_chat_writer;
GRANT INSERT (code_hash,user_id,challenge,created_at) ON oauth_native_codes TO wiring_chat_writer;
COMMIT;
