use std::{env, error::Error, fs, path::Path, sync::Arc, time::{Duration, SystemTime, UNIX_EPOCH}};

use axum::{
    Router, body::Bytes, extract::{RawQuery, State},
    http::{HeaderMap, HeaderValue, Method, Uri, header},
    response::{IntoResponse, Response}, routing::any,
};
use serde_json::Value;

use crate::{
    chat_write::Writer, mobile_http::{method_response, unavailable},
    public_http::{header_text, html_escape, localize, render_html, request_language},
    signed_session::{SessionKeys, WEB_TTL},
};

const THEMES: [&str; 5] = ["pastel", "mist", "dusk", "night", "slate"];

struct Landing {
    keys: SessionKeys,
    db: Arc<Writer>,
    html: Vec<[Bytes; 2]>,
    manifests: [Bytes; 2],
    secure: bool,
}

pub(crate) fn routes(writer: Option<Arc<Writer>>, directory: &Path) -> Result<Router, Box<dyn Error>> {
    if env::var("WIRING_PUBLIC_LANDING").as_deref() != Ok("1") { return Ok(Router::new()); }
    let secret = env::var("APP_SECRET_KEY")?;
    if secret.trim().is_empty() { return Err("Landing requires the existing signing key".into()); }
    let ios = env::var("WIRING_TESTFLIGHT_URL").unwrap_or_default();
    let android = env::var("WIRING_ANDROID_APK_URL").unwrap_or_default();
    let ios = ios.trim();
    let android = android.trim();
    let suffix = format!("{}{}", u8::from(!ios.is_empty()), u8::from(!android.is_empty()));
    let mut html = Vec::with_capacity(THEMES.len());
    for theme in THEMES {
        let file = format!("landing-{theme}-{suffix}");
        let load = |language| -> Result<Bytes, Box<dyn Error>> {
            let rendered = render_html(directory, &file, language)?;
            let rendered = std::str::from_utf8(&rendered)?
                .replace("__WIRING_TESTFLIGHT__", &html_escape(ios))
                .replace("__WIRING_APK__", &html_escape(android));
            Ok(rendered.into())
        };
        html.push([load("ru")?, load("en")?]);
    }
    let state = Landing {
        keys: SessionKeys::new(secret.as_bytes()), db: writer.ok_or("Landing requires PostgreSQL")?, html,
        manifests: [fs::read(directory.join("manifest-ru.json"))?.into(),
                    fs::read(directory.join("manifest-en.json"))?.into()],
        secure: env::var("SESSION_COOKIE_SECURE").is_ok_and(|value|
            matches!(value.to_ascii_lowercase().as_str(), "true" | "1" | "yes")),
    };
    Ok(Router::new().route("/m", any(serve)).route("/public/manifest.webmanifest", any(serve)).with_state(Arc::new(state)))
}

async fn serve(
    State(state): State<Arc<Landing>>, method: Method, uri: Uri, RawQuery(query): RawQuery, headers: HeaderMap,
) -> Response {
    let (locale, persist) = request_language(query.as_deref(), &headers);
    let now = SystemTime::now();
    let stamp = now.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    let claims = crate::spa_http::web_cookie(&headers).and_then(|token| state.keys.read_web(token, stamp));
    let uid = claims.as_ref().and_then(|claims| claims.get("uid")).and_then(Value::as_i64);
    let landing = uri.path() == "/m";
    let mut clear = false;
    let mut failed = false;
    if landing && method != Method::OPTIONS {
        if let Some(ua) = header_text(&headers, "user-agent").map(str::trim).filter(|value| !value.is_empty()) {
            let ip = header_text(&headers, "x-forwarded-for").unwrap_or("x").split(',').next().unwrap_or("x").trim();
            let visit = crate::visit::Visit::new(ip, ua);
            match state.db.transaction(move |tx| Box::pin(crate::visit::track(tx, uid, stamp as i64, visit))).await {
                Ok(axum::Json(exists)) => clear = !exists,
                Err(_) => failed = true,
            }
        }
    }
    let allowed = matches!(method, Method::GET | Method::HEAD);
    let mut response = if failed { unavailable(locale) }
    else if !allowed { method_response(&method, "GET, HEAD, OPTIONS") }
    else {
        let lang = usize::from(locale == "en");
        if landing {
            let theme = headers.get_all(header::COOKIE).iter().filter_map(|header| header.to_str().ok())
                .flat_map(|header| header.split(';')).find_map(|part| {
                    let (key, val) = part.trim().split_once('=')?;
                    (key == "wiring-theme").then_some(val)
                }).and_then(|name| THEMES.iter().position(|theme| *theme == name)).unwrap_or_default();
            ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], state.html[theme][lang].clone()).into_response()
        } else {
            ([(header::CONTENT_TYPE, "application/manifest+json"), (header::CACHE_CONTROL, "no-cache")],
                state.manifests[lang].clone()).into_response()
        }
    };
    localize(&mut response, &headers, locale, persist, landing || !allowed || failed);
    if !landing && allowed && !failed { response.headers_mut().remove(header::VARY); }
    let secure = if state.secure { "; Secure" } else { "" };
    let cookie = if clear {
        format!("wiring_session=; Expires=Thu, 01 Jan 1970 00:00:00 GMT; Max-Age=0{secure}; HttpOnly; Path=/; SameSite=Lax")
    } else if let Some(claims) = claims.filter(|claims| claims.get("_permanent") == Some(&Value::Bool(true))) {
        let token = state.keys.sign_web(&claims, stamp);
        if token.len() > 3800 { return response; }
        let expires = httpdate::fmt_http_date(now + Duration::from_secs(WEB_TTL));
        if !landing && allowed { response.headers_mut().insert(header::VARY, HeaderValue::from_static("Cookie")); }
        format!("wiring_session={token}; Expires={expires}{secure}; HttpOnly; Path=/; SameSite=Lax")
    } else { return response; };
    if let Ok(cookie) = HeaderValue::from_str(&cookie) { response.headers_mut().append(header::SET_COOKIE, cookie); }
    response
}
