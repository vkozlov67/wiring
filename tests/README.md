# Tests

| Suite | Command | What it covers |
|-------|---------|----------------|
| **Typecheck** | `npm run typecheck` | Strict TypeScript and Vite configuration check |
| **Languages** | `npm run test:i18n` and Python `test_i18n.py` | English coverage, placeholders, original member text, static legal/glossary copy, canonical catalog ids, persistence and failure/guest paths |
| **Languages in browser (optional)** | `npm run test:i18n:browser` (after build, same Playwright overrides) | English screens, RU/EN switching, saved preference and chat drafts, unchanged member content and mobile/desktop layout |
| **Router** | `npm run test:router` (after `npm run build`) | SPA paths, guards, `hrefFor` — `tests/router.test.mjs` |
| **Client API** | `npm run test:api` | Support JSON payload and case-insensitive content type, same-origin credentials, language header and multipart boundary |
| **Deck browser (optional)** | `npm run test:deck:browser` (after build, Playwright installed) | Built deck and recommendations: touch inertia, cancellation, resize, photo/keyboard navigation, action buttons and recommendation position restoration after navigation (fresh/reordered/removed candidates, exhausted list, retry, explicit refresh and aborted response) |
| **UI bug regressions (optional)** | `npm run test:ui:browser` (after build, same Playwright overrides) | Incoming like actions, confirmation/cancel/error, duplicate and navigation guards, bottom profile back, country search and unread badge alignment |
| **Trello regressions 31 October (optional)** | `npm run test:trello:browser` (after build, same Playwright overrides) | Full shell: saved diagnosis edits → fresh recommendations; crop cancel/done hit tests at mobile/landscape sizes; late chat images after 2.5s and keyboard resize while following latest or reading history |
| **Admin tabs (optional)** | `npm run test:admin:browser` (after build, same Playwright overrides; local `.venv` required) | Real Jinja template with synthetic data, tab history/deep links, keyboard, retained reply drafts, mobile overflow, no-JS fallback; no database or production account |
| **Rust HTTP API** | `./scripts/test-api.sh` | Routes, sessions, registration, consent, city catalog, chat, media, support and admin via nginx and PostgreSQL |
| **All** | `./scripts/test.sh` | Frontend checks, editorial data checks and the Rust HTTP suite |

The Rust suite lives in `deploy/beta/places/tests/http_api.rs`. It runs in a disposable Compose project with synthetic data, local secrets and no access to external providers. It makes no changes in an existing development or production database.

When adding a SPA URL, update `spa_http.rs`, `src/router/routes.ts`, the route contract in `http_api.rs` and the router test if matching changes.

The browser suite serves only local built assets and synthetic profiles, mocks API responses, and blocks external network requests. It does not use PostgreSQL or a production login. It requires an existing Playwright installation and Chromium browser; they are not runtime dependencies of WIRING and are not required by the default CI suite. `PLAYWRIGHT_MODULE_PATH` may point to an external Playwright `index.mjs`, and `PLAYWRIGHT_CHROMIUM_EXECUTABLE` may select an installed Chrome executable. Without overrides it uses `import("playwright")` and Playwright's default Chromium. This is mobile emulation, not a Safari or physical-device certification.


Own-profile browser regression: `npm run test:profile:browser` after build, with the same Playwright overrides. It uses the full shell, mocked local APIs, and verifies `/me` preview → explicit `/me/edit` → reload → profile-tab preview, without an implicit profile save. Optional `PUBLISHED_ASSETS=1` runs the same assertions on public assets after release. `PUBLISHED_BASELINE=1` reproduces the pre-fix editor entry using public assets only; it never reads a production account or sends a production API mutation.

Реакции: `node tests/chat-reactions-browser.test.mjs` после сборки (те же Playwright overrides) проверяет текст/фото/голосовые, замену/снятие, перезагрузку, ошибку сохранения, двойное нажатие и ширину 320/390/1024. Rust проверяет идемпотентность, чужой доступ и сохранение скрытых сообщений в PostgreSQL после unmatch, block и report.

Push: `node --test tests/web-push.test.mjs` проверяет первую установку service worker, смену VAPID key, отсутствие дублирования polling и обработку push без открытого окна. `wiring-mobile/tests/pushRegistration.test.mjs` проверяет регистрацию при старте/foreground/ротации токена, повтор после ошибки и отмену при logout.

`npm run test:push` включён в `scripts/test.sh`. Дополнительно `npm run test:push:browser` (Playwright overrides как выше) закрывает все страницы локального сайта, доставляет синтетическое событие через Chromium CDP в настоящий service worker и проверяет созданное системное уведомление. Этот тест не проверяет внешний Web Push transport, APNs, FCM или физическое устройство.



`node tests/archive-browser.test.mjs` проверяет четыре вкладки архива в собранном shell на 320/390/1280 px: раздельные списки и счётчики, переключение клавиатурой, путь «Скрыть → Исключить → Дизлайки», отмена подтверждения, ошибка и повтор, возврат анкеты. API синтетический, нужны Playwright и Chromium (можно задать `PLAYWRIGHT_MODULE_PATH` и `PLAYWRIGHT_CHROMIUM_EXECUTABLE`).
