use std::{env, error::Error, fs, path::Path, sync::Arc, time::{Duration, SystemTime, UNIX_EPOCH}};

use axum::{
    Json, Router,
    body::{Body, Bytes, to_bytes},
    extract::State,
    http::{HeaderMap, HeaderValue, Method, Request, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{any,post},
};
use serde_json::json;

use crate::{
    chat_write::Writer,
    mobile_http::{MAX_BODY, method_response, too_large, unavailable, upgrade},
    mobile_session::matches_access,
    public_http::{header_text, localize, request_language},
    signed_session::{SessionKeys, WEB_TTL},
    spa_http::web_cookie,
};

struct Services {
    db: Arc<Writer>,
    keys: SessionKeys,
    secure: bool,
    min_version: String,
    latest_version: String,
    ru: Bytes,
    en: Bytes,
    profile: Option<Arc<crate::profile_read::ProfileRules>>,
    login_limit: crate::login::LoginLimit,
    signup: Option<Arc<crate::registration::Registration>>,
    signup_limit: crate::login::LoginLimit,
    resend_limit: crate::login::LoginLimit,
    reset_link_limit: crate::login::LoginLimit,
    promo_limit: crate::login::LoginLimit,
    places: Option<crate::places_http::PublicPlaces>,
    history: Option<crate::chat_history_http::Openers>,
    browser_push: Option<crate::browser_push::BrowserPush>,
    place_validation: Option<crate::place_validation::Validation>,
    profile_edit: bool,
    message_send: Option<crate::message_send::Delivery>,
    recommendations: Option<crate::recommendations_http::Ranker>,
    media: Option<crate::media_http::Media>,
    message_delete: bool,
    photo_delete: bool,
    transcription: Option<crate::transcribe_http::Transcription>,
    voice: Option<crate::voice_upload::VoiceUpload>,
    photos: Option<crate::photo_upload::PhotoUpload>,
    demo_limit: crate::login::LoginLimit,
    support: Option<crate::support_http::Support>,
}

pub(crate) fn routes(writer: Option<Arc<Writer>>, catalog: Arc<crate::Catalog>, directory: &Path) -> Result<Router, Box<dyn Error>> {
    if env::var("WIRING_PUBLIC_SERVICE_INFO").as_deref() != Ok("1") {
        return Ok(Router::new());
    }
    let key = env::var("APP_SECRET_KEY")?;
    if key.trim().is_empty() {
        return Err("Public service info requires the existing signing key".into());
    }
    let service = Services {
        db: writer.ok_or("Public service info requires PostgreSQL")?,
        keys: SessionKeys::new(key.as_bytes()),
        secure: env::var("SESSION_COOKIE_SECURE").is_ok_and(|value|
            matches!(value.to_ascii_lowercase().as_str(), "true" | "1" | "yes")),
        min_version: env::var("MOBILE_MIN_VERSION").unwrap_or_default().trim().to_owned(),
        latest_version: env::var("MOBILE_LATEST_VERSION").unwrap_or_default().trim().to_owned(),
        ru: fs::read(directory.join("api-catalog-ru.json"))?.into(),
        en: fs::read(directory.join("api-catalog-en.json"))?.into(),
        profile: (env::var("WIRING_PUBLIC_PROFILE_READ").as_deref() == Ok("1"))
            .then(|| crate::profile_read::ProfileRules::load(directory, catalog.clone()).map(Arc::new)).transpose()?,
        login_limit: crate::login::LoginLimit::new(),
        signup: (env::var("WIRING_PUBLIC_REGISTRATION").as_deref() == Ok("1"))
            .then(|| crate::registration::Registration::load().map(Arc::new)).transpose()?,
        signup_limit: crate::login::LoginLimit::configured(Duration::from_secs(3600), 8),
        resend_limit: crate::login::LoginLimit::configured(Duration::from_secs(600), 8),
        reset_link_limit: crate::login::LoginLimit::configured(Duration::from_secs(600), 8),
        promo_limit: crate::login::LoginLimit::configured(Duration::from_secs(3600), 12),
        places: (env::var("WIRING_PUBLIC_PLACES").as_deref() == Ok("1"))
            .then(|| crate::places_http::PublicPlaces::new(catalog)).transpose()?,
        history: (env::var("WIRING_PUBLIC_CHAT_HISTORY").as_deref() == Ok("1"))
            .then(|| crate::chat_history_http::Openers::new(directory)).transpose()?,
        browser_push: (env::var("WIRING_PUBLIC_BROWSER_PUSH").as_deref() == Ok("1"))
            .then(crate::browser_push::BrowserPush::new),
        place_validation: (env::var("WIRING_PUBLIC_PLACE_VALIDATION").as_deref() == Ok("1"))
            .then(|| crate::place_validation::Validation::new(directory)).transpose()?,
        profile_edit: env::var("WIRING_PUBLIC_PROFILE_EDIT").as_deref() == Ok("1"),
        message_send: (env::var("WIRING_PUBLIC_MESSAGE_SEND").as_deref() == Ok("1"))
            .then(crate::message_send::Delivery::new).transpose()?,
        recommendations: (env::var("WIRING_PUBLIC_RECOMMENDATIONS").as_deref() == Ok("1"))
            .then(||crate::recommendations_http::Ranker::new(directory)).transpose()?,
        media: (env::var("WIRING_PUBLIC_MEDIA").as_deref() == Ok("1"))
            .then(crate::media_http::Media::new).transpose()?,
        message_delete: env::var("WIRING_PUBLIC_MESSAGE_DELETE").as_deref() == Ok("1"),
        photo_delete: env::var("WIRING_PUBLIC_PHOTO_DELETE").as_deref() == Ok("1"),
        transcription: (env::var("WIRING_PUBLIC_TRANSCRIBE").as_deref() == Ok("1"))
            .then(crate::transcribe_http::Transcription::new).transpose()?,
        voice: (env::var("WIRING_PUBLIC_VOICE_UPLOAD").as_deref() == Ok("1")).then(crate::voice_upload::VoiceUpload::new),
        photos: (env::var("WIRING_PUBLIC_PHOTO_UPLOAD").as_deref() == Ok("1"))
            .then(crate::photo_upload::PhotoUpload::new).transpose()?,
        demo_limit: crate::login::LoginLimit::configured(Duration::from_secs(3600), 30),
        support: (env::var("WIRING_PUBLIC_REPORT").as_deref() == Ok("1") || env::var("WIRING_PUBLIC_SUPPORT_FORM").as_deref() == Ok("1"))
            .then(||crate::support_http::Support::load(directory,env::var("WIRING_PUBLIC_SUPPORT_FORM").as_deref()==Ok("1"))).transpose()?,
    };
    let mut router = Router::new().route("/health", any(serve)).route("/api/catalog", any(serve))
        .route("/api/app-config", any(serve));
    if env::var("WIRING_PUBLIC_PUSH_DEVICE").as_deref() == Ok("1") {
        router = router.route("/api/push/device", any(serve));
    }
    if env::var("WIRING_PUBLIC_WEB_LOGOUT").as_deref() == Ok("1") {
        router = router.route("/api/logout", any(serve));
    }
    if service.profile.is_some() {
        router = router.route("/api/me", any(serve));
    }
    if env::var("WIRING_PUBLIC_PASSWORD_LOGIN").as_deref() == Ok("1") {
        if service.profile.is_none() { return Err("Password login requires profile reads".into()); }
        router = router.route("/api/login", any(serve)).route("/api/auth/token", any(serve));
    }
    if env::var("WIRING_PUBLIC_ACCOUNT_TOKENS").as_deref() == Ok("1") {
        if service.profile.is_none() { return Err("Account tokens require profile reads".into()); }
        router = router.route("/api/email/verify", any(serve)).route("/api/password/reset", any(serve));
    }
    if env::var("WIRING_PUBLIC_NATIVE_OAUTH_EXCHANGE").as_deref() == Ok("1") {
        if service.profile.is_none() { return Err("Native OAuth exchange requires profile reads".into()); }
        router = router.route("/api/auth/native/exchange", any(serve));
    }
    if service.signup.is_some() {
        if service.profile.is_none() { return Err("Registration requires profile reads".into()); }
        router = router.route("/api/register", any(serve)).route("/api/email/resend", any(serve))
            .route("/api/password/forgot", any(serve));
    }
    if env::var("WIRING_PUBLIC_REGISTRATION_ATTRIBUTION").as_deref() == Ok("1") {
        if service.profile.is_none() { return Err("Attribution requires account services".into()); }
        router = router.route("/api/analytics/registration", any(serve));
    }
    if env::var("WIRING_PUBLIC_ACCOUNT_SETTINGS").as_deref() == Ok("1") {
        if service.profile.is_none() { return Err("Account settings require profile reads".into()); }
        router = router.route("/api/me/language", any(serve)).route("/api/me/theme", any(serve))
            .route("/api/me/consents", any(serve)).route("/api/me/jev-feed", any(serve));
    }
    if env::var("WIRING_PUBLIC_ACCOUNT_DELETE").as_deref() == Ok("1") {
        if service.profile.is_none() { return Err("Account deletion requires account services".into()); }
        router = router.route("/api/me/delete", any(serve));
    }
    if env::var("WIRING_PUBLIC_PAIR_CONTROLS").as_deref() == Ok("1") {
        router = router.route("/api/unmatch", any(serve)).route("/api/block", any(serve));
    }
    if env::var("WIRING_PUBLIC_SUPPORT_REPLIES").as_deref() == Ok("1") {
        router = router.route("/api/support/replies", any(serve));
    }
    if env::var("WIRING_PUBLIC_MESSAGE_EDITS").as_deref() == Ok("1") {
        router = router.route("/api/messages/{id}", any(serve)).route("/api/messages/{id}/reaction", any(serve));
    }
    if service.message_delete && (service.media.is_none() || env::var("WIRING_PUBLIC_MESSAGE_EDITS").as_deref() != Ok("1")) {
        return Err("Message deletion requires media and message edits".into());
    }
    if service.history.is_some() {
        if service.profile.is_none() { return Err("Chat history requires profile reads".into()); }
        if env::var("WIRING_PUBLIC_MESSAGE_EDITS").as_deref() != Ok("1") {
            router = router.route("/api/messages/{id}", any(serve));
        }
    }
    if env::var("WIRING_PUBLIC_FEED_DELIVERY").as_deref() == Ok("1") {
        router = router.route("/api/feed/view", any(serve)).route("/api/feed/reset", any(serve))
            .route("/api/deck/restart", any(serve));
    }
    if env::var("WIRING_PUBLIC_PEOPLE_READ").as_deref() == Ok("1") {
        if service.profile.is_none() { return Err("People and matches require profile reads".into()); }
        router = router.route("/api/people/{id}", any(serve)).route("/api/matches", any(serve));
    }
    if env::var("WIRING_PUBLIC_ALBUMS").as_deref() == Ok("1") {
        router = router.route("/api/albums", any(serve)).route("/api/albums/{id}", any(serve));
    }
    if env::var("WIRING_PUBLIC_ONBOARD_SKIP").as_deref() == Ok("1") {
        if service.profile.is_none() { return Err("Onboarding requires profile reads".into()); }
        router = router.route("/api/onboard/skip", any(serve));
    }
    if service.places.is_some() {
        router = router.route("/api/places", any(serve));
    }
    if service.place_validation.is_some() {
        if service.places.is_none() || service.profile.is_none() { return Err("Place validation requires places and profile reads".into()); }
        router = router.route("/api/places/resolve", any(serve)).route("/api/me/city", any(serve));
    }
    if service.profile_edit && service.place_validation.is_none() { return Err("Profile edit requires place validation".into()); }
    if env::var("WIRING_PUBLIC_DISCOVERY").as_deref() == Ok("1") {
        if service.places.is_none() || service.profile.is_none() { return Err("Discovery requires places and profile reads".into()); }
        router = router.route("/api/feed", any(serve)).route("/api/likes", any(serve));
    }
    if env::var("WIRING_PUBLIC_PREMIUM").as_deref() == Ok("1") {
        if service.profile.is_none() { return Err("Premium requires profile reads".into()); }
        router = router.route("/api/plus", any(serve)).route("/api/premium/redeem", any(serve));
    }
    if env::var("WIRING_PUBLIC_NOTIFICATION_SETTINGS").as_deref() == Ok("1") {
        if service.profile.is_none() { return Err("Notification settings require profile reads".into()); }
        router = router.route("/api/notifications", any(serve));
    }
    if env::var("WIRING_PUBLIC_ARCHIVE").as_deref() == Ok("1") {
        router = router.route("/api/archive", any(serve));
    }
    if env::var("WIRING_PUBLIC_REWIND").as_deref() == Ok("1") {
        if service.profile.is_none() { return Err("Rewind requires profile reads".into()); }
        router = router.route("/api/rewind", any(serve));
    }
    if service.browser_push.is_some() {
        router = router.route("/api/push/public-key", any(serve)).route("/api/push/subscribe", any(serve))
            .route("/api/push/unsubscribe", any(serve));
    }
    if service.message_send.is_some() { router = router.route("/api/messages", any(serve)); }
    if service.recommendations.is_some() {
        if service.profile.is_none() || service.places.is_none() { return Err("Recommendations require profile and places".into()); }
        router = router.route("/api/recommendations", any(serve));
    }
    if service.media.is_some() {
        router=router.route("/api/messages/media/{id}",any(serve)).route("/media/{*path}",any(serve))
            .route("/public/{*path}",any(serve));
    }
    if service.transcription.is_some() {
        if service.media.is_none() {return Err("Transcription requires media".into());}
        router=router.route("/api/messages/{id}/transcribe",any(serve));
    }
    if service.voice.is_some() {
        if service.media.is_none() || service.message_send.is_none() {return Err("Voice uploading requires media and delivery".into());}
        router=router.route("/api/messages/voice",any(serve).layer(axum::extract::DefaultBodyLimit::max(MAX_BODY)));
    }
    if service.photos.is_some() {
        if service.media.is_none() || service.message_send.is_none() || service.profile.is_none() {return Err("Photo uploading requires media, delivery and profiles".into());}
        router=router.route("/api/messages/photo",any(serve).layer(axum::extract::DefaultBodyLimit::max(MAX_BODY)))
            .route("/api/photos",any(serve).layer(axum::extract::DefaultBodyLimit::max(MAX_BODY)));
    }
    if env::var("WIRING_PUBLIC_DEMO").as_deref() == Ok("1") {
        if service.profile.is_none() { return Err("Demo requires profile reads".into()); }
        router = router.route("/api/demo", any(serve));
    }
    if env::var("WIRING_PUBLIC_REPORT").as_deref() == Ok("1") {
        router = router.route("/api/report", any(serve));
    }
    if env::var("WIRING_PUBLIC_SUPPORT_FORM").as_deref() == Ok("1") {
        router = router.route("/support", post(serve));
    }
    if env::var("WIRING_PUBLIC_SWIPE").as_deref() == Ok("1") {
        if service.message_send.is_none() || service.profile.is_none() { return Err("Swipe requires delivery and profile reads".into()); }
        router = router.route("/api/swipe", any(serve));
    }
    if env::var("WIRING_PUBLIC_PHOTO_METADATA").as_deref() == Ok("1") {
        router = router.route("/api/photos/order", any(serve)).route("/api/photos/{id}", any(serve));
    }
    if service.photo_delete && (service.media.is_none() || env::var("WIRING_PUBLIC_PHOTO_METADATA").as_deref() != Ok("1")) {
        return Err("Photo deletion requires media and photo metadata".into());
    }
    Ok(router.with_state(Arc::new(service)))
}

async fn serve(State(state): State<Arc<Services>>, request: Request<Body>) -> Response {
    let (parts, body) = request.into_parts();
    let method = &parts.method;
    let headers = &parts.headers;
    let (lang, persist) = request_language(parts.uri.query(), headers);
    let now = SystemTime::now();
    let timestamp = now.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    let path = parts.uri.path();
    let web = web_cookie(headers).and_then(|cookie| state.keys.read_web(cookie, timestamp));
    let mut mobile = false;
    let mut clear = false;
    let mut login_uid = None;
    let mut language_preference = None;
    let result = async {
        let mut uid = web.as_ref().and_then(|claims| claims.get("uid")).and_then(serde_json::Value::as_i64);
        if path.starts_with("/api/") || path=="/support" {
            if let Some(response) = upgrade(header_text(headers, "x-app-version").unwrap_or(""), &state.min_version, lang) {
                return Ok(response);
            }
            if let Some(token) = header_text(headers, "authorization").filter(|_| !path.starts_with("/api/auth/")).and_then(|value| {
                let (scheme, token) = value.split_once(' ')?;
                scheme.eq_ignore_ascii_case("bearer").then_some(token.trim())
            }) {
                let mut valid = false;
                if let Some(claims) = state.keys.read_access(token, timestamp) {
                    let user = claims.uid;
                    valid = state.db.transaction(move |tx| Box::pin(async move {
                        let row = tx.query_opt("SELECT password_hash, COALESCE(deleted_at, 0) != 0 FROM users WHERE id = $1::bigint", &[&user]).await?;
                        Ok(row.is_some_and(|row| matches_access(&claims, row.get(0), row.get(1))))
                    })).await?.0;
                    uid = Some(user);
                }
                if !valid {
                    let error = if lang == "en" { "Invalid token" } else { "токен недействителен" };
                    return Ok((StatusCode::UNAUTHORIZED, Json(json!({"ok": false, "token_expired": true, "error": error}))).into_response());
                }
                mobile = true;
            }
            if method != Method::OPTIONS {
                if let Some(ua) = header_text(headers, "user-agent").map(str::trim).filter(|value| !value.is_empty()) {
                    let ip = header_text(headers, "x-forwarded-for").unwrap_or("x").split(',').next().unwrap_or("x").trim();
                    let visit = crate::visit::Visit::new(ip, ua);
                    let exists = state.db.transaction(move |tx| Box::pin(crate::visit::track(tx, uid, timestamp as i64, visit))).await?.0;
                    clear = !exists;
                }
            }
        }
        if path=="/support" && *method==Method::POST {
            use axum::extract::{FromRequest,Multipart};
            let mime=header_text(headers,"content-type").unwrap_or("").split(';').next().unwrap_or("").trim().to_ascii_lowercase();
            let is_json=mime=="application/json" || (mime.starts_with("application/") && mime.ends_with("+json"));
            let ip=header_text(headers,"x-forwarded-for").unwrap_or("x").split(',').next().unwrap_or("x").trim().to_owned();
            let wants_json=is_json || header_text(headers,"x-requested-with")==Some("XMLHttpRequest") || header_text(headers,"accept").is_some_and(|value|value.contains("application/json"));
            let fields=if mime=="multipart/form-data" {
                let bytes=match to_bytes(body,64*1024).await {Ok(bytes)=>bytes,Err(_)=>return Ok(too_large())};
                let mut request=Request::new(Body::from(bytes));
                *request.headers_mut()=headers.clone();
                *request.method_mut()=method.clone();
                let mut values=serde_json::Map::new();
                if let Ok(mut form)=Multipart::from_request(request,&()).await {
                    loop {
                        let field=match form.next_field().await {Ok(Some(field))=>field,Ok(None)=>break,Err(_)=>{values.clear();break;}};
                        let Some(name)=field.name().filter(|key| matches!(*key,"name"|"email"|"body")).map(str::to_owned) else {continue;};
                        if field.file_name().is_none() {
                            match field.text().await {Ok(value)=>{values.entry(name).or_insert(value.into());},Err(_)=>{values.clear();break;}}
                        }
                    }
                }
                serde_json::Value::Object(values)
            } else {
                let bytes=match to_bytes(body,64*1024).await {Ok(bytes)=>bytes,Err(_)=>return Ok(too_large())};
                if is_json {serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)}
                else {
                    let mut values=serde_json::Map::new();
                    for (key,value) in form_urlencoded::parse(&bytes) { values.entry(key.into_owned()).or_insert(value.into_owned().into()); }
                    serde_json::Value::Object(values)
                }
            };
            return state.support.as_ref().expect("support configured").ticket(&state.db,uid.filter(|_|!clear),fields,&ip,timestamp as i64,lang,wants_json).await;
        }
        if let Some(media)=state.media.as_ref() {
            if let Some(id)=path.strip_prefix("/api/messages/media/") {
                if !matches!(*method,Method::GET|Method::HEAD) { return Ok(method_response(method,"GET, HEAD, OPTIONS")); }
                let Some(uid)=uid.filter(|_|!clear) else {return Ok(crate::account_settings::signed_out(lang));};
                return media.chat(&state.db,uid,id,parts.uri.query().unwrap_or(""),timestamp as i64,lang).await;
            }
            if let Some(path)=path.strip_prefix("/media/") {
                if !matches!(*method,Method::GET|Method::HEAD) { return Ok(method_response(method,"GET, HEAD, OPTIONS")); }
                return Ok(media.profile(path,parts.uri.query().unwrap_or("")).await);
            }
            if let Some(path)=path.strip_prefix("/public/") {
                if !matches!(*method,Method::GET|Method::HEAD) { return Ok(method_response(method,"GET, HEAD, OPTIONS")); }
                return Ok(media.static_thumbnail(path).await);
            }
        }
        if path == "/api/demo" {
            if *method != Method::POST { return Ok(method_response(method,"POST, OPTIONS")); }
            let ip = header_text(headers,"x-forwarded-for").unwrap_or("x").split(',').next().unwrap_or("x").trim();
            if !state.demo_limit.allow(ip,std::time::Instant::now()) {
                let message = if lang == "en" { "Too many requests, try again later" } else { "слишком много запросов, попробуй позже" };
                return Ok((StatusCode::TOO_MANY_REQUESTS,Json(json!({"ok":false,"error":message}))).into_response());
            }
            let (response,uid) = crate::demo_http::create(&state.db,Arc::clone(state.profile.as_ref().expect("profile configured")),timestamp as i64,lang).await?;
            login_uid = Some(uid);
            return Ok(response);
        }
        if path == "/api/report" {
            if *method != Method::POST { return Ok(method_response(method,"POST, OPTIONS")); }
            let Some(uid)=uid.filter(|_|!clear) else {return Ok(crate::account_settings::signed_out(lang));};
            let mime=header_text(headers,"content-type").unwrap_or("").split(';').next().unwrap_or("").trim().to_ascii_lowercase();
            let fields=if mime=="application/json" || (mime.starts_with("application/") && mime.ends_with("+json")) {
                match to_bytes(body,MAX_BODY).await {
                    Ok(data)=>serde_json::from_slice(&data).unwrap_or(serde_json::Value::Null),
                    Err(_)=>return Ok(too_large()),
                }
            } else { serde_json::Value::Null };
            return state.support.as_ref().expect("report configured").report(&state.db,uid,fields,timestamp as i64,lang).await;
        }
        if matches!(path,"/api/messages/voice"|"/api/messages/photo"|"/api/photos") {
            use axum::extract::{FromRequest,Multipart};
            if *method!=Method::POST {return Ok(method_response(method,"POST, OPTIONS"));}
            let Some(uid)=uid.filter(|_|!clear) else {return Ok(crate::account_settings::signed_out(lang));};
            let ip=header_text(headers,"x-forwarded-for").unwrap_or("x").split(',').next().unwrap_or("x").trim();
            let bytes=match to_bytes(body,MAX_BODY).await {Ok(bytes)=>bytes,Err(_)=>return Ok(too_large())};
            let mut request=Request::new(Body::from(bytes));
            *request.headers_mut()=headers.clone();
            *request.method_mut()=method.clone();
            *request.extensions_mut()=parts.extensions.clone();
            let form=Multipart::from_request(request,&()).await.ok();
            if path!="/api/messages/voice" {
                return state.photos.as_ref().expect("photo upload configured").serve(&state.db,state.media.as_ref().expect("media configured"),
                    Arc::clone(state.profile.as_ref().expect("profile configured")),state.message_send.as_ref().expect("delivery configured"),
                    uid,path=="/api/messages/photo",ip,timestamp as i64,lang,form).await;
            }
            return state.voice.as_ref().expect("voice configured").send(&state.db,state.media.as_ref().expect("media configured"),
                state.message_send.as_ref().expect("delivery configured"),uid,ip,timestamp as i64,lang,form).await;
        }
        if matches!(path,"/api/messages"|"/api/swipe") {
            if *method != Method::POST { return Ok(method_response(method,"POST, OPTIONS")); }
            let Some(uid) = uid.filter(|_| !clear) else { return Ok(crate::account_settings::signed_out(lang)); };
            let mime = header_text(headers, "content-type").unwrap_or("").split(';').next().unwrap_or("").trim().to_ascii_lowercase();
            let fields = if mime == "application/json" || (mime.starts_with("application/") && mime.ends_with("+json")) {
                match to_bytes(body, MAX_BODY).await {
                    Ok(data) => serde_json::from_slice(&data).unwrap_or(serde_json::Value::Null),
                    Err(_) => return Ok(too_large()),
                }
            } else { serde_json::Value::Null };
            let delivery=state.message_send.as_ref().expect("message send configured");
            if path == "/api/swipe" {
                return crate::swipe_http::serve(&state.db,delivery,Arc::clone(state.profile.as_ref().expect("profile configured")),
                    uid,fields,timestamp as i64,lang).await;
            }
            return delivery.send(&state.db,uid,fields,timestamp as i64,lang).await;
        }
        if path == "/api/me" && *method == Method::PATCH && state.profile_edit {
            let Some(uid) = uid.filter(|_| !clear) else { return Ok(crate::account_settings::signed_out(lang)); };
            let mime = header_text(headers, "content-type").unwrap_or("").split(';').next().unwrap_or("").trim().to_ascii_lowercase();
            let fields = if mime == "application/json" || (mime.starts_with("application/") && mime.ends_with("+json")) {
                match to_bytes(body, MAX_BODY).await {
                    Ok(data) => serde_json::from_slice(&data).unwrap_or(serde_json::Value::Null),
                    Err(_) => return Ok(too_large()),
                }
            } else { serde_json::Value::Null };
            let profile = Arc::clone(state.profile.as_ref().expect("profile configured"));
            return crate::profile_edit::edit(&state.db,state.place_validation.as_ref().expect("place validation configured"),
                state.places.as_ref().expect("places configured"),profile,uid,fields,timestamp as i64,lang).await;
        }
        if let Some(photo) = path.strip_prefix("/api/photos/") {
            if photo!="order" && *method==Method::DELETE && state.photo_delete {
                if photo.is_empty() || !photo.bytes().all(|byte|byte.is_ascii_digit()) {return Ok(StatusCode::NOT_FOUND.into_response());}
                let Some(uid)=uid.filter(|_|!clear) else {return Ok(crate::account_settings::signed_out(lang));};
                return crate::photo_metadata::delete(&state.db,state.media.as_ref().expect("media configured"),uid,photo.parse().ok(),timestamp as i64,lang).await;
            }
            if *method != Method::PATCH { return Ok(method_response(method, "PATCH, OPTIONS")); }
            if photo != "order" && (photo.is_empty() || !photo.bytes().all(|byte|byte.is_ascii_digit())) {
                return Ok(StatusCode::NOT_FOUND.into_response());
            }
            let Some(uid) = uid.filter(|_| !clear) else { return Ok(crate::account_settings::signed_out(lang)); };
            let mime = header_text(headers, "content-type").unwrap_or("").split(';').next().unwrap_or("").trim().to_ascii_lowercase();
            let fields = if mime == "application/json" || (mime.starts_with("application/") && mime.ends_with("+json")) {
                match to_bytes(body, MAX_BODY).await {
                    Ok(data) => serde_json::from_slice(&data).unwrap_or(serde_json::Value::Null),
                    Err(_) => return Ok(too_large()),
                }
            } else { serde_json::Value::Null };
            return crate::photo_metadata::patch(&state.db, uid, photo, fields, timestamp as i64, lang).await;
        }
        if matches!(path, "/api/places/resolve" | "/api/me/city") {
            if *method != Method::POST { return Ok(method_response(method, "POST, OPTIONS")); }
            let Some(uid) = uid.filter(|_| !clear) else { return Ok(crate::account_settings::signed_out(lang)); };
            let mime = header_text(headers, "content-type").unwrap_or("").split(';').next().unwrap_or("").trim().to_ascii_lowercase();
            let fields = if mime == "application/json" || (mime.starts_with("application/") && mime.ends_with("+json")) {
                match to_bytes(body, MAX_BODY).await {
                    Ok(data) => serde_json::from_slice(&data).unwrap_or(serde_json::Value::Null),
                    Err(_) => return Ok(too_large()),
                }
            } else { serde_json::Value::Null };
            let places = state.places.as_ref().expect("places configured");
            let profile = Arc::clone(state.profile.as_ref().expect("profile configured"));
            return state.place_validation.as_ref().expect("validation configured")
                .serve(&state.db, places, profile, uid, path == "/api/me/city", fields, timestamp as i64, lang).await;
        }
        if matches!(path, "/api/push/public-key" | "/api/push/subscribe" | "/api/push/unsubscribe") {
            let key = path == "/api/push/public-key";
            if !(if key { matches!(*method, Method::GET | Method::HEAD) } else { *method == Method::POST }) {
                return Ok(method_response(method, if key { "GET, HEAD, OPTIONS" } else { "POST, OPTIONS" }));
            }
            let Some(uid) = uid.filter(|_| !clear) else { return Ok(crate::account_settings::signed_out(lang)); };
            let mime = header_text(headers, "content-type").unwrap_or("").split(';').next().unwrap_or("").trim().to_ascii_lowercase();
            let fields = if !key && (mime == "application/json" || (mime.starts_with("application/") && mime.ends_with("+json"))) {
                match to_bytes(body, MAX_BODY).await {
                    Ok(data) => serde_json::from_slice(&data).unwrap_or(serde_json::Value::Null),
                    Err(_) => return Ok(too_large()),
                }
            } else { serde_json::Value::Null };
            return state.browser_push.as_ref().expect("browser push configured")
                .serve(&state.db, uid, path, fields, timestamp as i64, lang).await;
        }
        if path == "/api/rewind" {
            if *method != Method::POST { return Ok(method_response(method, "POST, OPTIONS")); }
            let Some(uid) = uid.filter(|_| !clear) else { return Ok(crate::account_settings::signed_out(lang)); };
            let profile = Arc::clone(state.profile.as_ref().expect("profile configured"));
            return crate::archive_http::serve(&state.db, uid, crate::archive_http::Action::Rewind(profile), timestamp as i64, lang).await;
        }
        if path == "/api/archive" {
            if !matches!(*method, Method::GET | Method::HEAD | Method::POST) {
                return Ok(method_response(method, "GET, HEAD, POST, OPTIONS"));
            }
            let Some(uid) = uid.filter(|_| !clear) else { return Ok(crate::account_settings::signed_out(lang)); };
            let mime = header_text(headers, "content-type").unwrap_or("").split(';').next().unwrap_or("").trim().to_ascii_lowercase();
            let fields = if *method == Method::POST && (mime == "application/json" || (mime.starts_with("application/") && mime.ends_with("+json"))) {
                match to_bytes(body, MAX_BODY).await {
                    Ok(data) => serde_json::from_slice(&data).unwrap_or(serde_json::Value::Null),
                    Err(_) => return Ok(too_large()),
                }
            } else { serde_json::Value::Null };
            let action = if *method == Method::POST { crate::archive_http::Action::Revise(fields) } else { crate::archive_http::Action::Read };
            return crate::archive_http::serve(&state.db, uid, action, timestamp as i64, lang).await;
        }
        if path == "/api/notifications" {
            if *method != Method::PATCH { return Ok(method_response(method, "PATCH, OPTIONS")); }
            let Some(uid) = uid.filter(|_| !clear) else { return Ok(crate::account_settings::signed_out(lang)); };
            let mime = header_text(headers, "content-type").unwrap_or("").split(';').next().unwrap_or("").trim().to_ascii_lowercase();
            let fields = if mime == "application/json" || (mime.starts_with("application/") && mime.ends_with("+json")) {
                match to_bytes(body, MAX_BODY).await {
                    Ok(data) => serde_json::from_slice(&data).unwrap_or(serde_json::Value::Null),
                    Err(_) => return Ok(too_large()),
                }
            } else { serde_json::Value::Null };
            let profile = Arc::clone(state.profile.as_ref().expect("profile configured"));
            return crate::notification_settings::patch(&state.db, profile, uid, fields, timestamp as i64, lang).await;
        }
        if matches!(path, "/api/plus" | "/api/premium/redeem") {
            let redeem = path == "/api/premium/redeem";
            if *method != if redeem { Method::POST } else { Method::PATCH } {
                return Ok(method_response(method, if redeem { "POST, OPTIONS" } else { "PATCH, OPTIONS" }));
            }
            let Some(uid) = uid.filter(|_| !clear) else { return Ok(crate::account_settings::signed_out(lang)); };
            let mime = header_text(headers, "content-type").unwrap_or("").split(';').next().unwrap_or("").trim().to_ascii_lowercase();
            let fields = if mime == "application/json" || (mime.starts_with("application/") && mime.ends_with("+json")) {
                match to_bytes(body, MAX_BODY).await {
                    Ok(data) => serde_json::from_slice(&data).unwrap_or(serde_json::Value::Null),
                    Err(_) => return Ok(too_large()),
                }
            } else { serde_json::Value::Null };
            let ip = header_text(headers, "x-forwarded-for").unwrap_or("x").split(',').next().unwrap_or("x").trim();
            let profile = Arc::clone(state.profile.as_ref().expect("profile configured"));
            return crate::premium_http::serve(&state.db, profile, &state.promo_limit, uid, redeem, fields, timestamp as i64, ip, lang).await;
        }
        if path == "/api/albums" || path.starts_with("/api/albums/") {
            let item = path.strip_prefix("/api/albums/");
            if item.is_some_and(|id| id.is_empty() || !id.bytes().all(|byte| byte.is_ascii_digit())) {
                return Ok(StatusCode::NOT_FOUND.into_response());
            }
            let allow = if item.is_some() { "PATCH, DELETE, OPTIONS" } else { "GET, HEAD, POST, OPTIONS" };
            if !(if item.is_some() { matches!(*method, Method::PATCH | Method::DELETE) }
                else { matches!(*method, Method::GET | Method::HEAD | Method::POST) }) {
                return Ok(method_response(method, allow));
            }
            let Some(uid) = uid.filter(|_| !clear) else { return Ok(crate::account_settings::signed_out(lang)); };
            let mime = header_text(headers, "content-type").unwrap_or("").split(';').next().unwrap_or("").trim().to_ascii_lowercase();
            let fields = if *method == Method::PATCH && (mime == "application/json" || (mime.starts_with("application/") && mime.ends_with("+json"))) {
                match to_bytes(body, MAX_BODY).await {
                    Ok(data) => serde_json::from_slice(&data).unwrap_or(serde_json::Value::Null),
                    Err(_) => return Ok(too_large()),
                }
            } else { serde_json::Value::Null };
            return crate::albums_http::serve(&state.db, uid, method, item.and_then(|id| id.parse().ok()), fields, timestamp as i64, lang).await;
        }
        if path == "/api/onboard/skip" {
            if *method != Method::POST { return Ok(method_response(method, "POST, OPTIONS")); }
            let Some(uid) = uid.filter(|_| !clear) else { return Ok(crate::account_settings::signed_out(lang)); };
            let profile = Arc::clone(state.profile.as_ref().expect("profile configured"));
            return crate::albums_http::skip(&state.db, profile, uid, timestamp as i64, lang).await;
        }
        if let Some(tail) = path.strip_prefix("/api/messages/") {
            if let Some(id)=tail.strip_suffix("/transcribe") && let Some(transcribe)=state.transcription.as_ref() {
                if *method!=Method::POST {return Ok(method_response(method,"POST, OPTIONS"));}
                if id.is_empty() || !id.bytes().all(|byte|byte.is_ascii_digit()) {return Ok(StatusCode::NOT_FOUND.into_response());}
                let Some(uid)=uid.filter(|_|!clear) else {return Ok(crate::account_settings::signed_out(lang));};
                let ip=header_text(headers,"x-forwarded-for").unwrap_or("x").split(',').next().unwrap_or("x").trim();
                return transcribe.serve(&state.db,state.media.as_ref().expect("media configured"),uid,id.parse().ok(),ip,timestamp as i64,lang).await;
            }
            let (id, reaction) = tail.strip_suffix("/reaction").map_or((tail, false), |id| (id, true));
            if id.is_empty() || !id.bytes().all(|byte| byte.is_ascii_digit()) {
                return Ok(StatusCode::NOT_FOUND.into_response());
            }
            if !reaction && matches!(*method, Method::GET | Method::HEAD) && let Some(history) = &state.history {
                let Some(uid) = uid.filter(|_| !clear) else { return Ok(crate::account_settings::signed_out(lang)); };
                let profile = Arc::clone(state.profile.as_ref().expect("profile configured"));
                return history.read(&state.db, profile, uid, id.parse().ok(), parts.uri.query().unwrap_or(""), timestamp as i64, lang).await;
            }
            if !reaction && *method == Method::DELETE && state.message_delete {
                let Some(uid) = uid.filter(|_| !clear) else { return Ok(crate::account_settings::signed_out(lang)); };
                return crate::message_edits::delete(&state.db,state.media.as_ref().expect("media configured"),uid,id.parse().ok(),timestamp as i64,lang).await;
            }
            if *method != if reaction { Method::PUT } else { Method::PATCH } {
                return Ok(method_response(method, if reaction { "PUT, OPTIONS" } else { "PATCH, OPTIONS" }));
            }
            let Some(uid) = uid.filter(|_| !clear) else { return Ok(crate::account_settings::signed_out(lang)); };
            let mime = header_text(headers, "content-type").unwrap_or("").split(';').next().unwrap_or("").trim().to_ascii_lowercase();
            let fields = if mime == "application/json" || (mime.starts_with("application/") && mime.ends_with("+json")) {
                match to_bytes(body, MAX_BODY).await {
                    Ok(data) => serde_json::from_slice(&data).unwrap_or(serde_json::Value::Null),
                    Err(_) => return Ok(too_large()),
                }
            } else { serde_json::Value::Null };
            return crate::message_edits::mutate(&state.db, uid, id.parse::<i64>().ok(), reaction, fields, timestamp as i64, lang).await;
        }
        if matches!(path, "/api/feed/view" | "/api/feed/reset" | "/api/deck/restart") {
            if *method != Method::POST { return Ok(method_response(method, "POST, OPTIONS")); }
            let Some(uid) = uid.filter(|_| !clear) else { return Ok(crate::account_settings::signed_out(lang)); };
            let mime = header_text(headers, "content-type").unwrap_or("").split(';').next().unwrap_or("").trim().to_ascii_lowercase();
            let fields = if path != "/api/deck/restart" && (mime == "application/json" || (mime.starts_with("application/") && mime.ends_with("+json"))) {
                match to_bytes(body, MAX_BODY).await {
                    Ok(data) => serde_json::from_slice(&data).unwrap_or(serde_json::Value::Null),
                    Err(_) => return Ok(too_large()),
                }
            } else { serde_json::Value::Null };
            return crate::feed_delivery_http::serve(&state.db, uid, path, fields, timestamp as i64, lang).await;
        }
        if matches!(path, "/api/unmatch" | "/api/block") {
            if *method != Method::POST { return Ok(method_response(method, "POST, OPTIONS")); }
            let Some(uid) = uid.filter(|_| !clear) else { return Ok(crate::account_settings::signed_out(lang)); };
            let mime = header_text(headers,"content-type").unwrap_or("").split(';').next().unwrap_or("").trim().to_ascii_lowercase();
            let fields = if mime == "application/json" || (mime.starts_with("application/") && mime.ends_with("+json")) {
                match to_bytes(body, MAX_BODY).await {
                    Ok(data) => serde_json::from_slice(&data).unwrap_or(serde_json::Value::Null),
                    Err(_) => return Ok(too_large()),
                }
            } else { serde_json::Value::Null };
            return crate::account_settings::pair(&state.db,uid,path=="/api/block",fields,timestamp as i64,lang).await;
        }
        if path == "/api/me/delete" || (path == "/api/me" && *method == Method::DELETE
            && env::var("WIRING_PUBLIC_ACCOUNT_DELETE").as_deref() == Ok("1")) {
            if !matches!(*method, Method::POST | Method::DELETE) ||
                (path == "/api/me/delete" && *method != Method::POST)
            {
                return Ok(method_response(method, "POST, OPTIONS"));
            }
            let Some(uid) = uid.filter(|_| !clear) else { return Ok(crate::account_settings::signed_out(lang)); };
            let stamp = timestamp as i64;
            let success = state.db.transaction(move |tx|Box::pin(async move {
                let count = tx.execute("UPDATE users SET deleted_at=$2::bigint,last_seen=$2::bigint WHERE id=$1::bigint",
                    &[&uid,&stamp]).await?;
                Ok(count == 1)
            })).await?.0;
            clear = true;
            return Ok(if success { Json(json!({"ok":true})).into_response() }
                else { (StatusCode::UNAUTHORIZED,Json(json!({"ok":false,
                    "error":if lang=="en" {"No profile"} else {"нет профиля"}}))).into_response() });
        }
        if matches!(path, "/api/me/language" | "/api/me/theme" | "/api/me/consents" | "/api/me/jev-feed") {
            let expected = if path == "/api/me/jev-feed" { Method::POST } else { Method::PATCH };
            if *method != expected {
                return Ok(method_response(method, if expected == Method::POST {"POST, OPTIONS"} else {"PATCH, OPTIONS"}));
            }
            let Some(uid) = uid.filter(|_| !clear) else { return Ok(crate::account_settings::signed_out(lang)); };
            let mime = header_text(headers,"content-type").unwrap_or("").split(';').next().unwrap_or("").trim().to_ascii_lowercase();
            let fields = if mime == "application/json" || (mime.starts_with("application/") && mime.ends_with("+json")) {
                match to_bytes(body, MAX_BODY).await {
                    Ok(data) => serde_json::from_slice(&data).unwrap_or(serde_json::Value::Null),
                    Err(_) => return Ok(too_large()),
                }
            } else { serde_json::Value::Null };
            let profile = Arc::clone(state.profile.as_ref().expect("profile configured"));
            let (response, selected) = crate::account_settings::update(&state.db,profile,uid,path,fields,timestamp as i64,lang).await?;
            language_preference = selected.filter(|_|!header_text(headers,"authorization").unwrap_or("").starts_with("Bearer "));
            return Ok(response);
        }
        if matches!(path, "/api/login" | "/api/auth/token" | "/api/email/verify" | "/api/password/reset" |
            "/api/auth/native/exchange" | "/api/register" | "/api/email/resend" | "/api/password/forgot" | "/api/analytics/registration")
        {
            if *method != Method::POST { return Ok(method_response(method, "POST, OPTIONS")); }
            let ip = header_text(headers, "x-forwarded-for").unwrap_or("x").split(',').next().unwrap_or("x").trim();
            if matches!(path, "/api/login" | "/api/auth/token") && !state.login_limit.allow(ip, std::time::Instant::now()) {
                return Ok(crate::login::limited(lang));
            }
            let limiter = match path {
                "/api/register" => Some(&state.signup_limit),
                "/api/email/resend" => Some(&state.resend_limit),
                "/api/password/forgot" => Some(&state.reset_link_limit),
                _ => None,
            };
            if limiter.is_some_and(|limit| !limit.allow(ip, std::time::Instant::now())) {
                return Ok(if path == "/api/register" {
                    let error = if lang == "en" { "Too many registrations. Please wait." } else { "слишком много регистраций, подожди" };
                    (StatusCode::TOO_MANY_REQUESTS, Json(json!({"ok":false,"error":error}))).into_response()
                } else { crate::login::limited(lang) });
            }
            let mime = header_text(headers, "content-type").unwrap_or("").split(';').next().unwrap_or("").trim().to_ascii_lowercase();
            let fields = if mime == "application/json" || (mime.starts_with("application/") && mime.ends_with("+json")) {
                match to_bytes(body, MAX_BODY).await {
                    Ok(data) => serde_json::from_slice(&data).unwrap_or(serde_json::Value::Null),
                    Err(_) => return Ok(too_large()),
                }
            } else { serde_json::Value::Null };
            let native = path == "/api/auth/token" || (path == "/api/email/verify" && fields.get("mobile") == Some(&serde_json::Value::Bool(true)));
            let context = crate::login::Context {
                db: &state.db, profile: Arc::clone(state.profile.as_ref().expect("profile configured")),
                keys: &state.keys, stamp: timestamp as i64, locale: lang,
            };
            if path == "/api/register" {
                let registration = Arc::clone(state.signup.as_ref().expect("registration configured"));
                let (response, remove_session) = registration.register(context, headers, fields).await?;
                clear |= remove_session;
                return Ok(response);
            }
            if matches!(path, "/api/email/resend" | "/api/password/forgot") {
                return state.signup.as_ref().expect("registration configured")
                    .send_link(context, fields, path == "/api/password/forgot").await;
            }
            if path == "/api/analytics/registration" {
                return crate::registration::Registration::attribution(context,headers,fields).await;
            }
            if path == "/api/password/reset" {
                return crate::account_tokens::reset_password(context, fields).await;
            }
            if path == "/api/auth/native/exchange" {
                let response = crate::account_tokens::exchange_native(context, fields).await?;
                if response.status().is_success() { mobile = true; }
                return Ok(response);
            }
            let (mut response, uid) = if path == "/api/email/verify" {
                crate::account_tokens::verify_email(context, fields).await?
            } else {
                crate::login::authenticate(context, fields, native).await?
            };
            login_uid = uid;
            if native && response.status().is_success() {
                mobile = true;
                if path == "/api/email/verify" {
                    response.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
                }
            }
            return Ok(response);
        }
        if path == "/api/push/device" {
            return push_device(&state.db, method, uid.filter(|_| !clear), timestamp as i64, headers, body, lang).await;
        }
        if path == "/api/logout" {
            if method == Method::POST {
                clear = true;
                return Ok(Json(json!({"ok": true})).into_response());
            }
            return Ok(method_response(method, "POST, OPTIONS"));
        }
        if method != Method::GET && method != Method::HEAD {
            return Ok(method_response(method, "GET, HEAD, OPTIONS"));
        }
        let response = match path {
            "/api/recommendations" => {
                let Some(uid) = uid.filter(|_| !clear) else { return Ok(crate::account_settings::signed_out(lang)); };
                let profile=Arc::clone(state.profile.as_ref().expect("profile configured"));
                let places=state.places.as_ref().expect("places configured");
                return state.recommendations.as_ref().expect("recommendations configured").serve(&state.db,places,profile,uid,timestamp as i64,lang).await;
            }
            "/api/feed" | "/api/likes" => {
                let Some(uid) = uid.filter(|_| !clear) else { return Ok(crate::account_settings::signed_out(lang)); };
                let places = state.places.as_ref().expect("places configured");
                let profile = Arc::clone(state.profile.as_ref().expect("profile configured"));
                return crate::discovery_http::read(&state.db, places, profile, uid, path == "/api/feed", parts.uri.query().unwrap_or(""), timestamp as i64, lang).await;
            }
            "/api/places" => {
                let Some(uid) = uid.filter(|_| !clear) else { return Ok(crate::account_settings::signed_out(lang)); };
                return state.places.as_ref().expect("places configured")
                    .search(&state.db, uid, parts.uri.query().unwrap_or(""), timestamp as i64, lang).await;
            }
            path if path == "/api/matches" || path.starts_with("/api/people/") => {
                let Some(uid) = uid.filter(|_| !clear) else { return Ok(crate::account_settings::signed_out(lang)); };
                let profile = Arc::clone(state.profile.as_ref().expect("profile configured"));
                return crate::people_http::read(&state.db, profile, uid, path, timestamp as i64, lang).await;
            }
            "/health" => {
                let count: i64 = state.db.transaction(move |tx| Box::pin(async move {
                    let row = tx.query_one("SELECT count(*) FROM users WHERE COALESCE(deleted_at, 0) = 0", &[]).await?;
                    Ok(row.get(0))
                })).await?.0;
                Json(json!({"ok": true, "users": count})).into_response()
            }
            "/api/app-config" => Json(json!({"ok": true, "min_version": state.min_version, "latest_version": state.latest_version})).into_response(),
            "/api/support/replies" => {
                let Some(uid) = uid.filter(|_| !clear) else { return Ok(crate::account_settings::signed_out(lang)); };
                let stamp = timestamp as i64;
                let replies = state.db.transaction(move |tx| Box::pin(async move {
                    let updated = tx.execute("UPDATE users SET last_seen=$2::bigint WHERE id=$1::bigint AND COALESCE(deleted_at,0)=0", &[&uid, &stamp]).await?;
                    if updated == 0 { return Ok(None); }
                    // Report bans must not prevent a user from reading the reply to an appeal.
                    let rows = tx.query("SELECT id::bigint, source_kind, body, created_at::bigint
                        FROM support_replies WHERE user_id=$1::bigint ORDER BY id DESC LIMIT 40", &[&uid]).await?;
                    Ok(Some(rows.iter().map(|row| json!({"id":row.get::<_,i64>(0), "source_kind":row.get::<_,&str>(1),
                        "body":row.get::<_,&str>(2), "created_at":row.get::<_,i64>(3)})).collect::<Vec<_>>()))
                })).await?.0;
                match replies {
                    Some(replies) => Json(json!({"ok":true,"replies":replies})).into_response(),
                    None => crate::account_settings::signed_out(lang),
                }
            }
            "/api/me" => {
                let profile = Arc::clone(state.profile.as_ref().expect("profile route configured"));
                let user = if let Some(uid) = uid.filter(|_| !clear) {
                    let output = state.db.transaction(move |tx| Box::pin(async move {
                        profile.read(tx, uid, timestamp as i64, lang).await
                    })).await?.0;
                    clear |= output.1;
                    output.0
                } else { serde_json::Value::Null };
                Json(json!({"ok": true, "user": user})).into_response()
            }
            _ => ([(header::CONTENT_TYPE, "application/json")], if lang == "en" { state.en.clone() } else { state.ru.clone() }).into_response(),
        };
        Ok::<_, StatusCode>(response)
    }.await;
    let mut response = result.unwrap_or_else(|_| unavailable(lang));
    if let Some(preference) = language_preference {
        localize(&mut response, headers, preference, true, true);
    }
    localize(&mut response, headers, lang, persist, true);
    if path == "/api/auth/native/exchange" && *method == Method::POST {
        response.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        response.headers_mut().insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    }
    let secure = if state.secure { "; Secure" } else { "" };
    let cookie = if let Some(uid) = login_uid {
        let claims = json!({"_permanent": true, "uid": uid});
        let token = state.keys.sign_web(claims.as_object().expect("claims object"), timestamp);
        let expires = httpdate::fmt_http_date(now + Duration::from_secs(WEB_TTL));
        format!("wiring_session={token}; Expires={expires}{secure}; HttpOnly; Path=/; SameSite=Lax")
    } else if clear {
        format!("wiring_session=; Expires=Thu, 01 Jan 1970 00:00:00 GMT; Max-Age=0{secure}; HttpOnly; Path=/; SameSite=Lax")
    } else if let Some(web) = web.filter(|claims| !mobile && claims.get("_permanent") == Some(&serde_json::Value::Bool(true))) {
        let token = state.keys.sign_web(&web, timestamp);
        if token.len() > 3800 { return response; }
        let expires = httpdate::fmt_http_date(now + Duration::from_secs(WEB_TTL));
        format!("wiring_session={token}; Expires={expires}{secure}; HttpOnly; Path=/; SameSite=Lax")
    } else {
        return response;
    };
    if let Ok(value) = HeaderValue::from_str(&cookie) {
        response.headers_mut().append(header::SET_COOKIE, value);
    }
    response
}

async fn push_device(
    db: &Writer, method: &Method, uid: Option<i64>, now: i64, headers: &HeaderMap, body: Body, lang: &str,
) -> Result<Response, StatusCode> {
    if !matches!(*method, Method::POST | Method::DELETE) {
        return Ok(method_response(method, "POST, DELETE, OPTIONS"));
    }
    let en = lang == "en";
    let error = |status, en_msg, ru_msg| (status, Json(json!({"ok": false, "error": if en { en_msg } else { ru_msg }}))).into_response();
    let Some(uid) = uid else {
        return Ok(error(StatusCode::UNAUTHORIZED, "Please sign in", "нужна сессия"));
    };
    let mime = header_text(headers, "content-type").unwrap_or("").split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    let fields = if mime == "application/json" || (mime.starts_with("application/") && mime.ends_with("+json")) {
        match to_bytes(body, MAX_BODY).await {
            Ok(data) => serde_json::from_slice(&data).unwrap_or(serde_json::Value::Null),
            Err(_) => return Ok(too_large()),
        }
    } else { serde_json::Value::Null };
    let token = fields.get("token").and_then(serde_json::Value::as_str).unwrap_or("");
    if *method == Method::DELETE {
        let token = token.to_owned();
        let _ = db.transaction(move |tx| Box::pin(async move {
            tx.execute("DELETE FROM mobile_devices WHERE token = $1 AND user_id = $2::bigint", &[&token, &uid]).await?;
            Ok(())
        })).await?;
        return Ok(Json(json!({"ok": true})).into_response());
    }
    let token = token.trim().to_owned();
    let valid = token.strip_prefix("ExpoPushToken[").or_else(|| token.strip_prefix("ExponentPushToken["))
        .and_then(|token| token.strip_suffix(']')).is_some_and(|token| token.len() >= 8 &&
            token.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-'));
    if !valid { return Ok(error(StatusCode::BAD_REQUEST, "Invalid push token", "неверный push-токен")); }
    let platform = fields.get("platform").and_then(serde_json::Value::as_str).unwrap_or("").trim().to_ascii_lowercase();
    if !matches!(platform.as_str(), "ios" | "android") {
        return Ok(error(StatusCode::BAD_REQUEST, "platform: ios or android", "platform: ios или android"));
    }
    let saved = db.transaction(move |tx| Box::pin(async move {
        let count = tx.execute(
            "INSERT INTO mobile_devices (token, user_id, platform, created_at, updated_at)
             SELECT $1, id, $3, $4::bigint, $4::bigint FROM users
             WHERE id = $2::bigint AND COALESCE(notify_enabled, 0) != 0 AND COALESCE(notify_push, 0) != 0
             ON CONFLICT(token) DO UPDATE SET user_id = excluded.user_id, platform = excluded.platform, updated_at = excluded.updated_at",
            &[&token, &uid, &platform, &now],
        ).await?;
        Ok(count == 1)
    })).await?.0;
    Ok(if saved { Json(json!({"ok": true})).into_response() } else {
        error(StatusCode::FORBIDDEN, "Enable push notifications first", "сначала включи пуш в уведомлениях")
    })
}
