BEGIN;
GRANT UPDATE (city) ON users TO wiring_chat_writer;
GRANT SELECT (query, country, city), INSERT (query, country, city, created_at), UPDATE (city, created_at)
    ON place_resolutions TO wiring_chat_writer;
COMMIT;
