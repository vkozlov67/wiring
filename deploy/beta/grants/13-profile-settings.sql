BEGIN;
GRANT SELECT (photo_rights_consent_at),
      UPDATE (language, theme, special_data_consent_at, photo_rights_consent_at)
    ON users TO wiring_chat_writer;
COMMIT;
