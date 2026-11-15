GRANT SELECT (user_id, created_at, client_id, consent_at, next_attempt_at, delivery_state, attempts),
    UPDATE (delivery_state, attempts, next_attempt_at, submitted_at, error_code)
ON registration_events TO wiring_chat_writer;
