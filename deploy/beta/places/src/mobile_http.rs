use std::{env, error::Error, sync::Arc, time::{SystemTime, UNIX_EPOCH}};

use axum::{
    Json, Router,
    body::{Body, to_bytes},
    extract::State,
    http::{Method, Request, StatusCode, header},
    response::{IntoResponse, Response},
    routing::any,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use pbkdf2::sha2::{Digest, Sha256};
use serde_json::{Value, json};

use crate::{
    chat_write::Writer,
    mobile_session::{self, RotationOutcome},
    public_http::{header_text, localize, request_language},
    signed_session::{AccessClaims, MOBILE_TTL, SessionKeys},
};

pub(crate) const REFRESH_TTL: i64 = 90 * 24 * 60 * 60;
pub(crate) const MAX_BODY: usize = 9 * 1024 * 1024;

struct Mobile {
    writer: Arc<Writer>,
    keys: SessionKeys,
    min_version: String,
}

pub(crate) fn routes(writer: Option<Arc<Writer>>) -> Result<Router, Box<dyn Error>> {
    if env::var("WIRING_PUBLIC_MOBILE_SESSIONS").as_deref() != Ok("1") {
        return Ok(Router::new());
    }
    let writer = writer.ok_or("Mobile sessions require PostgreSQL")?;
    let secret = env::var("APP_SECRET_KEY")?;
    if secret.trim().is_empty() {
        return Err("Mobile sessions require the existing signing key".into());
    }
    let mobile = Mobile {
        writer,
        keys: SessionKeys::new(secret.as_bytes()),
        min_version: env::var("MOBILE_MIN_VERSION").unwrap_or_default().trim().to_owned(),
    };
    Ok(Router::new()
        .route("/api/auth/refresh", any(session))
        .route("/api/auth/logout", any(session))
        .with_state(Arc::new(mobile)))
}

async fn session(State(state): State<Arc<Mobile>>, request: Request<Body>) -> Response {
    let (parts, body) = request.into_parts();
    let (locale, persist) = request_language(parts.uri.query(), &parts.headers);
    let version = header_text(&parts.headers, "x-app-version").unwrap_or("");
    let mut response = if let Some(upgrade) = upgrade(version, &state.min_version, locale) {
        upgrade
    } else if parts.method == Method::OPTIONS {
        method_response(&parts.method, "POST, OPTIONS")
    } else if parts.method != Method::POST {
        method_response(&parts.method, "POST, OPTIONS")
    } else {
        let content_type = header_text(&parts.headers, "content-type").unwrap_or("");
        let mime = content_type.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
        let mut fields = Value::Null;
        if mime == "application/json" || (mime.starts_with("application/") && mime.ends_with("+json")) {
            match to_bytes(body, MAX_BODY).await {
                Ok(data) => fields = serde_json::from_slice(&data).unwrap_or(Value::Null),
                Err(_) => return finish(too_large(), &parts.headers, locale, persist),
            }
        }
        let token = string_field(&fields, "refresh_token");
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() as i64;
        if parts.uri.path() == "/api/auth/logout" {
            if token.is_empty() {
                Json(json!({"ok": true})).into_response()
            } else {
                let logout = mobile_session::logout_request(token_hash(&token), string_field(&fields, "push_token"), now);
                match state.writer.transaction(move |tx| Box::pin(mobile_session::revoke(tx, logout))).await {
                    Ok(response) => response.into_response(),
                    Err(_) => unavailable(locale),
                }
            }
        } else {
            let mut random = [0u8; 48];
            if getrandom::fill(&mut random).is_err() {
                return finish(unavailable(locale), &parts.headers, locale, persist);
            }
            let new_token = URL_SAFE_NO_PAD.encode(random);
            let rotation = mobile_session::rotation(token_hash(&token), token_hash(&new_token), now, now + REFRESH_TTL);
            match state.writer.transaction(move |tx| Box::pin(mobile_session::rotate(tx, rotation))).await {
                Ok(Json(RotationOutcome::Rotated { user_id, fingerprint })) => {
                    let claims = AccessClaims { uid: user_id, pw: fingerprint };
                    let access = state.keys.sign_access(&claims, now as u64);
                    Json(json!({"ok": true, "access_token": access, "refresh_token": new_token, "expires_in": MOBILE_TTL})).into_response()
                }
                Ok(Json(RotationOutcome::Invalid)) => {
                    let message = if locale == "en" { "Invalid session" } else { "сессия недействительна" };
                    (StatusCode::UNAUTHORIZED, Json(json!({"ok": false, "error": message, "token_expired": true}))).into_response()
                }
                Err(_) => unavailable(locale),
            }
        }
    };
    localize(&mut response, &parts.headers, locale, persist, true);
    response
}

fn finish(mut response: Response, headers: &axum::http::HeaderMap, locale: &'static str, persist: bool) -> Response {
    localize(&mut response, headers, locale, persist, true);
    response
}

pub(crate) fn too_large() -> Response {
    (StatusCode::PAYLOAD_TOO_LARGE,
     [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
     "<!doctype html>\n<html lang=en>\n<title>413 Request Entity Too Large</title>\n<h1>Request Entity Too Large</h1>\n<p>The data value transmitted exceeds the capacity limit.</p>\n").into_response()
}

pub(crate) fn method_response(method: &Method, allow: &'static str) -> Response {
    let headers = [(header::ALLOW, allow), (header::CONTENT_TYPE, "text/html; charset=utf-8")];
    if *method == Method::OPTIONS {
        return (headers, "").into_response();
    }
    (StatusCode::METHOD_NOT_ALLOWED, headers,
     "<!doctype html>\n<html lang=en>\n<title>405 Method Not Allowed</title>\n<h1>Method Not Allowed</h1>\n<p>The method is not allowed for the requested URL.</p>\n").into_response()
}

pub(crate) fn upgrade(version: &str, minimum: &str, locale: &str) -> Option<Response> {
    if version.is_empty() || minimum.is_empty() || version_parts(version) >= version_parts(minimum) {
        return None;
    }
    let message = if locale == "en" { "This app version is out of date. Please update WIRING." }
        else { "версия приложения устарела — обнови WIRING" };
    Some((StatusCode::UPGRADE_REQUIRED, Json(json!({
        "ok": false, "error": message, "upgrade": true, "min_version": minimum,
    }))).into_response())
}

pub(crate) fn unavailable(locale: &str) -> Response {
    let message = if locale == "en" { "The server is temporarily unavailable" } else { "сервер временно недоступен" };
    (StatusCode::SERVICE_UNAVAILABLE, Json(json!({"ok": false, "error": message}))).into_response()
}

pub(crate) fn token_hash(value: &str) -> String {
    Sha256::digest(value.as_bytes()).iter().map(|byte| format!("{byte:02x}")).collect()
}

pub(crate) fn string_field(input: &Value, field: &str) -> String {
    match input.get(field) {
        Some(Value::String(value)) => value.clone(),
        Some(Value::Bool(true)) => "True".to_owned(),
        Some(Value::Number(n)) if n.as_f64() != Some(0.0) => n.to_string(),
        _ => String::new(),
    }
}

fn version_parts(value: &str) -> Vec<(usize, String)> {
    let parts: Vec<_> = value.split(|c: char| !c.is_ascii_digit()).filter(|part| !part.is_empty())
        .take(4).map(|part| {
            let significant = part.trim_start_matches('0');
            (significant.len(), significant.to_owned())
        }).collect();
    if parts.is_empty() { vec![(0, String::new())] } else { parts }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mobile_version_comparison_matches_numeric_tuples() {
        assert!(version_parts("1.9") < version_parts("1.10"));
        assert!(version_parts("v01.2") < version_parts("1.2.0"));
        assert_eq!(version_parts("v1.02.3.4.5"), version_parts("1.2.3.4"));
        assert_eq!(version_parts("missing"), version_parts("0"));
    }
}
