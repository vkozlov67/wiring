GRANT SELECT (lines, created_at), INSERT (user_id, other_id, lines, created_at), UPDATE (lines, created_at)
    ON chat_openers TO wiring_chat_writer;
