use std::{env, time::Duration};

use reqwest::{Client, Method, RequestBuilder, Response, StatusCode, Url, header};
use serde_json::{Value, json};
use tokio_postgres::{NoTls, Client as Db};

struct Api {
    http: Client,
    host: String,
}

impl Api {
    fn new() -> Self {
        // The test image can reach only its own Compose network.
        assert_eq!(env::var("WIRING_HTTP_TEST_ORIGIN").as_deref(), Ok("http://proxy"));
        Self {
            http: Client::builder().no_proxy().redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(15)).build().unwrap(),
            host: env::var("WIRING_HTTP_TEST_HOST").unwrap(),
        }
    }

    fn request(&self, method: Method, path: &str) -> RequestBuilder {
        self.http.request(method, format!("http://proxy{path}")).header(header::HOST, &self.host)
    }

    fn get(&self, path: &str) -> RequestBuilder { self.request(Method::GET, path) }
    fn post(&self, path: &str, data: Value) -> RequestBuilder { self.request(Method::POST, path).json(&data) }
    fn origin(&self) -> String { format!("http://{}", self.host) }

    async fn web(&self, user: &str) -> String {
        let response = expect(self.post("/api/login", json!({"email":format!("{user}@wiring.test"),"password":"wiring-dev"})), 200).await;
        session(&response)
    }

    async fn native(&self, user: &str) -> Value {
        json_response(self.post("/api/auth/token", json!({"email":format!("{user}@wiring.test"),"password":"wiring-dev"}))
            .header("x-app-platform","ios"), 200).await
    }
}

async fn expect(request: RequestBuilder, code: u16) -> Response {
    let inspection = request.try_clone().unwrap().build().unwrap();
    let route = format!("{} {}", inspection.method(), inspection.url().path());
    let response = request.send().await.unwrap_or_else(|error| panic!("{route}: {error}"));
    if response.status() != StatusCode::from_u16(code).unwrap() {
        let actual = response.status();
        let body = response.text().await.unwrap_or_default();
        panic!("{route}: expected {code}, got {actual}: {}", body.chars().take(400).collect::<String>());
    }
    response
}

async fn json_response(request: RequestBuilder, code: u16) -> Value {
    expect(request, code).await.json().await.expect("JSON response")
}

fn session(response: &Response) -> String {
    response.headers().get_all(header::SET_COOKIE).iter().filter_map(|cookie| cookie.to_str().ok())
        .find(|value| value.starts_with("wiring_session="))
        .expect("session cookie").split(';').next().unwrap().to_owned()
}

async fn database() -> Db {
    let dsn = env::var("WIRING_HTTP_TEST_DB").unwrap();
    let url = Url::parse(&dsn).unwrap();
    assert_eq!((url.host_str(), url.path(), url.username()), (Some("postgres"), "/wiring_local", "wiring_local"));
    let (db, connection) = tokio_postgres::connect(&dsn, NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap() });
    db
}

async fn user_id(db: &Db, username: &str) -> i64 {
    db.query_one("SELECT id FROM users WHERE email=$1", &[&format!("{username}@wiring.test")]).await.unwrap().get(0)
}

#[tokio::test]
async fn frontend_catalog_and_route_contract() {
    let api = Api::new();
    assert_eq!(json_response(api.get("/health"), 200).await["ok"], true);
    for route in ["/", "/sign-in", "/sign-up", "/login", "/register", "/forgot", "/reset", "/verify",
        "/feed", "/for-you", "/likes", "/chats", "/chats/42", "/me", "/me/edit", "/consents",
        "/plus", "/premium", "/onboard", "/delete-account", "/p/42", "/r/sample", "/notifications", "/archive"] {
        let html = expect(api.get(route), 200).await.text().await.unwrap();
        assert!(html.contains("/public/dist/app.js"), "{route}");
    }
    for path in ["/m", "/privacy", "/support", "/sw.js", "/public/dist/app.js", "/public/manifest.webmanifest",
        "/.well-known/apple-app-site-association", "/.well-known/assetlinks.json"] {
        assert!(!expect(api.get(path), 200).await.bytes().await.unwrap().is_empty(), "{path}");
    }
    for path in ["/_wiring_uploads/1/private.jpg", "/_wiring_private_uploads/chat/1/hidden.jpg", "/does-not-exist"] {
        expect(api.get(path), 404).await;
    }
    let response = expect(api.get("/api/catalog?lang=en"), 200).await;
    assert_eq!(response.headers()["content-language"], "en");
    let catalog: Value = response.json().await.unwrap();
    assert!(catalog["neuro"].as_array().unwrap().iter().any(|x| x["id"] == "narcolepsy" && x["label"] == "narcolepsy"));
    expect(api.get("/api/places?q=Belgrade"), 401).await;
    let web = api.web("dev").await;
    let places = json_response(api.get("/api/places?q=Belgrade&lang=en").header(header::COOKIE, &web), 200).await;
    assert!(places["places"].as_array().unwrap().iter().any(|v| v["label"].as_str().is_some_and(|s| s.contains("Belgrade"))));
    for path in ["/api/feed", "/api/likes", "/api/matches", "/api/inbox", "/api/albums", "/api/archive"] {
        assert_eq!(json_response(api.get(path).header(header::COOKIE, &web), 200).await["ok"], true, "{path}");
    }
}

#[tokio::test]
async fn session_rotation_cookie_login_and_oauth_start() {
    let api = Api::new();
    let web = api.web("dev").await;
    assert_eq!(json_response(api.get("/api/me").header(header::COOKIE, &web), 200).await["user"]["email"], "dev@wiring.test");
    expect(api.post("/api/logout", json!({})).header(header::COOKIE, &web), 200).await;
    let tokens = api.native("peer").await;
    let access = tokens["access_token"].as_str().unwrap();
    assert_eq!(json_response(api.get("/api/me").bearer_auth(access), 200).await["user"]["email"], "peer@wiring.test");
    let refresh = tokens["refresh_token"].as_str().unwrap();
    let rotated = json_response(api.post("/api/auth/refresh", json!({"refresh_token":refresh})), 200).await;
    assert_ne!(rotated["refresh_token"], refresh);
    expect(api.post("/api/auth/refresh", json!({"refresh_token":refresh})), 401).await;
    expect(api.post("/api/auth/refresh", json!({"refresh_token":rotated["refresh_token"]})), 401).await;

    assert_eq!(json_response(api.get("/api/auth/providers"), 200).await["providers"], json!(["google"]));
    expect(api.post("/api/auth/google/start", json!({"mode":"login"})).header(header::ORIGIN, "https://foreign.invalid"), 403).await;
    let result = json_response(api.post("/api/auth/google/start", json!({"mode":"login"})).header(header::ORIGIN, api.origin()), 200).await;
    let redirect = Url::parse(result["url"].as_str().unwrap()).unwrap();
    assert_eq!(redirect.host_str(), Some("accounts.google.com"));
    let query: std::collections::HashMap<_, _> = redirect.query_pairs().collect();
    assert_eq!(query["redirect_uri"], format!("{}/api/auth/google/callback", api.origin()));
    assert_eq!(query["code_challenge_method"], "S256");
    assert!(!query["state"].is_empty());
}

#[tokio::test]
async fn register_verify_and_reset_without_external_mail() {
    let api = Api::new();
    let db = database().await;
    let email = "registration-fixture@wiring.test";
    let mut fields = json!({"name":"Rust Fixture","email":email,"password":"new-local-password","age_confirm":true});
    expect(api.post("/api/register", fields.clone()), 400).await;
    fields["privacy_confirm"] = true.into();
    assert_eq!(json_response(api.post("/api/register", fields.clone()), 200).await["needs_email_verify"], true);
    expect(api.post("/api/register", fields), 409).await;
    let token: String = db.query_one("SELECT v.token FROM email_verifications v JOIN users u ON u.id=v.user_id WHERE u.email=$1", &[&email]).await.unwrap().get(0);
    let verified = expect(api.post("/api/email/verify", json!({"token":token})), 200).await;
    assert!(!session(&verified).is_empty());
    expect(api.post("/api/email/verify", json!({"token":token})), 400).await;
    expect(api.post("/api/password/forgot", json!({"email":email})), 200).await;
    let reset: String = db.query_one("SELECT p.token FROM password_resets p JOIN users u ON u.id=p.user_id WHERE u.email=$1", &[&email]).await.unwrap().get(0);
    expect(api.post("/api/password/reset", json!({"token":reset,"password":"changed-local-password"})), 200).await;
    expect(api.post("/api/login", json!({"email":email,"password":"changed-local-password"})), 200).await;
}

fn jpeg_request(request: RequestBuilder, bytes: &[u8]) -> RequestBuilder {
    let mut body = b"--fixture-boundary\r\nContent-Disposition: form-data; name=\"file\"; filename=\"fixture.jpg\"\r\nContent-Type: image/jpeg\r\n\r\n".to_vec();
    body.extend_from_slice(bytes);
    body.extend_from_slice(b"\r\n--fixture-boundary--\r\n");
    request.header(header::CONTENT_TYPE, "multipart/form-data; boundary=fixture-boundary").body(body)
}

#[tokio::test]
async fn upload_and_read_photo_through_nginx() {
    let api = Api::new();
    let web = api.web("ira").await;
    let source = expect(api.get("/public/portraits/p01.jpg"), 200).await.bytes().await.unwrap();
    let thumbnail = expect(api.get("/public/portraits/p01.jpg?s=sm"), 200).await.bytes().await.unwrap();
    assert!(thumbnail.len() < source.len() && thumbnail.starts_with(&[0xff, 0xd8]));
    let upload = || api.request(Method::POST, "/api/photos").header(header::COOKIE, &web);
    expect(jpeg_request(upload(), b"not a jpg"), 400).await;
    let result = json_response(jpeg_request(upload(), &source), 200).await;
    let photo_id = result["photo"]["id"].as_i64().unwrap();
    let db = database().await;
    let path: String = db.query_one("SELECT path FROM photos WHERE id=$1", &[&photo_id]).await.unwrap().get(0);
    let photo = expect(api.get(&format!("/media/{path}")), 200).await.bytes().await.unwrap();
    assert!(photo.starts_with(&[0xff, 0xd8]));
    assert_eq!(json_response(api.request(Method::DELETE, &format!("/api/photos/{photo_id}")).header(header::COOKIE, &web), 200).await["ok"], true);
    expect(api.get(&format!("/media/{path}")), 404).await;
}

#[tokio::test]
async fn message_idempotency_reactions_and_soft_deletion() {
    let api = Api::new();
    let db = database().await;
    let dev = api.web("dev").await;
    let dev_id = user_id(&db, "dev").await;
    let peer = user_id(&db, "peer").await;
    let peer_cookie = api.web("peer").await;
    let sent_payload = json!({"to_id":peer,"body":"Rust message fixture","client_id":"one-message-fixture"});
    let message = json_response(api.post("/api/messages", sent_payload.clone()).header(header::COOKIE,&dev), 200).await;
    let id = message["id"].as_i64().unwrap();
    let retry = json_response(api.post("/api/messages", sent_payload).header(header::COOKIE,&dev), 200).await;
    assert_eq!(retry, json!({"ok":true,"duplicate":true,"id":id}));
    let edit = || api.request(Method::PATCH,&format!("/api/messages/{id}")).json(&json!({"body":"Edited Rust message"}));
    expect(edit().header(header::COOKIE,&peer_cookie), 404).await;
    expect(edit().header(header::COOKIE,&dev), 200).await;
    let unread = json_response(api.get("/api/inbox").header(header::COOKIE, &peer_cookie), 200).await;
    assert!(unread["unread"].as_i64().unwrap() > 0);
    expect(api.get(&format!("/api/messages/{dev_id}")).header(header::COOKIE, &peer_cookie), 200).await;
    let read = json_response(api.get("/api/inbox").header(header::COOKIE, &peer_cookie), 200).await;
    assert_eq!(read["unread"], 0);
    expect(api.request(Method::PUT,&format!("/api/messages/{id}/reaction")).header(header::COOKIE,&peer_cookie)
        .json(&json!({"emoji":"👍"})), 200).await;

    for (username, action) in [("peer", "unmatch"), ("alma", "block"), ("borya", "report")] {
        let other = user_id(&db, username).await;
        let before: i64 = db.query_one("SELECT count(*) FROM messages WHERE (from_id,to_id) IN (($1::bigint,$2::bigint),($2,$1))", &[&dev_id,&other]).await.unwrap().get(0);
        assert!(before >= 10);
        let path = format!("/api/{action}");
        expect(api.post(&path, json!({"user_id":other,"reason":"spam","details":"Synthetic report"}))
            .header(header::COOKIE,&dev), 200).await;
        let row = db.query_one("SELECT count(*), count(deleted_at) FROM messages WHERE (from_id,to_id) IN (($1::bigint,$2::bigint),($2,$1))", &[&dev_id,&other]).await.unwrap();
        assert_eq!((row.get::<_,i64>(0),row.get::<_,i64>(1)), (before,before), "{action}");
    }
}

#[tokio::test]
async fn feed_recovers_prefetched_cards_until_viewed() {
    let api = Api::new();
    let cookie = api.web("ira").await;
    let feed = |query: &str| api.get(&format!("/api/feed?hide_empty=0&{query}")).header(header::COOKIE, &cookie);
    let initial = json_response(feed("limit=30"), 200).await;
    let cards = initial["cards"].as_array().unwrap();
    assert!(cards.len() >= 2);
    let viewed = cards[0]["id"].as_i64().unwrap();
    let prefetched = cards[1]["id"].as_i64().unwrap();
    expect(api.post("/api/feed/view", json!({"target_id":viewed})).header(header::COOKIE, &cookie), 200).await;
    let recovered = json_response(feed("limit=30"), 200).await;
    let recovered = recovered["cards"].as_array().unwrap();
    assert!(!recovered.iter().any(|card| card["id"] == viewed));
    assert!(recovered.iter().any(|card| card["id"] == prefetched));
    let skipped = json_response(feed(&format!("limit=30&skip={prefetched}")), 200).await;
    assert!(!skipped["cards"].as_array().unwrap().iter().any(|card| card["id"] == prefetched));
}

#[tokio::test]
async fn support_and_admin_are_usable_on_loopback() {
    let api = Api::new();
    let db = database().await;
    let member = api.web("vera").await;
    expect(api.get("/support").bearer_auth("invalid"), 401).await;
    let contact = "Synthetic support regression";
    let support = api.post("/support", json!({"body":contact})).header(header::COOKIE,&member).header(header::ACCEPT,"application/json");
    assert_eq!(json_response(support, 200).await["ok"], true);
    let owner: i32 = db.query_one("SELECT user_id FROM support_tickets WHERE body=$1", &[&contact]).await.unwrap().get(0);
    assert_eq!(i64::from(owner), user_id(&db,"vera").await);
    expect(api.get("/admin/export/marketing-audience"), 403).await;

    let page = expect(api.get("/admin"), 200).await;
    let cookie = session(&page);
    assert!(!page.headers().get_all(header::SET_COOKIE).iter().any(|c| c.to_str().unwrap().contains("; Secure")));
    let html = page.text().await.unwrap();
    let csrf = html.split_once("name=\"csrf\" value=\"").unwrap().1.split('"').next().unwrap();
    let token = env::var("WIRING_HTTP_TEST_ADMIN").unwrap();
    let try_login = || api.request(Method::POST,"/admin").header(header::COOKIE,&cookie)
        .form(&[("token",token.as_str()),("csrf",csrf)]);
    expect(try_login().header(header::ORIGIN,"https://foreign.invalid"), 403).await;
    let admin = session(&expect(try_login().header(header::ORIGIN,api.origin()), 303).await);
    expect(api.get("/admin/export/marketing-audience").header(header::COOKIE,&admin), 200).await;
}

#[tokio::test]
async fn account_preferences_device_registration_and_restore() {
    let api = Api::new();
    let db = database().await;
    let uid = user_id(&db, "kostya").await;
    let session = api.web("kostya").await;
    let patch = |path, object: Value| api.request(Method::PATCH,path).header(header::COOKIE,&session).json(&object);
    expect(patch("/api/me/theme", json!({"theme":"not-a-theme"})), 400).await;
    expect(patch("/api/me/theme", json!({"theme":"night"})), 200).await;
    expect(patch("/api/me/language", json!({"language":"en"})), 200).await;
    let settings = db.query_one("SELECT theme,language FROM users WHERE id=$1", &[&uid]).await.unwrap();
    assert_eq!((settings.get::<_,&str>(0),settings.get::<_,&str>(1)), ("night","en"));

    let tokens = api.native("kostya").await;
    let bearer = tokens["access_token"].as_str().unwrap();
    let token = "ExpoPushToken[wiring_fixture_device]";
    expect(api.post("/api/push/device", json!({"platform":"ios","token":token})).bearer_auth(bearer), 200).await;
    let device = db.query_one("SELECT user_id::bigint FROM mobile_devices WHERE token=$1", &[&token]).await.unwrap();
    assert_eq!(device.get::<_,i64>(0), uid);
    expect(api.request(Method::DELETE,"/api/push/device").json(&json!({"token":token})).bearer_auth(bearer), 200).await;
    assert!(db.query_opt("SELECT 1 FROM mobile_devices WHERE token=$1", &[&token]).await.unwrap().is_none());

    expect(api.request(Method::DELETE,"/api/me").header(header::COOKIE,&session), 200).await;
    assert!(db.query_one("SELECT deleted_at IS NOT NULL FROM users WHERE id=$1", &[&uid]).await.unwrap().get::<_,bool>(0));
    expect(api.get("/api/me").bearer_auth(bearer), 401).await;
    assert!(!api.web("kostya").await.is_empty());
    assert!(db.query_one("SELECT deleted_at IS NULL FROM users WHERE id=$1", &[&uid]).await.unwrap().get::<_,bool>(0));
}
