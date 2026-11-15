use std::{collections::HashMap, env, fs, io, path::Path, sync::Arc};

use axum::{
    Router,
    body::Bytes,
    extract::{RawQuery, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode, Uri, header},
    response::{IntoResponse, Response},
};
use serde_json::json;

const EDITORIAL_PATHS: [&str; 7] = [
    "/account-deletion", "/child-safety", "/privacy", "/legal", "/rules", "/marketing", "/glossary",
];
const SITEMAP_PATHS: [&str; 9] = [
    "/", "/m", "/legal", "/rules", "/privacy", "/marketing", "/support", "/child-safety", "/account-deletion",
];

struct Document {
    content_type: &'static str,
    ru: Bytes,
    en: Bytes,
}

pub(crate) fn html_escape(value: &str) -> String {
    value.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
        .replace('"', "&#34;").replace('\'', "&#39;")
}

fn script_string(value: &str) -> String {
    json!(value).to_string().replace('<', "\\u003c").replace('>', "\\u003e")
        .replace('&', "\\u0026").replace('\'', "\\u0027")
}

fn get_setting(name: &str, default: &str) -> String {
    env::var(name).unwrap_or_else(|_| default.to_owned()).trim().to_owned()
}

pub(crate) fn render_html(directory: &Path, name: &str, language: &str) -> io::Result<Bytes> {
    let origin = get_setting("SITE_URL", "https://wiring.date").trim_end_matches('/').to_owned();
    let google = get_setting("GOOGLE_ANALYTICS_ID", "G-WH72XL7E2J");
    let yandex = get_setting("YANDEX_METRIKA_ID", "");
    let suffix = if google.is_empty() && yandex.is_empty() { "" } else { "-analytics" };
    let source = fs::read_to_string(directory.join(format!("{name}-{language}{suffix}.html")))?;
    Ok(source.replace("\"__WIRING_GOOGLE_ANALYTICS__\"", &script_string(&google))
        .replace("\"__WIRING_YANDEX_METRIKA__\"", &script_string(&yandex))
        .replace("__WIRING_SITE_ORIGIN__", &html_escape(&origin)).into())
}

pub(crate) fn router(directory: &Path) -> io::Result<Router> {
    let origin = get_setting("SITE_URL", "https://wiring.date").trim_end_matches('/').to_owned();
    let mut documents = HashMap::new();
    for path in EDITORIAL_PATHS {
        documents.insert(path, Document {
            content_type: "text/html; charset=utf-8",
            ru: render_html(directory, &path[1..], "ru")?,
            en: render_html(directory, &path[1..], "en")?,
        });
    }
    let fingerprints: Vec<_> = get_setting("ANDROID_APP_LINK_SHA256", "").split(',')
        .map(|value| value.trim().to_ascii_uppercase())
        .filter(|value| value.len() == 95 && value.split(':').all(|byte| {
            byte.len() == 2 && byte.bytes().all(|digit| digit.is_ascii_hexdigit())
        }))
        .collect();
    let asset_links = if fingerprints.is_empty() {
        json!([])
    } else {
        json!([{"relation": ["delegate_permission/common.handle_all_urls"], "target": {
            "namespace": "android_app", "package_name": "date.wiring.app", "sha256_cert_fingerprints": fingerprints
        }}])
    };
    let locations: String = SITEMAP_PATHS.iter()
        .map(|path| format!("<url><loc>{}{path}</loc></url>", html_escape(&origin)))
        .collect();
    let metadata = [
        ("/robots.txt", "text/plain; charset=utf-8",
         format!("User-agent: *\nAllow: /\nDisallow: /admin\nSitemap: {origin}/sitemap.xml\n").into_bytes()),
        ("/sitemap.xml", "application/xml; charset=utf-8",
         format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?><urlset xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\">{locations}</urlset>").into_bytes()),
        ("/.well-known/apple-app-site-association", "application/json",
         fs::read(directory.join("apple-app-site-association.json"))?),
        ("/.well-known/assetlinks.json", "application/json", asset_links.to_string().into_bytes()),
    ];
    for (path, content_type, bytes) in metadata {
        let bytes = Bytes::from(bytes);
        documents.insert(path, Document { content_type, ru: bytes.clone(), en: bytes });
    }
    Ok(Router::new().fallback(serve).with_state(Arc::new(documents)))
}

fn language(value: &str) -> Option<&'static str> {
    match value {
        "en" => Some("en"),
        "ru" => Some("ru"),
        _ => None,
    }
}

pub(crate) fn header_text<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name)?.to_str().ok()
}

pub(crate) fn request_language(query: Option<&str>, headers: &HeaderMap) -> (&'static str, bool) {
    let selected = query.and_then(|query| {
        form_urlencoded::parse(query.as_bytes()).find(|(key, _)| key == "lang")
            .and_then(|(_, value)| language(&value))
    });
    let cookie = headers.get_all(header::COOKIE).iter().filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';')).find_map(|value| {
            let (key, value) = value.trim().split_once('=')?;
            (key == "wiring-language").then_some(value)
        }).and_then(language);
    let locale = header_text(headers, "x-wiring-language").and_then(language)
        .or(selected).or(cookie).unwrap_or("ru");
    (locale, selected.is_some())
}

pub(crate) fn localize(response: &mut Response, headers: &HeaderMap, locale: &'static str, persist: bool, localized: bool) {
    if localized {
        response.headers_mut().insert(header::CONTENT_LANGUAGE, HeaderValue::from_static(locale));
        response.headers_mut().insert(header::VARY, HeaderValue::from_static("Cookie, X-Wiring-Language"));
    } else if header_text(headers, "user-agent").is_some_and(|value| !value.trim().is_empty()) {
        response.headers_mut().insert(header::VARY, HeaderValue::from_static("Cookie"));
    }
    if persist {
        let secure = if header_text(headers, "x-forwarded-proto") == Some("https") { "; Secure" } else { "" };
        let expires = httpdate::fmt_http_date(std::time::SystemTime::now() + std::time::Duration::from_secs(31_536_000));
        let cookie = format!("wiring-language={locale}; Expires={expires}; Max-Age=31536000{secure}; Path=/; SameSite=Lax");
        if let Ok(value) = HeaderValue::from_str(&cookie) {
            response.headers_mut().append(header::SET_COOKIE, value);
        }
    }
}

async fn serve(
    State(documents): State<Arc<HashMap<&'static str, Document>>>,
    method: Method,
    uri: Uri,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
) -> Response {
    let Some(document) = documents.get(uri.path()) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let (locale, persist) = request_language(query.as_deref(), &headers);
    let mut response = match method {
        Method::GET | Method::HEAD => {
            let bytes = if locale == "en" { &document.en } else { &document.ru };
            let mut response = if method == Method::HEAD { Bytes::new() } else { bytes.clone() }.into_response();
            response.headers_mut().insert(header::CONTENT_LENGTH, bytes.len().into());
            response.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static(document.content_type));
            response
        }
        Method::OPTIONS => (
            [(header::ALLOW, "GET, HEAD, OPTIONS"), (header::CONTENT_TYPE, "text/html; charset=utf-8")], ""
        ).into_response(),
        _ => (
            StatusCode::METHOD_NOT_ALLOWED,
            [(header::ALLOW, "GET, HEAD, OPTIONS"), (header::CONTENT_TYPE, "text/html; charset=utf-8")],
            "<!doctype html>\n<html lang=en>\n<title>405 Method Not Allowed</title>\n<h1>Method Not Allowed</h1>\n<p>The method is not allowed for the requested URL.</p>\n",
        ).into_response(),
    };
    let localized = method != Method::GET && method != Method::HEAD
        || document.content_type.starts_with("text/html") || document.content_type == "application/json";
    localize(&mut response, &headers, locale, persist, localized);
    response
}
