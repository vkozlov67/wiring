use std::{env, error::Error, path::Path, sync::Arc, time::{Duration, SystemTime, UNIX_EPOCH}};

use axum::{
    Router,
    body::Bytes,
    extract::{RawQuery, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode, Uri, header},
    response::{IntoResponse, Response},
    routing::{any,get},
};
use serde_json::{Map, Value};

use crate::{
    chat_write::Writer,
    mobile_http::{method_response, unavailable, upgrade},
    public_http::{header_text, localize, render_html, request_language},
    signed_session::{SessionKeys, WEB_TTL},
};

const PATHS: &[&str] = &[
    "/", "/sign-in", "/sign-up", "/login", "/register", "/forgot", "/reset", "/verify",
    "/feed", "/for-you", "/likes", "/chats", "/me", "/me/edit", "/consents", "/plus",
    "/premium", "/onboard", "/delete-account", "/support", "/notifications", "/archive",
];

struct Spa {
    db: Arc<Writer>,
    keys: SessionKeys,
    secure_cookie: bool,
    min_version: String,
    ru: Bytes,
    en: Bytes,
}

pub(crate) fn web_cookie(headers: &HeaderMap) -> Option<&str> {
    headers.get_all(header::COOKIE).iter().filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';')).find_map(|value| {
            let (key, value) = value.trim().split_once('=')?;
            (key == "wiring_session").then_some(value)
        })
}

pub(crate) fn routes(writer: Option<Arc<Writer>>, directory: &Path) -> Result<Router, Box<dyn Error>> {
    if env::var("WIRING_PUBLIC_SPA").as_deref() != Ok("1") {
        return Ok(Router::new());
    }
    let key = env::var("APP_SECRET_KEY")?;
    if key.trim().is_empty() {
        return Err("Public SPA requires the existing signing key".into());
    }
    let spa = Arc::new(Spa {
        db: writer.ok_or("Public SPA requires PostgreSQL")?,
        keys: SessionKeys::new(key.as_bytes()),
        secure_cookie: env::var("SESSION_COOKIE_SECURE").is_ok_and(|value| {
            matches!(value.to_ascii_lowercase().as_str(), "true" | "1" | "yes")
        }),
        min_version: env::var("MOBILE_MIN_VERSION").unwrap_or_default().trim().to_owned(),
        ru: render_html(directory, "spa", "ru")?,
        en: render_html(directory, "spa", "en")?,
    });
    let mut router = Router::new();
    for path in PATHS.iter().copied().chain(["/chats/{id}", "/p/{id}", "/r/{code}"]) {
        router = router.route(path, if path=="/support" && env::var("WIRING_PUBLIC_SUPPORT_FORM").as_deref()==Ok("1") {get(serve)}else{any(serve)});
    }
    Ok(router.with_state(spa))
}

async fn serve(
    State(state): State<Arc<Spa>>, method: Method, uri: Uri, RawQuery(query): RawQuery, headers: HeaderMap,
) -> Response {
    let (language, persist) = request_language(query.as_deref(), &headers);
    let now = SystemTime::now();
    let timestamp = now.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    let mut claims = web_cookie(&headers).and_then(|cookie| state.keys.read_web(cookie, timestamp)).unwrap_or_default();
    let mut bearer_uid=None;
    let mut modified = false;
    let mut receipt = String::new();
    let respond = |mut response: Response, claims: &Map<String, Value>, modified: bool, receipt: &str| {
        localize(&mut response, &headers, language, persist, true);
        let secure = if state.secure_cookie { "; Secure" } else { "" };
        if modified && claims.is_empty() {
            append_cookie(&mut response, format!("wiring_session=; Expires=Thu, 01 Jan 1970 00:00:00 GMT; Max-Age=0{secure}; HttpOnly; Path=/; SameSite=Lax"));
        } else {
            let permanent = claims.get("_permanent") == Some(&Value::Bool(true));
            if permanent || modified {
                let token = state.keys.sign_web(claims, timestamp);
                let expires = if permanent {
                    format!("; Expires={}", httpdate::fmt_http_date(now + Duration::from_secs(WEB_TTL)))
                } else { String::new() };
                if token.len() <= 3800 {
                    append_cookie(&mut response, format!("wiring_session={token}{expires}{secure}; HttpOnly; Path=/; SameSite=Lax"));
                }
            }
        }
        if !receipt.is_empty() {
            let secure = if header_text(&headers, "x-forwarded-proto") == Some("https") { "; Secure" } else { "" };
            let expires = httpdate::fmt_http_date(now + Duration::from_secs(43_200));
            append_cookie(&mut response, format!("wiring-registration-receipt={receipt}; Expires={expires}; Max-Age=43200{secure}; HttpOnly; Path=/api/analytics; SameSite=Lax"));
        }
        response
    };
    let path = uri.path();
    if path == "/support" {
        // The support shell still enforces the mobile version floor.
        if let Some(response) = upgrade(header_text(&headers, "x-app-version").unwrap_or(""), &state.min_version, language) {
            return respond(response, &claims, false, "");
        }
        if let Some(token)=header_text(&headers,"authorization").and_then(|value| {
            let (scheme,token)=value.split_once(' ')?;
            scheme.eq_ignore_ascii_case("bearer").then_some(token.trim())
        }) {
            claims.clear();
            let invalid=|| {
                let error=if language=="en"{"Invalid token"}else{"токен недействителен"};
                let response=(StatusCode::UNAUTHORIZED,axum::Json(serde_json::json!({"ok":false,"token_expired":true,"error":error}))).into_response();
                respond(response,&claims,false,"")
            };
            let Some(access)=state.keys.read_access(token,timestamp) else {return invalid();};
            let user=access.uid;
            match state.db.transaction(move |tx| Box::pin(async move {
                let row=tx.query_opt("SELECT password_hash, COALESCE(deleted_at,0)!=0 FROM users WHERE id=$1::bigint",&[&user]).await?;
                Ok(row.is_some_and(|row|crate::mobile_session::matches_access(&access,row.get(0),row.get(1))))
            })).await {
                Ok(axum::Json(true))=>bearer_uid=Some(user),
                Ok(axum::Json(false))=>return invalid(),
                Err(_)=>return respond(unavailable(language),&claims,false,""),
            }
        }
    }
    let dynamic = path.strip_prefix("/p/").or_else(|| path.strip_prefix("/chats/"));
    if dynamic.is_some_and(|value| !value.bytes().all(|byte| byte.is_ascii_digit())) {
        return respond((StatusCode::NOT_FOUND, [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
            "<!doctype html>\n<html lang=en>\n<title>404 Not Found</title>\n<h1>Not Found</h1>\n<p>The requested URL was not found on the server. If you entered the URL manually please check your spelling and try again.</p>\n"
        ).into_response(), &claims, false, "");
    }
    if method != Method::GET && method != Method::HEAD {
        let allow = if path == "/support" { "GET, HEAD, OPTIONS, POST" } else { "GET, HEAD, OPTIONS" };
        return respond(method_response(&method, allow), &claims, false, "");
    }
    // Preserve the device counters used by the admin panel when a browser supplies a UA.
    if let Some(ua) = header_text(&headers, "user-agent").map(str::trim).filter(|value| !value.is_empty()) {
        let uid = bearer_uid.or_else(||claims.get("uid").and_then(Value::as_i64));
        let ip = header_text(&headers, "x-forwarded-for").unwrap_or("x").split(',').next().unwrap_or("x").trim();
        let visit = crate::visit::Visit::new(ip, ua);
        match state.db.transaction(move |tx| Box::pin(crate::visit::track(tx, uid, timestamp as i64, visit))).await {
            Ok(axum::Json(false)) if uid.is_some() && bearer_uid.is_none() => {
                claims.clear();
                modified = true;
            }
            Err(_) => return respond(unavailable(language), &claims, false, ""),
            _ => {},
        }
    }
    let created = claims.remove("registration_created");
    let managed = claims.remove("registration_receipt");
    modified |= created.is_some() || managed.is_some();
    if let Some(Value::String(token)) = managed {
        if token.len() <= 512 && token.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b)) {
            receipt = token;
        }
    }
    let mut html = if language == "en" { state.en.clone() } else { state.ru.clone() };
    if created.as_ref().is_some_and(truthy) || !receipt.is_empty() {
        let mut rendered = std::str::from_utf8(&html).expect("rendered HTML is UTF-8").to_owned();
        if created.as_ref().is_some_and(truthy) {
            rendered = rendered.replace("data-registration-created=\"false\"", "data-registration-created=\"true\"");
        }
        if !receipt.is_empty() {
            rendered = rendered.replace("data-registration-managed=\"false\"", "data-registration-managed=\"true\"");
        }
        html = rendered.into();
    }
    let size = html.len();
    let mut response = html.into_response();
    response.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("text/html; charset=utf-8"));
    response.headers_mut().insert(header::CONTENT_LENGTH, size.into());
    response.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    respond(response, &claims, modified, &receipt)
}

fn truthy(value: &Value) -> bool {
    match value {
        Value::Null | Value::Bool(false) => false,
        Value::Number(value) => value.as_f64() != Some(0.0),
        Value::String(value) => !value.is_empty(),
        Value::Array(value) => !value.is_empty(),
        Value::Object(value) => !value.is_empty(),
        Value::Bool(true) => true,
    }
}

fn append_cookie(response: &mut Response, cookie: String) {
    if let Ok(value) = HeaderValue::from_str(&cookie) {
        response.headers_mut().append(header::SET_COOKIE, value);
    }
}
