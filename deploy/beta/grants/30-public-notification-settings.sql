BEGIN;
GRANT UPDATE (notify_enabled, notify_push) ON users TO wiring_chat_writer;
GRANT SELECT (user_id), DELETE ON push_subscriptions TO wiring_chat_writer;
COMMIT;
