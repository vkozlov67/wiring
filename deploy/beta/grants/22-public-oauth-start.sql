GRANT SELECT (state_hash,browser,provider,redirect_uri,verifier,created_at,native_challenge),
      INSERT (state_hash,browser,provider,verifier,mode,referral,redirect_uri,created_at,native_challenge,native_state,
              registration_managed,analytics_client_id,analytics_consent),
      DELETE ON oauth_flows TO wiring_chat_writer;
