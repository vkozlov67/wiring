-- Synthetic fixtures used only in the isolated developer/test Compose project.
BEGIN;
DO $fixtures$
DECLARE
  stamp integer := extract(epoch FROM now())::integer;
  dev integer;
BEGIN
  PERFORM pg_advisory_xact_lock(739201, 13);
  IF current_database() <> 'wiring_local' OR current_user <> 'wiring_local' THEN
    RAISE EXCEPTION 'Refusing to add local fixtures outside the Compose database';
  END IF;
  IF EXISTS (SELECT FROM app_migrations WHERE id='local_fixture_0001') THEN RETURN; END IF;

  INSERT INTO users(email,password_hash,name,age,city,gender,looking_for,bio,photo,
                    onboard_done,privacy_accepted_at,special_data_consent_at,photo_rights_consent_at,
                    email_verified_at,created_at,last_seen,premium_until)
  SELECT fixture.username || '@wiring.test', 'pbkdf2:sha256:1000000$wiring-local-fixture$69d15265774f277187aa9919f1b097b5f5b1e2c3b01fb28f3f66db177f9ec5f5', fixture.display_name, fixture.age, 'Нови-Сад',
         fixture.gender, 'everyone', 'Синтетическая анкета для локальной разработки.', fixture.photo,
         1, stamp, stamp, stamp, stamp, stamp, stamp, CASE WHEN username='dev' THEN stamp+31536000 ELSE 0 END
  FROM (VALUES
    ('dev', 'Дев', 27, 'other', 'portraits/p01.jpg'),
    ('peer', 'Пир', 26, 'woman', 'portraits/p02.jpg'),
    ('alma', 'Альма', 26, 'woman', 'portraits/p03.jpg'),
    ('borya', 'Боря', 31, 'man', 'portraits/p04.jpg'),
    ('vera', 'Вера', 29, 'woman', 'portraits/p05.jpg'),
    ('gleb', 'Глеб', 32, 'man', 'portraits/p06.jpg'),
    ('dana', 'Дана', 25, 'other', 'portraits/p07.jpg'),
    ('eva', 'Ева', 27, 'woman', 'portraits/p08.jpg'),
    ('zhenya', 'Женя', 28, 'other', 'portraits/p09.jpg'),
    ('ira', 'Ира', 24, 'woman', 'portraits/p01.jpg'),
    ('kostya', 'Костя', 30, 'man', 'portraits/p02.jpg'),
    ('jura', 'Юра', 29, 'man', 'portraits/p03.jpg')
  ) AS fixture(username,display_name,age,gender,photo)
  ON CONFLICT (email) DO NOTHING;

  SELECT id INTO STRICT dev FROM users WHERE email='dev@wiring.test';
  INSERT INTO photos(user_id,path,is_primary,created_at)
    SELECT id,photo,1,stamp FROM users u
    WHERE u.email LIKE '%@wiring.test' AND NOT EXISTS (SELECT FROM photos p WHERE p.user_id=u.id);
  INSERT INTO user_tags(user_id,kind,tag)
    SELECT id,'vibe','nonsmalltalk' FROM users WHERE email LIKE '%@wiring.test'
    ON CONFLICT DO NOTHING;
  INSERT INTO swipes(from_id,to_id,direction,created_at)
    SELECT dev,u.id,'like',stamp FROM users u WHERE u.email IN
      ('peer@wiring.test','alma@wiring.test','borya@wiring.test','vera@wiring.test','gleb@wiring.test','dana@wiring.test','eva@wiring.test')
    UNION ALL
    SELECT u.id,dev,'like',stamp FROM users u WHERE u.email IN
      ('peer@wiring.test','alma@wiring.test','borya@wiring.test','vera@wiring.test','gleb@wiring.test','dana@wiring.test','eva@wiring.test')
    ON CONFLICT DO NOTHING;
  INSERT INTO messages(from_id,to_id,body,created_at)
    SELECT CASE WHEN i%2=1 THEN u.id ELSE dev END, CASE WHEN i%2=1 THEN dev ELSE u.id END,
           CASE WHEN i=1 THEN 'Привет, это тестовый чат' ELSE 'Тестовое сообщение ' || i END,
           stamp-600+i*10
    FROM users u JOIN swipes s ON s.from_id=dev AND s.to_id=u.id CROSS JOIN generate_series(1,10) g(i);
  INSERT INTO app_migrations(id,applied_at) VALUES('local_fixture_0001',stamp);
END;
$fixtures$;
COMMIT;
