BEGIN;
GRANT SELECT (id,public_key), INSERT (id,private_pem,public_key) ON push_vapid TO wiring_chat_writer;
GRANT SELECT (endpoint,user_id,p256dh,auth,created_at), INSERT (endpoint,user_id,p256dh,auth,created_at), UPDATE (user_id,p256dh,auth,created_at)
    ON push_subscriptions TO wiring_chat_writer;
COMMIT;
