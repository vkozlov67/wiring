# Закрытая beta WIRING

На отдельном Docker-хосте Caddy принимает HTTPS для `beta.wiring.club` и отдаёт приложение только IP-адресам из списка доступа. Flask, Rust и PostgreSQL находятся во внутренней сети Docker. У базы и обоих приложений нет опубликованных портов. PostgreSQL, медиа и сертификаты хранятся в отдельных томах проекта `wiring-beta`. На сервер приложения передаются только настройки beta. Отправка писем, OAuth и аналитика не настраиваются, внутренняя сеть блокирует внешний транспорт push.

## Запуск Flask

1. На выделенном тестовом хосте с Docker Compose v2 подготовить копию исходников и открыть входящий TCP на портах 80 и 443. В DNS-зоне `wiring.club` добавить запись A с именем `beta` и IPv4-адресом тестового хоста. Если хост принимает IPv6, можно добавить AAAA и указать IPv6 тестировщиков. Записи должны вести прямо на Caddy, поскольку он проверяет адрес TCP-соединения. Порт 80 используется для автоматического HTTPS.
2. Из корня репозитория создать файл параметров beta. Указать реальные адреса тестировщиков. Оба секрета должны быть независимыми случайными строками.

   ```sh
   umask 077
   cat > deploy/beta/.env <<EOF_BETA
   BETA_ALLOWED_IPS=203.0.113.10/32 2001:db8::10/128
   BETA_DB_PASSWORD=$(openssl rand -hex 24)
   BETA_SESSION_SECRET=$(openssl rand -hex 32)
   BETA_PLACES_BACKEND=flask
   EOF_BETA
   ```

3. Собрать и запустить beta, затем создать синтетические анкеты. Тестовые пользователи `dev@wiring.test` и `peer@wiring.test` входят с паролем `wiring-dev`. Доступ к окружению ограничивает Caddy.

   ```sh
   beta() { docker compose --env-file deploy/beta/.env -f deploy/beta/compose.yml --profile standalone "$@"; }
   beta up -d --build --wait
   beta exec -T flask python scripts/ensure_dev_user.py
   python3 deploy/beta/smoke.py https://beta.wiring.club --backend flask --write /tmp/wiring-beta-flask.json
   ```

4. С IP вне списка проверить, что HTTPS возвращает 403. С разрешённого IP в браузере войти и открыть чат и выбор города. Для отдельной сборки iOS или Android задать `EXPO_PUBLIC_API_URL=https://beta.wiring.club` в конфигурации Expo и проверять с разрешённого IP. Сборки для пользователей продолжают использовать свой обычный адрес API.

Для локальной проверки в том же файле дополнительно задать `BETA_SITE=http://localhost`, `BETA_ORIGIN=http://localhost:5080`, `BETA_BIND=127.0.0.1`, `BETA_HTTP_PORT=5080`, `BETA_HTTPS_PORT=5443`, `BETA_SECURE_COOKIE=0` и `BETA_ALLOWED_IPS=private_ranges`. Передать `http://localhost:5080` скрипту проверки.

## Хост с существующим nginx

На `188.166.105.108` используется каталог `/opt/wiring-beta` и конфигурация Compose с прокси Caddy, доступным только на loopback. Внешний nginx завершает TLS и проверяет IP тестировщика.

```sh
beta() { docker compose --env-file deploy/beta/.env -f deploy/beta/compose.yml -f deploy/beta/compose.nginx.yml --profile standalone "$@"; }
beta up -d --build --wait
beta exec -T flask python scripts/ensure_dev_user.py
python3 deploy/beta/smoke.py http://localhost:5080 --loopback-proxy --backend flask --write /tmp/wiring-beta-flask.json
```

`nginx-http.conf` выделяет путь ACME в `/var/www/wiring-beta-acme`. После появления DNS-записи выпустить отдельный сертификат через `certbot certonly --webroot -w /var/www/wiring-beta-acme -d beta.wiring.club`. Добавить `nginx-https.conf` к конфигурации этого же vhost и указать разрешённые адреса директивами `allow` в `/opt/wiring-beta/allowed-nginx.conf`. Перед каждой перезагрузкой nginx запустить `nginx -t`. После настройки HTTPS повторить smoke по домену и проверить 403 с IP вне списка.

## Переключение поиска и откат

Запустить Rust отдельно, сохранив Flask для входа, сессий и остальных маршрутов.

```sh
beta --profile rust up -d --build --wait places
```

Изменить `BETA_PLACES_BACKEND` на `rust` в параметрах beta и пересоздать только Flask. После проверки доступа и лимита Flask передаёт Rust нормализованный запрос и параметры поиска. Rust отбирает, сортирует, переводит результаты `GET /api/places` и убирает дубли. `HEAD`, `OPTIONS` и `POST /api/places/resolve` обслуживает Flask.

```sh
beta up -d --no-deps --wait flask
python3 deploy/beta/smoke.py https://beta.wiring.club --backend rust --compare /tmp/wiring-beta-flask.json
```

Для отката установить `BETA_PLACES_BACKEND=flask` и повторить `beta up -d --no-deps --wait flask`. Затем запустить smoke с `--backend flask --compare /tmp/wiring-beta-flask.json` и проверить выбор города в уже открытой сессии.

## Rust и PostgreSQL

После инициализации схемы Flask создать на beta роль `wiring_chat_writer` с отдельным паролем и применить `grants/*.sql` в порядке номеров для включаемых операций. В Compose overlay добавить Rust-сервису `WIRING_CHAT_DATABASE_URL` для `wiring_beta`, обоим приложениям передать общий `WIRING_CHAT_INTERNAL_TOKEN` из закрытого файла окружения. Включить нужные URL для Flask из [списка маршрутов](../../docs/rust-migration.md), используя адрес `http://places:5081`. Подключать Rust к `beta` и оставлять внутренний порт неопубликованным.

Для схемы с nginx дополнительные Compose overlays подключаются после `compose.nginx.yml` и настроек внутреннего API: `public`, `mobile`, `inbox`, `spa`, `info`, `device`, `landing`, `web-logout`, `profile-read` (файлы `compose.<имя>.yml`). Последний overlay выбирает Caddyfile с полным набором включённых публичных маршрутов. Имена образов в overlays относятся к проверенным выпускам; для новой сборки указать свой образ в последнем overlay. Публичный слушатель Rust на `5082` остаётся в закрытой сети, а внешний nginx продолжает проверять IP.

Сначала пересоздать только `places`. В контейнере Flask запустить соответствующий проверяемому выпуску `compare_*.py`. Затем пересоздать `edge` и повторить проверку через него. Сценарии с тестовыми учётными данными и cookies запускать только в beta.
