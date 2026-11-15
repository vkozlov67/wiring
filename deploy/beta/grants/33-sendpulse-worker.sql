BEGIN;
GRANT SELECT (marketing_consent_at, email_verified_at) ON users TO wiring_chat_writer;
GRANT SELECT (addressbook_id, email_key, synced_at, attempts, retry_at),
    INSERT (addressbook_id, email_key, user_id, synced_at, attempts, retry_at), UPDATE (synced_at, attempts, retry_at)
    ON sendpulse_contacts TO wiring_chat_writer;
COMMIT;
