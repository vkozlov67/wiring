BEGIN;
GRANT SELECT (notify_enabled, notify_push) ON users TO wiring_chat_writer;
GRANT SELECT (platform, updated_at) ON mobile_devices TO wiring_chat_writer;
GRANT INSERT (token, user_id, platform, created_at, updated_at),
      UPDATE (user_id, platform, updated_at) ON mobile_devices TO wiring_chat_writer;
COMMIT;
