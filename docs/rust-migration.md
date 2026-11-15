# Перенос серверной части WIRING на Rust

**Продакшен переведён на Rust 15 ноября 2026.** Для обоих доменов nginx использует Rust. `wiring.service` остановлен и отключён. Описанные ниже промежуточные схемы сохраняются как история миграции. Рабочий порядок выпуска и отката приведён в конце.

Начальный этап охватывает `GET /api/places`. Flask проверяет веб-сессию или мобильный токен, состояние пользователя и лимит запросов. Rust выбирает, сортирует и переводит варианты из того же каталога. Нормализация поисковой строки остаётся во Flask. Сайт и мобильные приложения используют текущий контракт. PostgreSQL, сессии и схема данных остаются общими.

Каталог входит в сборку Rust-сервиса. Его следует пересобирать вместе с любыми изменениями `cities.py`, `resources/places` или переводов каталога. Сравнение на beta проверяет ответы Flask и Rust с веб-cookie и Bearer, русский и английский, порядок результатов и ошибки. Инструкции запуска beta находятся в [deploy/beta/README.md](../deploy/beta/README.md).

Публичный `GET /api/catalog` может брать готовый JSON из той же сборки через `WIRING_PLACES_CATALOG_URL=http://127.0.0.1:5081/catalog`. Flask применяет текущий язык запроса и возвращает прежнюю структуру. Снимок справочника также зависит от `catalog.py` и `glossary.py`.

## Сборка первого маршрута

Собрать образ под архитектуру сервера и извлечь бинарник с каталогом из одного образа.

```sh
docker build --target places --platform linux/amd64 -t wiring-places:release -f deploy/beta/Dockerfile .
mkdir -p wiring-places-release/catalog
places_container=$(docker create wiring-places:release)
docker cp "$places_container:/usr/local/bin/wiring-beta-places" wiring-places-release/
docker cp "$places_container:/catalog/." wiring-places-release/catalog/
docker cp "$places_container:/documents" wiring-places-release/
docker rm "$places_container"
```

Разместить релиз в отдельном каталоге под `/opt/wiring-places` и направить `/opt/wiring-places/current` на него. Установить `deploy/wiring-places.service` в systemd и запустить сервис. Бинарник по умолчанию слушает `127.0.0.1:5081`. Проверка запроса к каталогу с хоста до включения маршрута:

```sh
curl --fail --silent --show-error \
  -H 'Content-Type: application/json' \
  --data '{"needle":"tokyo","normalized_city":"","country":"","include_countries":false,"english":true}' \
  http://127.0.0.1:5081/search
```

После установки совместимой версии Flask добавить в окружение Gunicorn `WIRING_PLACES_SEARCH_URL=http://127.0.0.1:5081/search` и перезапустить Flask. При ошибке сервиса или некорректном ответе Flask использует свой поиск. Для отката убрать переменную и перезапустить Flask. Nginx, база и ключи сессий не участвуют в переключении.

## Внутренние маршруты

Flask принимает публичные запросы, проверяет веб-сессию или мобильный токен и формат данных. Публичные URL и JSON общие для сайта и приложений.

| Параметр Flask | Внутренний путь | Операция Rust |
| --- | --- | --- |
| `WIRING_FEED_FILTER_URL` | `/feed/filter` | Отбор кандидатов по стране, целям знакомства и признакам скрытия |
| `WIRING_FEED_UPDATE_URL` | `/feed/update` | Отметка просмотра и сброс выдачи по текущему поколению с сохранением исключений |
| `WIRING_FEED_CLAIM_URL` | `/feed/claim` | Случайный порядок, резервирование страницы и повторный показ при фильтрации |
| `WIRING_FEED_SWIPE_URL` | `/feed/swipe` | Лайки, постоянные пропуски и отложенные анкеты |
| `WIRING_CHAT_THREAD_URL` | `/chat/thread` | Формирование JSON истории для режима, где выборка остаётся во Flask |
| `WIRING_CHAT_HISTORY_URL` | `/chat/history` | Выбор страницы, формирование цитат, реакций и ссылок, запись прочтения |
| `WIRING_CHAT_SEND_URL` | `/chat/send` | Запись текста, отметки прочтения и уведомления с дедупликацией по `client_id` |
| `WIRING_CHAT_MEDIA_URL` | `/chat/send` | Запись фото, альбомов и голосовых сообщений |
| `WIRING_CHAT_MUTATE_URL` | `/chat/mutate` | Редактирование, мягкое удаление, добавление и снятие реакций |
| `WIRING_CHAT_PAIR_URL` | `/chat/pair` | Размэтч, блокировка, жалоба и автобан после жалоб двух разных пользователей |
| `WIRING_CHAT_TRANSCRIBE_URL` | `/chat/mutate` | Сохранение результата расшифровки |
| `WIRING_CHAT_INBOX_URL` | `/chat/inbox` | Подсчёт входящих лайков, непрочитанных сообщений и чатов |
| `WIRING_CHAT_MATCHES_URL` | `/chat/matches` | Список чатов, последнее сообщение, порядок и непрочитанные |
| `WIRING_NOTICES_URL` | `/notices/query` | Выборка непрочитанных уведомлений и отметка о прочтении |
| `WIRING_PASSWORD_URL` | `/auth/password` | Проверка существующих хешей PBKDF2-SHA256 при входе по паролю |
| `WIRING_MOBILE_REFRESH_URL` | `/auth/refresh` | Ротация мобильного refresh-токена и отзыв семейства при повторном использовании |
| `WIRING_MOBILE_LOGOUT_URL` | `/auth/logout` | Отзыв семейства токенов при выходе и снятие регистрации указанного устройства |
| `WIRING_MOBILE_ISSUE_URL` | `/auth/issue` | Сохранение новой мобильной сессии при входе по паролю |
| `WIRING_PROFILE_SETTINGS_URL` | `/profile/settings` | Запись языка, темы и согласий профиля |

Rust проверяет доступ к переписке под общей с Flask блокировкой пары. Мягкое удаление сохраняет строки сообщений в PostgreSQL. Flask валидирует и обрабатывает загруженные файлы, выдаёт медиа, обращается к сервису распознавания речи и отправляет push и почту после записи уведомления. Для ленты Flask проверяет правила совместимости и формирует анкеты. Rust резервирует страницы под блокировкой строки зрителя и проверяет текущие блокировки, инкогнито и исключения. При неподтверждённой операции с побочным эффектом API возвращает 503. Повторная вставка через Flask может создать дубликат.

## Выпуск и откат

Внутренние маршруты с БД включаются, когда Rust получает `WIRING_CHAT_DATABASE_URL` и `WIRING_CHAT_INTERNAL_TOKEN` длиной от 32 символов. Flask получает тот же токен и URL включённых маршрутов. В systemd параметры БД и токен хранятся в отдельных файлах с правами 0600. На проде Rust слушает `127.0.0.1:5081`. Права PostgreSQL-роли `wiring_chat_writer` находятся в [deploy/beta/grants](../deploy/beta/grants).

При обновлении мобильной сессии Flask генерирует новый refresh-токен и передаёт Rust хеши старого и нового токенов. Rust проверяет срок действия и отпечаток пароля, затем атомарно ротирует токен. Flask подписывает access-токен в существующем формате.

Сначала проверить артефакт на отдельной базе beta и в закрытом beta-приложении. Продакшен использует тот же бинарник и каталог. Drop-in с URL лежат в `/etc/systemd/system/wiring.service.d/`. Активный бинарник находится по `/opt/wiring-places/current`, резервные копии файлов Flask находятся в `rollback/` каждого выпуска. Для отключения отдельной операции убрать её переменную из drop-in, выполнить `systemctl daemon-reload` и перезапустить `wiring.service`.

Выпуски Rust установлены вручную. Исходники адаптеров должны попасть в следующий обычный деплой Flask. При изменении каталога пересобрать Rust-артефакт вместе с Flask.

## Публичные маршруты Rust

Публичный слушатель на `127.0.0.1:5082` отделён от внутренних операций на `5081`. Nginx отправляет на него `/account-deletion`, `/child-safety`, `/privacy`, `/legal`, `/rules`, `/marketing`, `/glossary`, `/robots.txt`, `/sitemap.xml` и два файла привязки приложений из `/.well-known/`. На старом домене Rust обслуживает только эти два файла привязки, а страницы сохраняют редирект. Юридический HTML входит в артефакт `documents/` и собирается из общих с Flask шаблонов и переводов; в процессе Rust Python не нужен.

Перед переключением запустить `deploy/beta/compare_public.py` с адресами Flask и Rust, затем повторить проверку через прокси beta. Артефакт должен содержать каталог и документы; параметры домена, аналитики и публичный отпечаток Android хранятся в `/etc/wiring-public.env`. Чтобы вернуть эти маршруты во Flask, убрать файлы `wiring-rust-{canonical,legacy}-documents.conf` из `/etc/nginx/snippets/`, выполнить `nginx -t` и перезагрузить nginx.

`POST /api/auth/refresh` и `POST /api/auth/logout` направляются в Rust на обоих API-доменах. Rust использует общую таблицу сессий, отзывает семейство при повторном использовании refresh-токена и подписывает access-токены в формате, который принимает Flask. Для переключения нужны `WIRING_PUBLIC_MOBILE_SESSIONS=1` и отдельный root-only файл `/etc/wiring-mobile-rust.env` с тем же ключом подписи и порогом версии, что у Flask. При изменении этих настроек обновить оба сервиса. Чтобы вернуть маршруты во Flask, убрать файлы `wiring-rust-{canonical,legacy}-mobile.conf` из nginx и перезагрузить его. Токены, выпущенные Rust, продолжат работать после отката.

`GET /api/inbox` и `POST /api/notices/read` на обоих доменах принимают существующие веб-cookie и мобильные токены. Rust проверяет подпись, срок и отпечаток пароля для мобильной сессии; учитывает ограничения аккаунта и обновляет `last_seen`. Для включения нужны `WIRING_PUBLIC_INBOX=1` и права из `15-public-inbox.sql`. Для отката убрать файлы `wiring-rust-{canonical,legacy}-inbox.conf` из nginx и перезагрузить его.

На `wiring.club` Rust отдаёт HTML оболочки для главной страницы, входа, ленты, чатов, профилей и других маршрутов из `spa_http.rs`. Он принимает и обновляет Flask cookie, потребляет маркер OAuth-регистрации один раз и сохраняет статистику устройств. Список URL синхронизируется с `nginx-wiring-rust-canonical-spa.conf` и `Caddyfile.spa`. Для включения нужны `WIRING_PUBLIC_SPA=1`, файлы `spa-*.html` из сборки и права из `16-public-spa.sql`. Для отката убрать `wiring-rust-canonical-spa.conf` и перезагрузить nginx.

`/health` на основном домене и `/api/catalog`, `/api/app-config` на обоих доменах включаются через `WIRING_PUBLIC_SERVICE_INFO=1`. Health проверяет PostgreSQL; справочник собирается для двух языков. Параметры минимальной и последней версии мобильного клиента должны совпадать с Flask. Проверки `compare_info.py` сравнивают ответы, включая поведение для ошибочных токенов. Для отката убрать `wiring-rust-{canonical,legacy}-info.conf` из nginx и перезагрузить его.

Регистрация и удаление мобильного устройства по `/api/push/device` работают через Rust на обоих доменах. Для включения нужны `WIRING_PUBLIC_PUSH_DEVICE=1` и права из `17-public-push-device.sql`. При удалении Rust ограничивает запрос владельцем устройства. Доставка уведомлений остаётся во Flask. Для отката убрать `wiring-rust-{canonical,legacy}-device.conf` из nginx и перезагрузить его.

Rust отдаёт `/m` на основном домене и локализованный `/public/manifest.webmanifest` на обоих доменах. Сборка содержит варианты лендинга для пяти тем, двух языков и наличия ссылок для Android и iOS. Для включения нужны `WIRING_PUBLIC_LANDING=1` и ссылки из `/etc/wiring-mobile-links.env`. Для отката убрать `wiring-rust-{canonical,legacy}-landing.conf` и перезагрузить nginx.

`POST /api/logout` на обоих доменах удаляет веб-cookie через Rust. Для включения нужен `WIRING_PUBLIC_WEB_LOGOUT=1`. Для отката убрать `wiring-rust-{canonical,legacy}-web-logout.conf` и перезагрузить nginx.

`GET /api/me` и `HEAD /api/me` на обоих доменах обслуживает Rust. Для включения нужны `WIRING_PUBLIC_PROFILE_READ=1`, `profile-rules.json` из сборки, права из `18-public-profile-read.sql` и булева настройка `WIRING_JEV_CONFIGURED`, отражающая наличие ключа рекомендаций у Flask. Для отката убрать `wiring-rust-{canonical,legacy}-profile.conf` и перезагрузить nginx.

Веб-вход по паролю (`POST /api/login`) и выдача мобильной сессии (`POST /api/auth/token`) работают через Rust на обоих доменах. Сохраняются подтверждение почты, семидневный срок восстановления и форматы Flask cookie и токенов. Оба маршрута делят лимит в 20 попыток за пять минут с одного IP. Для включения нужны `WIRING_PUBLIC_PASSWORD_LOGIN=1` и права из `19-public-login.sql`.

`POST /api/email/verify`, `POST /api/password/reset` и `POST /api/auth/native/exchange` также обслуживает Rust. Проверка срока действия и погашение кода выполняются под блокировкой строки; для мобильного клиента refresh-токен сохраняется в той же транзакции. Для включения нужны `WIRING_PUBLIC_ACCOUNT_TOKENS=1`, `WIRING_PUBLIC_NATIVE_OAUTH_EXCHANGE=1` и права из `20-public-account-tokens.sql`. Для отката убрать пары nginx snippets `wiring-rust-{canonical,legacy}-login.conf` или `wiring-rust-{canonical,legacy}-account-tokens.conf` и перезагрузить nginx.

`POST /api/register`, `POST /api/email/resend` и `POST /api/password/forgot` работают через Rust. Регистрация сохраняет согласия, реферальную награду, событие и код подтверждения в одной транзакции. Продакшен отправляет ссылки через [Resend](https://resend.com/docs/api-reference/emails/send-email); в закрытой бете доставка отключена. Для включения нужны `WIRING_PUBLIC_REGISTRATION=1` и права из `21-public-registration.sql`. Для отката убрать пару `wiring-rust-{canonical,legacy}-registration.conf` и перезагрузить nginx.

На обоих доменах Rust обслуживает список OAuth-провайдеров, начало входа, переход из мобильного браузера, callbacks и возврат в приложение. Начало и callback используют прежнюю cookie, таблицу состояний и те же адреса перенаправления. Для включения нужны `WIRING_PUBLIC_OAUTH_START=1`, `WIRING_PUBLIC_OAUTH_CALLBACK=1` и права из `22-public-oauth-start.sql` и `23-public-oauth-callback.sql`. Откат возможен по группам: убрать пару `wiring-rust-{canonical,legacy}-oauth-start.conf` или `wiring-rust-{canonical,legacy}-oauth-callback.conf` и перезагрузить nginx. При выпуске проверены продовые маршруты, состояние и отмена для всех трёх провайдеров; успешный внешний обмен с реальным аккаунтом не проверялся.

`POST /api/analytics/registration` принимает подписанную cookie регистрации через Rust. Для включения нужны `WIRING_PUBLIC_REGISTRATION_ATTRIBUTION=1` и права из `24-public-registration-attribution.sql`. Для отката убрать пару `wiring-rust-{canonical,legacy}-attribution.conf` и перезагрузить nginx.

Язык, тема, согласия и переключатель рекомендаций, а также `POST /api/me/delete` и `DELETE /api/me` работают через Rust. Удаление ставит `deleted_at` и очищает веб-сессию. Права из `25-public-account-settings.sql`; настройки `WIRING_PUBLIC_ACCOUNT_SETTINGS=1` и `WIRING_PUBLIC_ACCOUNT_DELETE=1`. Для отката убрать пару `wiring-rust-{canonical,legacy}-account-settings.conf` и `wiring-rust-profile-method-delete.conf` и перезагрузить nginx. Фоновая очистка данных после срока восстановления пока выполняется Python.

`POST /api/block` и `POST /api/unmatch` обслуживает Rust (`WIRING_PUBLIC_PAIR_CONTROLS=1`). Обе операции скрывают переписку через `messages.deleted_at` и используют общую блокировку пары. Для отката убрать пару `wiring-rust-{canonical,legacy}-pair-controls.conf` и перезагрузить nginx.

Таймер `wiring-registrations.timer` запускает Rust-команду `sync-registrations` через drop-in `wiring-registrations-rust.conf`. Используются права из `26-registration-worker.sql`. Состояние доставки сохраняется до отправки: при неясном результате запись ждёт ручной сверки, чтобы исключить повторную отправку. Для отката остановить таймер, дождаться завершения текущей задачи, убрать drop-in и запустить таймер снова после `systemctl daemon-reload`.

`GET /api/support/replies` на обоих доменах обслуживает Rust. Запрос читает до 40 ответов только для владельца сессии и доступен после блокировки аккаунта по жалобам. Для включения нужны `WIRING_PUBLIC_SUPPORT_REPLIES=1` и права из `27-public-support-replies.sql`. Для отката убрать пару `wiring-rust-{canonical,legacy}-support-replies.conf` и перезагрузить nginx.

Правки текста (`PATCH /api/messages/{id}`) и реакции (`PUT /api/messages/{id}/reaction`) переключены на Rust. Права из `02-chat-mutations.sql`; настройка `WIRING_PUBLIC_MESSAGE_EDITS=1`. История и удаление сообщений маршрутизируются отдельно. Для отката убрать пару `wiring-rust-{canonical,legacy}-message-edits.conf` и перезагрузить nginx.

Отметка просмотра, сброс ленты и ответ старым клиентам на попытку перезапуска (`POST /api/feed/view`, `/api/feed/reset`, `/api/deck/restart`) работают через Rust. Для включения нужны `WIRING_PUBLIC_FEED_DELIVERY=1` и права из `05-feed-update.sql`. Сброс сохраняет постоянные исключения. Для отката убрать пару `wiring-rust-{canonical,legacy}-feed-delivery.conf` и перезагрузить nginx.

Rust обслуживает `GET /api/people/{id}` и `GET /api/matches` (`WIRING_PUBLIC_PEOPLE_READ=1`). Для карточки другого участника выбирается публичный набор полей; почта, настройки и уведомления доступны только владельцу. [Проверка совместимости](../deploy/check-rust-social.sh) сравнивает Flask и Rust на временной локальной базе с синтетическими веб- и мобильными аккаунтами. Для отката убрать пару `wiring-rust-{canonical,legacy}-people-read.conf` и перезагрузить nginx.

Чтение, переименование и удаление альбомов, совместимый ответ на попытку создать альбом и `POST /api/onboard/skip` обслуживает Rust. Удаление альбома сохраняет фотографии. Для включения нужны `WIRING_PUBLIC_ALBUMS=1`, `WIRING_PUBLIC_ONBOARD_SKIP=1` и права из `28-public-albums.sql`. Для отката убрать пару `wiring-rust-{canonical,legacy}-albums.conf` и перезагрузить nginx.

Nginx отдаёт `/sw.js` и обычные файлы из `/public/` из `/var/www/wiring-static/`. Скрипт `deploy.sh` обновляет каталог из `public/` после сборки. Динамический манифест и миниатюры с `s=sm` отдаёт Rust. Для отката убрать пару `wiring-rust-{canonical,legacy}-static.conf` и перезагрузить nginx.

`GET /api/places` и `HEAD /api/places` на обоих доменах обслуживает Rust, включая проверку сессии и лимит 180 запросов в минуту на пользователя. Для включения нужен `WIRING_PUBLIC_PLACES=1`. Ответы сравниваются на двух языках с латиницей, кириллицей, псевдонимами и фильтрами стран. Для отката убрать пару `wiring-rust-{canonical,legacy}-places.conf` и перезагрузить nginx.

`PATCH /api/plus` и `POST /api/premium/redeem` работают через Rust (`WIRING_PUBLIC_PREMIUM=1`, права из `29-public-premium.sql`). Проверка лимита, запись активации и продление подписки выполняются в одной транзакции с блокировкой промокода. Для отката убрать пару `wiring-rust-{canonical,legacy}-premium.conf` и перезагрузить nginx.

`PATCH /api/notifications` и чтение истории `GET /api/messages/{id}` работают через Rust. Отключение push очищает подписки и устройства только этого аккаунта. История сохраняет пагинацию, реакции, отметки прочтения и подсказки. Права из `30-public-notification-settings.sql` и `31-public-chat-history.sql`; флаги `WIRING_PUBLIC_NOTIFICATION_SETTINGS=1` и `WIRING_PUBLIC_CHAT_HISTORY=1`. Для отката убрать пару `wiring-rust-{canonical,legacy}-notification-settings.conf` или `wiring-rust-message-method-history.conf` и перезагрузить nginx.

`GET /api/archive` и `POST /api/archive` обслуживает Rust (`WIRING_PUBLIC_ARCHIVE=1`, права из `32-public-archive.sql`). Разрыв пары из архива использует ту же мягкую очистку переписки, что и чат. Для отката убрать пару `wiring-rust-{canonical,legacy}-archive.conf` и перезагрузить nginx.

Запрос публичного VAPID-ключа, подписка и отписка браузера обслуживаются Rust (`WIRING_PUBLIC_BROWSER_PUSH=1`, права из `34-public-browser-push.sql`). Сохраняется существующий ключ и формат подписок. При использовании `WEB_PUSH_VAPID_PRIVATE_KEY` в Flask нужно передать соответствующий публичный ключ в `WIRING_VAPID_PUBLIC_KEY` для Rust. Для отката убрать пару `wiring-rust-{canonical,legacy}-browser-push.conf` и перезагрузить nginx.

Проверка города и сохранение города профиля (`POST /api/places/resolve`, `POST /api/me/city`) работают через Rust. Они используют общий каталог, кеш `place_resolutions` и существующую проверку неизвестных названий. Нужны `WIRING_PUBLIC_PLACE_VALIDATION=1`, `place-countries.json` и права из `35-public-place-validation.sql`. `POST /api/rewind` включён через `WIRING_PUBLIC_REWIND=1`; постоянное исключение и взаимный лайк по-прежнему нельзя отменить этим маршрутом. Для отката убрать соответствующую пару `wiring-rust-{canonical,legacy}-{place-validation,rewind}.conf` и перезагрузить nginx.

Порядок фотографий, основная фотография и перенос в существующий альбом обслуживаются через `PATCH /api/photos/order` и `PATCH /api/photos/{id}` в Rust (`WIRING_PUBLIC_PHOTO_METADATA=1`, права из `36-public-photo-metadata.sql`). Для отката убрать пару `wiring-rust-{canonical,legacy}-photo-metadata.conf` и перезагрузить nginx.

Выдача `GET /api/feed` и `GET /api/likes` работает напрямую через Rust (`WIRING_PUBLIC_DISCOVERY=1`, права из `37-public-discovery.sql`). Сохраняются фильтры, правила видимости и история выдачи в PostgreSQL; без WIRING+ входящие лайки остаются скрытыми. Для отката убрать пару `wiring-rust-{canonical,legacy}-discovery.conf` и перезагрузить nginx.

Веб-оболочка `GET /support` и форма `POST /support` на `wiring.club` работают через Rust (`WIRING_PUBLIC_SUPPORT_FORM=1`, право из `42-public-support-form.sql`). GET и POST принимают веб-сессию и мобильный Bearer; POST принимает JSON и браузерные формы. На тестовой базе проверены привязка обращения, валидация, HTML и отклонение недействительного мобильного токена. Отправку в рабочую почту не проверяли. Для отката POST и мобильных GET убрать `wiring-rust-support-shell-backend.conf` и перезагрузить nginx.

`PATCH /api/me` обслуживает Rust (`WIRING_PUBLIC_PROFILE_EDIT=1`, права из `38-public-profile-edit.sql`). Проверка сохраняет правила возраста, согласий, фотографии, черновиков и ограничения смены имени. Для отката убрать `wiring-rust-profile-method-edit.conf` и перезагрузить nginx.

`POST /api/messages` обслуживает Rust (`WIRING_PUBLIC_MESSAGE_SEND=1`, права из `39-public-message-send.sql`). Текст, ответ на сообщение и уведомление записываются с дедупликацией по `client_id`. Доставка на устройства начинается после записи сообщения. Проверены шифрование браузерного push по [RFC 8291](https://www.rfc-editor.org/rfc/rfc8291.html) и операции в базе; фактическая доставка на устройство отдельно не проверялась. Для отката убрать пару `wiring-rust-{canonical,legacy}-message-send.conf` и перезагрузить nginx.

`POST /api/swipe` обслуживает Rust (`WIRING_PUBLIC_SWIPE=1`). Запросы с веб-сессией и мобильным токеном сравнены с Flask на отдельной базе на прод-хосте. При переключении проверено, что на проде нет активных анкет с `is_seed`. Для отката убрать пару `wiring-rust-{canonical,legacy}-swipe.conf` и перезагрузить nginx. Если активные seed-анкеты вернутся, нужно сначала проверить совместимость логики автоматического ответа.

`GET /api/recommendations` работает через Rust (`WIRING_PUBLIC_RECOMMENDATIONS=1`). На отдельной базе сравнены доступ, выдача анкет и порядок без вызова Jev. Сохраняется прежняя конфигурация Jev; вызов с реальными профилями в проверку не входил. Для отката убрать пару `wiring-rust-{canonical,legacy}-recommendations.conf` и перезагрузить nginx.

`POST /api/report` работает через Rust (`WIRING_PUBLIC_REPORT=1`). Жалоба и блокировка записываются в одной транзакции, сообщения получают `deleted_at`. На временной базе проверены запись жалобы и мягкое удаление, без отправки уведомлений. В проде используются существующие настройки уведомлений. Для отката убрать пару `wiring-rust-{canonical,legacy}-report.conf` и перезагрузить nginx.

Продовый `wiring-purge.timer` запускает Rust-команду `purge-accounts --apply` с отдельной ролью PostgreSQL (`40-purge-worker.sql`). Срок восстановления в семь дней сохраняется. На отдельной базе проверены данные, файлы и повторный запуск; для рабочей базы при переключении выполнен только режим `--plan`. Для отката убрать `/etc/systemd/system/wiring-purge.service.d/rust.conf` и перечитать юниты systemd.

`POST /api/demo` обслуживает Rust (`WIRING_PUBLIC_DEMO=1`) и выдаёт совместимую с Flask сессию. Гости старше трёх дней очищаются по существующему расписанию через право из `41-guest-cleanup.sql`; функция PostgreSQL проверяет тип и возраст аккаунта сама. На отдельной базе проверено, что свежие гости и обычные аккаунты сохраняются. Для отката убрать пару `wiring-rust-{canonical,legacy}-demo.conf`, файл `/etc/systemd/system/wiring-purge.service.d/guest.conf` и перечитать nginx и systemd.

Выдача вложений чата, фотографий профиля и миниатюр обслуживается Rust (`WIRING_PUBLIC_MEDIA=1`). Nginx читает файлы через внутренние маршруты только после проверки доступа. На тестовых файлах проверены `HEAD`, `Range`, приватный кеш и невозможность прямого запроса внутреннего URL. Файлы приватных миниатюр и аудио находятся в `/var/lib/wiring/rust-media-cache`; таймер очистки удаляет соответствующие записи кеша. Для отката убрать пару `wiring-rust-{canonical,legacy}-media.conf` и `wiring-rust-static-thumbnail-backend.conf`, затем перезагрузить nginx.

`DELETE /api/messages/{id}` обслуживает Rust (`WIRING_PUBLIC_MESSAGE_DELETE=1`). Операция доступна автору в действующей переписке; сохраняет строку с `deleted_at` и удаляет файлы вложения и его кеш. Для отката убрать `wiring-rust-message-method-delete.conf` и перезагрузить nginx.

`DELETE /api/photos/{id}` работает через Rust (`WIRING_PUBLIC_PHOTO_DELETE=1`, право из `43-public-photo-delete.sql`). Удаляется только фото владельца, последнее фото защищено, основное выбирается из оставшихся. Для отката убрать `wiring-rust-photo-method-delete.conf` и перезагрузить nginx.

`POST /api/messages/voice` работает через Rust (`WIRING_PUBLIC_VOICE_UPLOAD=1`). Тест проверяет загрузку синтетического WAV с мобильным токеном, ссылку на сообщение, предел длительности и формат воспроизведения. Для отката убрать пару `wiring-rust-{canonical,legacy}-voice-upload.conf` и перезагрузить nginx.

Расшифровка голосовых включена в Rust (`WIRING_PUBLIC_TRANSCRIBE=1`). Сохранённая расшифровка и ошибки проверены на синтетических данных.

Фото профиля и чата загружаются через Rust (`WIRING_PUBLIC_PHOTO_UPLOAD=1`). На тестовых изображениях проверена загрузка без вызова внешнего провайдера; на проде уменьшенные копии по-прежнему проверяет OpenAI Moderations.

Новая пустая база PostgreSQL может быть создана командой `wiring-beta-places init-schema` с `WIRING_SCHEMA_DATABASE_URL` владельца схемы. Команда загружает `schema/0001-baseline.sql`, создаёт стандартные промокоды и не создаёт аккаунты. Повторный запуск ничего не меняет; на базе, созданной прежним приложением, команда отказывается выполнять изменения. После создания базы отдельно применяются нужные права из `deploy/beta/grants/`. На рабочей базе команда не запускалась.

Действия админки для WIRING+, промокодов, снятия автобана и задач (включая импорт обращений) обслуживает Rust (`WIRING_PUBLIC_ADMIN_ACTIONS=1`, права из `44-admin-actions.sql`). Они принимают существующую подписанную сессию администратора и требуют `Origin` основного домена. На временной базе проверены отказ для обычной сессии, сохранение результатов и дедупликация. Для отката убрать `wiring-rust-canonical-admin-actions.conf` и перезагрузить nginx.

Админ-страница `/admin` и защищённая выгрузка `/admin/export/marketing-audience` работают через Rust (`WIRING_PUBLIC_ADMIN_PAGE=1`, права из `45-admin-dashboard.sql`). Форма входа проверяет `Origin` и токен из подписанной сессии; страницы и выгрузка не кешируются. На временной базе проверены обе локализации и отказ выгрузки без админской сессии. Для отката убрать `wiring-rust-canonical-admin-page.conf` и перезагрузить nginx. Почтовые ответы администратора отправляет Rust через Resend (`WIRING_PUBLIC_ADMIN_REPLIES=1`). Логика проверена без пробных писем в рабочие ящики.

Таймер SendPulse запускает Rust-команду `sync-sendpulse` с прежней адресной книгой и прежними условиями согласия.

## Завершение переключения

На продакшене включены четыре последние реализации: модерация уменьшенных копий фото (профиль и чат), OpenAI Transcriptions по запросу пользователя, ответы админки через Resend и синхронизация согласившихся контактов через SendPulse. Используются те же получатели и параметры. Rust также выполняет удаление просроченных аккаунтов и доставку конверсий регистрации. Резервное копирование PostgreSQL остаётся системной задачей.

[Проверка на сервере](../deploy/check-flask-retirement.sh) проверяет оба каталога через TLS, флаги интеграций и отсутствие прокси на Flask. После остановки Gunicorn отдельно проверены HTML, статические файлы и миниатюры, публичный каталог и параметры OAuth, авторизация API, поддержка и админка. Вход через реальные аккаунты Google, Яндекса и Apple и отправка пробных писем в рабочие ящики не входили в этот прогон.

## Публикация и откат

GitHub Actions собирает клиент и Linux-бинарник вместе с каталогом и документами в задаче проверок, а затем выкладывает тот же артефакт. Ручная сборка использует `deploy/build-rust-release.sh`. `deploy.sh` переключает символьную ссылку `/opt/wiring-places/current` и публикует `/var/www/wiring-static`. До и после переключения он проверяет оба домена, отсутствие Flask в nginx, выключенный сервис и закрытый порт `5070`. При ошибке после переключения он возвращает предыдущий выпуск и файлы клиента. Конфигурация nginx и systemd управляется отдельно. База PostgreSQL и каталог пользовательских загрузок при выкладке не заменяются.

Для отката самого переключения с Flask есть отдельный серверный снимок nginx и systemd с `restore.sh` под `/root/wiring-flask-retirement.*`. Обычный откат приложения возвращает предыдущий Rust-релиз.
