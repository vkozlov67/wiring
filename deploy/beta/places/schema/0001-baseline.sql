-- WIRING PostgreSQL baseline for an empty database. No user data or role credentials.

CREATE TABLE public.albums (
    id bigint NOT NULL,
    user_id integer NOT NULL,
    title text NOT NULL,
    sort_order integer DEFAULT 0 NOT NULL,
    created_at integer NOT NULL
);

CREATE SEQUENCE public.albums_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;

ALTER SEQUENCE public.albums_id_seq OWNED BY public.albums.id;

CREATE TABLE public.app_migrations (
    id text NOT NULL,
    applied_at integer NOT NULL
);

CREATE TABLE public.blocks (
    from_id integer NOT NULL,
    to_id integer NOT NULL,
    created_at integer NOT NULL
);

CREATE TABLE public.chat_openers (
    user_id integer NOT NULL,
    other_id integer NOT NULL,
    lines text NOT NULL,
    created_at integer NOT NULL
);

CREATE TABLE public.device_visits (
    id bigint NOT NULL,
    user_id integer,
    visitor_key text DEFAULT ''::text NOT NULL,
    device_class text NOT NULL,
    os_name text DEFAULT ''::text NOT NULL,
    created_at integer NOT NULL
);

CREATE SEQUENCE public.device_visits_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;

ALTER SEQUENCE public.device_visits_id_seq OWNED BY public.device_visits.id;

CREATE TABLE public.email_verifications (
    token text NOT NULL,
    user_id integer NOT NULL,
    created_at integer NOT NULL,
    used_at integer
);

CREATE TABLE public.feature_tasks (
    id bigint NOT NULL,
    source_type text NOT NULL,
    source_id text NOT NULL,
    source_chat text DEFAULT ''::text NOT NULL,
    source_author text DEFAULT ''::text NOT NULL,
    source_created_at integer DEFAULT 0 NOT NULL,
    raw_text text NOT NULL,
    title text NOT NULL,
    category text NOT NULL,
    impact integer DEFAULT 1 NOT NULL,
    reach integer DEFAULT 1 NOT NULL,
    effort integer DEFAULT 3 NOT NULL,
    confidence integer DEFAULT 1 NOT NULL,
    score real DEFAULT 0 NOT NULL,
    threshold real DEFAULT 6 NOT NULL,
    status text DEFAULT 'filtered'::text NOT NULL,
    duplicate_of integer,
    approved_at integer,
    approved_by text DEFAULT ''::text NOT NULL,
    created_at integer NOT NULL,
    updated_at integer NOT NULL,
    dedupe_key text NOT NULL
);

CREATE SEQUENCE public.feature_tasks_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;

ALTER SEQUENCE public.feature_tasks_id_seq OWNED BY public.feature_tasks.id;

CREATE TABLE public.feed_history (
    user_id integer NOT NULL,
    other_id integer NOT NULL,
    delivered_at bigint NOT NULL,
    viewed_at bigint,
    excluded_at bigint
);

CREATE TABLE public.filter_events (
    id bigint NOT NULL,
    user_id integer NOT NULL,
    source text NOT NULL,
    fingerprint text NOT NULL,
    kind text NOT NULL,
    value text NOT NULL,
    created_at integer NOT NULL
);

CREATE SEQUENCE public.filter_events_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;

ALTER SEQUENCE public.filter_events_id_seq OWNED BY public.filter_events.id;

CREATE TABLE public.message_reactions (
    message_id bigint NOT NULL,
    user_id bigint NOT NULL,
    emoji text NOT NULL
);

CREATE TABLE public.messages (
    id bigint NOT NULL,
    from_id integer NOT NULL,
    to_id integer NOT NULL,
    body text NOT NULL,
    created_at integer NOT NULL,
    deleted_at integer,
    reply_to_id integer,
    photo text DEFAULT ''::text NOT NULL,
    extra_photos text DEFAULT '[]'::text NOT NULL,
    audio text DEFAULT ''::text NOT NULL,
    audio_duration integer DEFAULT 0 NOT NULL,
    transcript text DEFAULT ''::text NOT NULL,
    edited_at integer,
    client_id text
);

CREATE SEQUENCE public.messages_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;

ALTER SEQUENCE public.messages_id_seq OWNED BY public.messages.id;

CREATE TABLE public.mobile_devices (
    token text NOT NULL,
    user_id integer NOT NULL,
    platform text NOT NULL,
    created_at integer NOT NULL,
    updated_at integer NOT NULL
);

CREATE TABLE public.mobile_refresh_tokens (
    token_hash text NOT NULL,
    user_id integer NOT NULL,
    family text NOT NULL,
    pw text NOT NULL,
    device text DEFAULT ''::text NOT NULL,
    created_at integer NOT NULL,
    expires_at integer NOT NULL,
    used_at integer,
    revoked_at integer
);

CREATE TABLE public.notes (
    user_id integer NOT NULL,
    other_id integer NOT NULL,
    body text DEFAULT ''::text NOT NULL,
    updated_at integer NOT NULL
);

CREATE TABLE public.notifications (
    id bigint NOT NULL,
    user_id integer NOT NULL,
    kind text NOT NULL,
    from_id integer DEFAULT 0 NOT NULL,
    body text DEFAULT ''::text NOT NULL,
    created_at integer NOT NULL,
    read integer DEFAULT 0 NOT NULL
);

CREATE SEQUENCE public.notifications_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;

ALTER SEQUENCE public.notifications_id_seq OWNED BY public.notifications.id;

CREATE TABLE public.oauth_flows (
    state_hash text NOT NULL,
    browser text NOT NULL,
    provider text NOT NULL,
    verifier text NOT NULL,
    mode text NOT NULL,
    referral text NOT NULL,
    redirect_uri text NOT NULL,
    created_at bigint NOT NULL,
    callback_payload text,
    native_challenge text,
    native_state text,
    registration_managed integer DEFAULT 0 NOT NULL,
    analytics_client_id text,
    analytics_consent integer DEFAULT 0 NOT NULL
);

CREATE TABLE public.oauth_identities (
    provider text NOT NULL,
    subject text NOT NULL,
    user_id bigint NOT NULL
);

CREATE TABLE public.oauth_native_codes (
    code_hash text NOT NULL,
    user_id bigint NOT NULL,
    challenge text NOT NULL,
    created_at bigint NOT NULL
);

CREATE TABLE public.password_resets (
    token text NOT NULL,
    user_id integer NOT NULL,
    created_at integer NOT NULL,
    used_at integer
);

CREATE TABLE public.photos (
    id bigint NOT NULL,
    user_id integer NOT NULL,
    album_id integer,
    path text NOT NULL,
    is_primary integer DEFAULT 0 NOT NULL,
    sort_order integer DEFAULT 0 NOT NULL,
    created_at integer NOT NULL
);

CREATE SEQUENCE public.photos_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;

ALTER SEQUENCE public.photos_id_seq OWNED BY public.photos.id;

CREATE TABLE public.place_resolutions (
    query text NOT NULL,
    country text NOT NULL,
    city text NOT NULL,
    created_at bigint NOT NULL
);

CREATE TABLE public.promo_codes (
    code text NOT NULL,
    days integer NOT NULL,
    max_uses integer DEFAULT 0 NOT NULL,
    uses integer DEFAULT 0 NOT NULL,
    benefit text DEFAULT 'plus'::text NOT NULL
);

CREATE TABLE public.promo_redemptions (
    code text NOT NULL,
    user_id integer NOT NULL,
    created_at integer NOT NULL
);

CREATE TABLE public.push_subscriptions (
    endpoint text NOT NULL,
    user_id integer NOT NULL,
    p256dh text NOT NULL,
    auth text NOT NULL,
    created_at integer NOT NULL
);

CREATE TABLE public.push_vapid (
    id integer NOT NULL,
    private_pem text NOT NULL,
    public_key text NOT NULL
);

CREATE TABLE public.reads (
    user_id integer NOT NULL,
    other_id integer NOT NULL,
    last_read_id integer DEFAULT 0 NOT NULL,
    deleted_at integer
);

CREATE TABLE public.referrals (
    referrer_id integer NOT NULL,
    referred_id integer NOT NULL,
    created_at integer NOT NULL
);

CREATE TABLE public.registration_events (
    user_id bigint NOT NULL,
    created_at bigint NOT NULL,
    channel text NOT NULL,
    mode text NOT NULL,
    consent_at bigint,
    client_id text,
    delivery_state text NOT NULL,
    attempts integer DEFAULT 0 NOT NULL,
    next_attempt_at bigint DEFAULT 0 NOT NULL,
    submitted_at bigint,
    error_code text
);

CREATE TABLE public.reports (
    id bigint NOT NULL,
    from_id integer NOT NULL,
    to_id integer NOT NULL,
    reason text NOT NULL,
    details text DEFAULT ''::text NOT NULL,
    created_at integer NOT NULL
);

CREATE SEQUENCE public.reports_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;

ALTER SEQUENCE public.reports_id_seq OWNED BY public.reports.id;

CREATE TABLE public.sendpulse_contacts (
    addressbook_id bigint NOT NULL,
    email_key text NOT NULL,
    user_id bigint NOT NULL,
    synced_at bigint,
    attempts integer DEFAULT 0 NOT NULL,
    retry_at bigint DEFAULT 0 NOT NULL
);

CREATE TABLE public.session_handoffs (
    nonce text NOT NULL,
    user_id integer NOT NULL,
    expires_at integer NOT NULL
);

CREATE TABLE public.snoozes (
    user_id integer NOT NULL,
    other_id integer NOT NULL,
    until_at integer NOT NULL,
    created_at integer NOT NULL
);

CREATE TABLE public.support_replies (
    id bigint NOT NULL,
    source_kind text NOT NULL,
    source_id integer NOT NULL,
    user_id integer,
    email text DEFAULT ''::text NOT NULL,
    body text NOT NULL,
    request_id text NOT NULL,
    created_at integer NOT NULL,
    email_attempted_at integer,
    email_sent_at integer,
    CONSTRAINT support_replies_source_kind_check CHECK ((source_kind = ANY (ARRAY['report'::text, 'ticket'::text])))
);

CREATE SEQUENCE public.support_replies_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;

ALTER SEQUENCE public.support_replies_id_seq OWNED BY public.support_replies.id;

CREATE TABLE public.support_tickets (
    id bigint NOT NULL,
    user_id integer,
    name text DEFAULT ''::text NOT NULL,
    email text DEFAULT ''::text NOT NULL,
    body text NOT NULL,
    ip text DEFAULT ''::text NOT NULL,
    created_at integer NOT NULL
);

CREATE SEQUENCE public.support_tickets_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;

ALTER SEQUENCE public.support_tickets_id_seq OWNED BY public.support_tickets.id;

CREATE TABLE public.swipes (
    from_id integer NOT NULL,
    to_id integer NOT NULL,
    direction text NOT NULL,
    created_at integer NOT NULL
);

CREATE TABLE public.telegram_cursors (
    chat_id text NOT NULL,
    last_message_id integer DEFAULT 0 NOT NULL,
    updated_at integer NOT NULL
);

CREATE TABLE public.user_prompts (
    user_id integer NOT NULL,
    prompt_id text NOT NULL,
    answer text NOT NULL,
    sort_order integer DEFAULT 0 NOT NULL
);

CREATE TABLE public.user_tags (
    user_id integer NOT NULL,
    kind text NOT NULL,
    tag text NOT NULL
);

CREATE TABLE public.users (
    id bigint NOT NULL,
    email text NOT NULL,
    password_hash text NOT NULL,
    name text NOT NULL,
    age integer NOT NULL,
    city text NOT NULL,
    gender text NOT NULL,
    looking_for text NOT NULL,
    bio text DEFAULT ''::text NOT NULL,
    photo text DEFAULT ''::text NOT NULL,
    job text DEFAULT ''::text NOT NULL,
    intent text DEFAULT 'dating'::text NOT NULL,
    height integer,
    communication text DEFAULT ''::text NOT NULL,
    last_seen integer DEFAULT 0 NOT NULL,
    is_seed integer DEFAULT 0 NOT NULL,
    deleted_at integer,
    created_at integer NOT NULL,
    feed_generation integer DEFAULT 0 NOT NULL,
    name_changed_at integer,
    theme text,
    language text DEFAULT 'ru'::text NOT NULL,
    onboard_done integer DEFAULT 0 NOT NULL,
    profile_setup_required integer DEFAULT 0 NOT NULL,
    premium_until integer DEFAULT 0 NOT NULL,
    incognito integer DEFAULT 0 NOT NULL,
    paused integer DEFAULT 0 NOT NULL,
    notify_enabled integer DEFAULT 1 NOT NULL,
    notify_push integer DEFAULT 1 NOT NULL,
    referral_code text,
    privacy_accepted_at integer,
    special_data_consent_at integer,
    photo_rights_consent_at integer,
    marketing_consent_at integer,
    seek_min_age integer DEFAULT 18 NOT NULL,
    seek_max_age integer DEFAULT 99 NOT NULL,
    seek_place text DEFAULT ''::text NOT NULL,
    hide_tags text DEFAULT ''::text NOT NULL,
    report_banned_at integer,
    jev_feed_enabled integer DEFAULT 0 NOT NULL,
    jev_recommendations_unlocked integer DEFAULT 0 NOT NULL,
    email_verified_at integer
);

CREATE SEQUENCE public.users_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;

ALTER SEQUENCE public.users_id_seq OWNED BY public.users.id;

ALTER TABLE ONLY public.albums ALTER COLUMN id SET DEFAULT nextval('public.albums_id_seq'::regclass);

ALTER TABLE ONLY public.device_visits ALTER COLUMN id SET DEFAULT nextval('public.device_visits_id_seq'::regclass);

ALTER TABLE ONLY public.feature_tasks ALTER COLUMN id SET DEFAULT nextval('public.feature_tasks_id_seq'::regclass);

ALTER TABLE ONLY public.filter_events ALTER COLUMN id SET DEFAULT nextval('public.filter_events_id_seq'::regclass);

ALTER TABLE ONLY public.messages ALTER COLUMN id SET DEFAULT nextval('public.messages_id_seq'::regclass);

ALTER TABLE ONLY public.notifications ALTER COLUMN id SET DEFAULT nextval('public.notifications_id_seq'::regclass);

ALTER TABLE ONLY public.photos ALTER COLUMN id SET DEFAULT nextval('public.photos_id_seq'::regclass);

ALTER TABLE ONLY public.reports ALTER COLUMN id SET DEFAULT nextval('public.reports_id_seq'::regclass);

ALTER TABLE ONLY public.support_replies ALTER COLUMN id SET DEFAULT nextval('public.support_replies_id_seq'::regclass);

ALTER TABLE ONLY public.support_tickets ALTER COLUMN id SET DEFAULT nextval('public.support_tickets_id_seq'::regclass);

ALTER TABLE ONLY public.users ALTER COLUMN id SET DEFAULT nextval('public.users_id_seq'::regclass);

ALTER TABLE ONLY public.albums
    ADD CONSTRAINT albums_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.app_migrations
    ADD CONSTRAINT app_migrations_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.blocks
    ADD CONSTRAINT blocks_pkey PRIMARY KEY (from_id, to_id);

ALTER TABLE ONLY public.chat_openers
    ADD CONSTRAINT chat_openers_pkey PRIMARY KEY (user_id, other_id);

ALTER TABLE ONLY public.device_visits
    ADD CONSTRAINT device_visits_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.email_verifications
    ADD CONSTRAINT email_verifications_pkey PRIMARY KEY (token);

ALTER TABLE ONLY public.feature_tasks
    ADD CONSTRAINT feature_tasks_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.feature_tasks
    ADD CONSTRAINT feature_tasks_source_type_source_id_key UNIQUE (source_type, source_id);

ALTER TABLE ONLY public.feed_history
    ADD CONSTRAINT feed_history_pkey PRIMARY KEY (user_id, other_id);

ALTER TABLE ONLY public.filter_events
    ADD CONSTRAINT filter_events_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.message_reactions
    ADD CONSTRAINT message_reactions_pkey PRIMARY KEY (message_id, user_id);

ALTER TABLE ONLY public.messages
    ADD CONSTRAINT messages_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.mobile_devices
    ADD CONSTRAINT mobile_devices_pkey PRIMARY KEY (token);

ALTER TABLE ONLY public.mobile_refresh_tokens
    ADD CONSTRAINT mobile_refresh_tokens_pkey PRIMARY KEY (token_hash);

ALTER TABLE ONLY public.notes
    ADD CONSTRAINT notes_pkey PRIMARY KEY (user_id, other_id);

ALTER TABLE ONLY public.notifications
    ADD CONSTRAINT notifications_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.oauth_flows
    ADD CONSTRAINT oauth_flows_pkey PRIMARY KEY (state_hash);

ALTER TABLE ONLY public.oauth_identities
    ADD CONSTRAINT oauth_identities_pkey PRIMARY KEY (provider, subject);

ALTER TABLE ONLY public.oauth_identities
    ADD CONSTRAINT oauth_identities_user_id_provider_key UNIQUE (user_id, provider);

ALTER TABLE ONLY public.oauth_native_codes
    ADD CONSTRAINT oauth_native_codes_pkey PRIMARY KEY (code_hash);

ALTER TABLE ONLY public.password_resets
    ADD CONSTRAINT password_resets_pkey PRIMARY KEY (token);

ALTER TABLE ONLY public.photos
    ADD CONSTRAINT photos_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.place_resolutions
    ADD CONSTRAINT place_resolutions_pkey PRIMARY KEY (query, country);

ALTER TABLE ONLY public.promo_codes
    ADD CONSTRAINT promo_codes_pkey PRIMARY KEY (code);

ALTER TABLE ONLY public.promo_redemptions
    ADD CONSTRAINT promo_redemptions_pkey PRIMARY KEY (code, user_id);

ALTER TABLE ONLY public.push_subscriptions
    ADD CONSTRAINT push_subscriptions_pkey PRIMARY KEY (endpoint);

ALTER TABLE ONLY public.push_vapid
    ADD CONSTRAINT push_vapid_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.reads
    ADD CONSTRAINT reads_pkey PRIMARY KEY (user_id, other_id);

ALTER TABLE ONLY public.referrals
    ADD CONSTRAINT referrals_pkey PRIMARY KEY (referred_id);

ALTER TABLE ONLY public.registration_events
    ADD CONSTRAINT registration_events_pkey PRIMARY KEY (user_id);

ALTER TABLE ONLY public.reports
    ADD CONSTRAINT reports_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.sendpulse_contacts
    ADD CONSTRAINT sendpulse_contacts_pkey PRIMARY KEY (addressbook_id, email_key);

ALTER TABLE ONLY public.session_handoffs
    ADD CONSTRAINT session_handoffs_pkey PRIMARY KEY (nonce);

ALTER TABLE ONLY public.snoozes
    ADD CONSTRAINT snoozes_pkey PRIMARY KEY (user_id, other_id);

ALTER TABLE ONLY public.support_replies
    ADD CONSTRAINT support_replies_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.support_replies
    ADD CONSTRAINT support_replies_request_id_key UNIQUE (request_id);

ALTER TABLE ONLY public.support_tickets
    ADD CONSTRAINT support_tickets_pkey PRIMARY KEY (id);

ALTER TABLE ONLY public.swipes
    ADD CONSTRAINT swipes_pkey PRIMARY KEY (from_id, to_id);

ALTER TABLE ONLY public.telegram_cursors
    ADD CONSTRAINT telegram_cursors_pkey PRIMARY KEY (chat_id);

ALTER TABLE ONLY public.user_prompts
    ADD CONSTRAINT user_prompts_pkey PRIMARY KEY (user_id, prompt_id);

ALTER TABLE ONLY public.user_tags
    ADD CONSTRAINT user_tags_pkey PRIMARY KEY (user_id, kind, tag);

ALTER TABLE ONLY public.users
    ADD CONSTRAINT users_email_key UNIQUE (email);

ALTER TABLE ONLY public.users
    ADD CONSTRAINT users_pkey PRIMARY KEY (id);

CREATE INDEX idx_device_visits_class ON public.device_visits USING btree (device_class, created_at);

CREATE INDEX idx_device_visits_created ON public.device_visits USING btree (created_at);

CREATE INDEX idx_feature_tasks_board ON public.feature_tasks USING btree (status, score DESC, updated_at DESC);

CREATE INDEX idx_feature_tasks_dedupe ON public.feature_tasks USING btree (dedupe_key, status);

CREATE INDEX idx_feed_history_other ON public.feed_history USING btree (other_id);

CREATE INDEX idx_filter_events_kind ON public.filter_events USING btree (kind, value, created_at);

CREATE INDEX idx_filter_events_user ON public.filter_events USING btree (user_id, fingerprint, created_at);

CREATE UNIQUE INDEX idx_messages_client_id ON public.messages USING btree (from_id, client_id) WHERE (client_id IS NOT NULL);

CREATE INDEX idx_messages_pair ON public.messages USING btree (from_id, to_id, id);

CREATE INDEX idx_mobile_devices_user ON public.mobile_devices USING btree (user_id);

CREATE INDEX idx_mobile_refresh_family ON public.mobile_refresh_tokens USING btree (family);

CREATE INDEX idx_mobile_refresh_user ON public.mobile_refresh_tokens USING btree (user_id);

CREATE INDEX idx_notices_user ON public.notifications USING btree (user_id, read);

CREATE INDEX idx_push_subscriptions_user ON public.push_subscriptions USING btree (user_id);

CREATE INDEX idx_referrals_from ON public.referrals USING btree (referrer_id);

CREATE INDEX idx_support_replies_user ON public.support_replies USING btree (user_id, id);

CREATE INDEX idx_swipes_to ON public.swipes USING btree (to_id, direction);

CREATE INDEX idx_users_deleted ON public.users USING btree (deleted_at);

CREATE UNIQUE INDEX idx_users_referral ON public.users USING btree (referral_code);

CREATE INDEX registration_delivery_queue ON public.registration_events USING btree (delivery_state, next_attempt_at);

ALTER TABLE ONLY public.albums
    ADD CONSTRAINT albums_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE CASCADE;

ALTER TABLE ONLY public.blocks
    ADD CONSTRAINT blocks_from_id_fkey FOREIGN KEY (from_id) REFERENCES public.users(id) ON DELETE CASCADE;

ALTER TABLE ONLY public.blocks
    ADD CONSTRAINT blocks_to_id_fkey FOREIGN KEY (to_id) REFERENCES public.users(id) ON DELETE CASCADE;

ALTER TABLE ONLY public.chat_openers
    ADD CONSTRAINT chat_openers_other_id_fkey FOREIGN KEY (other_id) REFERENCES public.users(id) ON DELETE CASCADE;

ALTER TABLE ONLY public.chat_openers
    ADD CONSTRAINT chat_openers_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE CASCADE;

ALTER TABLE ONLY public.device_visits
    ADD CONSTRAINT device_visits_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE SET NULL;

ALTER TABLE ONLY public.email_verifications
    ADD CONSTRAINT email_verifications_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE CASCADE;

ALTER TABLE ONLY public.feature_tasks
    ADD CONSTRAINT feature_tasks_duplicate_of_fkey FOREIGN KEY (duplicate_of) REFERENCES public.feature_tasks(id) ON DELETE SET NULL;

ALTER TABLE ONLY public.feed_history
    ADD CONSTRAINT feed_history_other_id_fkey FOREIGN KEY (other_id) REFERENCES public.users(id) ON DELETE CASCADE;

ALTER TABLE ONLY public.feed_history
    ADD CONSTRAINT feed_history_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE CASCADE;

ALTER TABLE ONLY public.filter_events
    ADD CONSTRAINT filter_events_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE CASCADE;

ALTER TABLE ONLY public.message_reactions
    ADD CONSTRAINT message_reactions_message_id_fkey FOREIGN KEY (message_id) REFERENCES public.messages(id) ON DELETE CASCADE;

ALTER TABLE ONLY public.message_reactions
    ADD CONSTRAINT message_reactions_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE CASCADE;

ALTER TABLE ONLY public.messages
    ADD CONSTRAINT messages_from_id_fkey FOREIGN KEY (from_id) REFERENCES public.users(id) ON DELETE CASCADE;

ALTER TABLE ONLY public.messages
    ADD CONSTRAINT messages_to_id_fkey FOREIGN KEY (to_id) REFERENCES public.users(id) ON DELETE CASCADE;

ALTER TABLE ONLY public.mobile_devices
    ADD CONSTRAINT mobile_devices_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE CASCADE;

ALTER TABLE ONLY public.mobile_refresh_tokens
    ADD CONSTRAINT mobile_refresh_tokens_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE CASCADE;

ALTER TABLE ONLY public.notes
    ADD CONSTRAINT notes_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE CASCADE;

ALTER TABLE ONLY public.notifications
    ADD CONSTRAINT notifications_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE CASCADE;

ALTER TABLE ONLY public.oauth_identities
    ADD CONSTRAINT oauth_identities_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id);

ALTER TABLE ONLY public.oauth_native_codes
    ADD CONSTRAINT oauth_native_codes_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id);

ALTER TABLE ONLY public.password_resets
    ADD CONSTRAINT password_resets_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE CASCADE;

ALTER TABLE ONLY public.photos
    ADD CONSTRAINT photos_album_id_fkey FOREIGN KEY (album_id) REFERENCES public.albums(id) ON DELETE SET NULL;

ALTER TABLE ONLY public.photos
    ADD CONSTRAINT photos_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE CASCADE;

ALTER TABLE ONLY public.promo_redemptions
    ADD CONSTRAINT promo_redemptions_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE CASCADE;

ALTER TABLE ONLY public.push_subscriptions
    ADD CONSTRAINT push_subscriptions_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE CASCADE;

ALTER TABLE ONLY public.reads
    ADD CONSTRAINT reads_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE CASCADE;

ALTER TABLE ONLY public.referrals
    ADD CONSTRAINT referrals_referred_id_fkey FOREIGN KEY (referred_id) REFERENCES public.users(id) ON DELETE CASCADE;

ALTER TABLE ONLY public.referrals
    ADD CONSTRAINT referrals_referrer_id_fkey FOREIGN KEY (referrer_id) REFERENCES public.users(id) ON DELETE CASCADE;

ALTER TABLE ONLY public.registration_events
    ADD CONSTRAINT registration_events_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE CASCADE;

ALTER TABLE ONLY public.reports
    ADD CONSTRAINT reports_from_id_fkey FOREIGN KEY (from_id) REFERENCES public.users(id) ON DELETE CASCADE;

ALTER TABLE ONLY public.reports
    ADD CONSTRAINT reports_to_id_fkey FOREIGN KEY (to_id) REFERENCES public.users(id) ON DELETE CASCADE;

ALTER TABLE ONLY public.snoozes
    ADD CONSTRAINT snoozes_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE CASCADE;

ALTER TABLE ONLY public.support_replies
    ADD CONSTRAINT support_replies_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE CASCADE;

ALTER TABLE ONLY public.support_tickets
    ADD CONSTRAINT support_tickets_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE SET NULL;

ALTER TABLE ONLY public.swipes
    ADD CONSTRAINT swipes_from_id_fkey FOREIGN KEY (from_id) REFERENCES public.users(id) ON DELETE CASCADE;

ALTER TABLE ONLY public.swipes
    ADD CONSTRAINT swipes_to_id_fkey FOREIGN KEY (to_id) REFERENCES public.users(id) ON DELETE CASCADE;

ALTER TABLE ONLY public.user_prompts
    ADD CONSTRAINT user_prompts_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE CASCADE;

ALTER TABLE ONLY public.user_tags
    ADD CONSTRAINT user_tags_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE CASCADE;

INSERT INTO public.promo_codes (code,days,max_uses,uses,benefit) VALUES
  ('WIRINGPLUS',90,0,0,'plus'),
  ('WIRINGFORYOU',0,0,0,'recommendations');

-- A fresh database has no rows eligible for these historical backfills.
INSERT INTO public.app_migrations (id,applied_at) VALUES
  ('beta_plus_gift_existing_users_2026_09',EXTRACT(EPOCH FROM now())::integer),
  ('beta_plus_gift_3mo_all_2026_09',EXTRACT(EPOCH FROM now())::integer),
  ('signup_plus_backfill_2026_09_26',EXTRACT(EPOCH FROM now())::integer),
  ('legacy_vibe_cleanup_2026_09',EXTRACT(EPOCH FROM now())::integer);
