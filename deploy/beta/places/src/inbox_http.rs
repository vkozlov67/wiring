use std::{env, error::Error, sync::Arc, time::{Duration, SystemTime, UNIX_EPOCH}};

use axum::{
    Json, Router,
    body::{Body, to_bytes},
    extract::State,
    http::{Method, Request, StatusCode, header::{self, HeaderValue}},
    response::{IntoResponse, Response},
    routing::any,
};
use serde_json::{Map, Value, json};

use crate::{
    chat_read::{self, InboxCounts, Notice},
    chat_write::Writer,
    mobile_http::{MAX_BODY, method_response, too_large, unavailable, upgrade},
    mobile_session::matches_access,
    public_http::{header_text, localize, request_language},
    signed_session::{AccessClaims, SessionKeys, WEB_TTL},
};

struct Inbox {
    writer: Arc<Writer>,
    keys: SessionKeys,
    min_version: String,
    secure: bool,
}

enum Auth {
    Mobile(Option<AccessClaims>),
    Web(Option<i64>),
}

enum Action {
    Inbox,
    Notices(Vec<i64>),
    OptionsOrWrongMethod,
}

enum Outcome {
    InvalidToken,
    SignedOut,
    Banned,
    Allowed,
    Inbox(InboxCounts, Vec<Notice>),
}

pub(crate) fn routes(writer: Option<Arc<Writer>>) -> Result<Router, Box<dyn Error>> {
    if env::var("WIRING_PUBLIC_INBOX").as_deref() != Ok("1") {
        return Ok(Router::new());
    }
    let writer = writer.ok_or("Public inbox requires PostgreSQL")?;
    let key = env::var("APP_SECRET_KEY")?;
    if key.trim().is_empty() {
        return Err("Public inbox requires the existing signing key".into());
    }
    let inbox = Inbox {
        writer,
        keys: SessionKeys::new(key.as_bytes()),
        min_version: env::var("MOBILE_MIN_VERSION").unwrap_or_default().trim().to_owned(),
        secure: env::var("SESSION_COOKIE_SECURE").is_ok_and(|value| {
            matches!(value.to_ascii_lowercase().as_str(), "true" | "1" | "yes")
        }),
    };
    Ok(Router::new()
        .route("/api/inbox", any(serve))
        .route("/api/notices/read", any(serve))
        .with_state(Arc::new(inbox)))
}

async fn serve(State(state): State<Arc<Inbox>>, request: Request<Body>) -> Response {
    let (parts, body) = request.into_parts();
    let (language, persist) = request_language(parts.uri.query(), &parts.headers);
    let version = header_text(&parts.headers, "x-app-version").unwrap_or("");
    let now = SystemTime::now();
    let timestamp = now.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    let web = crate::spa_http::web_cookie(&parts.headers).and_then(|cookie| state.keys.read_web(cookie, timestamp));
    let finish = |mut response: Response, mobile: bool, clear: bool| {
        localize(&mut response, &parts.headers, language, persist, true);
        if !mobile {
            if clear {
                let secure = if state.secure { "; Secure" } else { "" };
                let value = format!("wiring_session=; Expires=Thu, 01 Jan 1970 00:00:00 GMT; Max-Age=0{secure}; HttpOnly; Path=/; SameSite=Lax");
                if let Ok(value) = HeaderValue::from_str(&value) {
                    response.headers_mut().append(header::SET_COOKIE, value);
                }
            } else {
                renew(&mut response, &state, web.as_ref(), now, timestamp);
            }
        }
        response
    };
    if let Some(response) = upgrade(version, &state.min_version, language) {
        return finish(response, false, false);
    }
    let access = header_text(&parts.headers, "authorization").and_then(|header| {
        let (scheme, token) = header.split_once(' ')?;
        scheme.eq_ignore_ascii_case("Bearer").then_some(token.trim())
    });
    let auth = if let Some(token) = access {
        Auth::Mobile(state.keys.read_access(token, timestamp))
    } else {
        Auth::Web(web.as_ref().and_then(|claims| claims.get("uid")).and_then(Value::as_i64))
    };
    let inbox = parts.uri.path() == "/api/inbox";
    let allow = if inbox { "GET, HEAD, OPTIONS" } else { "POST, OPTIONS" };
    let action = if inbox && matches!(parts.method, Method::GET | Method::HEAD) {
        Action::Inbox
    } else if !inbox && parts.method == Method::POST {
        let mime = header_text(&parts.headers, "content-type").unwrap_or("")
            .split(';').next().unwrap_or("").trim().to_ascii_lowercase();
        let ids = if mime == "application/json" || (mime.starts_with("application/") && mime.ends_with("+json")) {
            match to_bytes(body, MAX_BODY).await {
                Ok(data) => notice_ids(serde_json::from_slice(&data).unwrap_or(Value::Null)),
                Err(_) => return finish(too_large(), access.is_some(), false),
            }
        } else { Vec::new() };
        Action::Notices(ids)
    } else {
        Action::OptionsOrWrongMethod
    };
    let action_requires_auth = !matches!(action, Action::OptionsOrWrongMethod);
    if matches!(auth, Auth::Web(None)) && !action_requires_auth {
        return finish(method_response(&parts.method, allow), false, false);
    }
    if matches!(auth, Auth::Web(None)) && action_requires_auth {
        return finish(response(Outcome::SignedOut, language, &parts.method, allow), false, false);
    }
    if matches!(auth, Auth::Mobile(None)) {
        return finish(response(Outcome::InvalidToken, language, &parts.method, allow), false, false);
    }
    let result = state.writer.transaction(move |tx| Box::pin(async move {
        let (uid, access) = match auth {
            Auth::Mobile(Some(claims)) => (claims.uid, Some(claims)),
            Auth::Web(Some(uid)) => (uid, None),
            _ => return Ok((Outcome::SignedOut, false)),
        };
        let row = tx.query_opt(
            "SELECT password_hash, COALESCE(deleted_at, 0) != 0, COALESCE(report_banned_at, 0) != 0
             FROM users WHERE id = $1::bigint",
            &[&uid],
        ).await?;
        let Some(row) = row else {
            return Ok((if access.is_some() { Outcome::InvalidToken } else { Outcome::SignedOut }, access.is_none()));
        };
        if let Some(claims) = access {
            if !matches_access(&claims, row.get(0), row.get(1)) {
                return Ok((Outcome::InvalidToken, false));
            }
        }
        if !action_requires_auth { return Ok((Outcome::Allowed, false)); }
        if row.get::<_, bool>(2) { return Ok((Outcome::Banned, false)); }
        tx.execute("UPDATE users SET last_seen = $2::bigint WHERE id = $1::bigint", &[&uid, &(timestamp as i64)]).await?;
        match action {
            Action::Inbox => Ok((Outcome::Inbox(chat_read::inbox_counts(tx, uid).await?,
                chat_read::unread_notices(tx, uid, 8).await?), false)),
            Action::Notices(ids) => {
                chat_read::mark_notices_read(tx, uid, ids).await?;
                Ok((Outcome::Allowed, false))
            }
            Action::OptionsOrWrongMethod => unreachable!(),
        }
    })).await;
    match result {
        Ok(Json((outcome, clear))) => {
            let mobile = access.is_some() && !matches!(outcome, Outcome::InvalidToken);
            finish(response(outcome, language, &parts.method, allow), mobile, clear)
        }
        Err(_) => finish(unavailable(language), access.is_some(), false),
    }
}

fn renew(response: &mut Response, state: &Inbox, claims: Option<&Map<String, Value>>, now: SystemTime, timestamp: u64) {
    let Some(claims) = claims else { return };
    if claims.get("_permanent") != Some(&Value::Bool(true)) { return; }
    let token = state.keys.sign_web(claims, timestamp);
    if token.len() > 3800 { return; }
    let expires = httpdate::fmt_http_date(now + Duration::from_secs(WEB_TTL));
    let secure = if state.secure { "; Secure" } else { "" };
    let value = format!("wiring_session={token}; Expires={expires}{secure}; HttpOnly; Path=/; SameSite=Lax");
    if let Ok(value) = HeaderValue::from_str(&value) {
        response.headers_mut().append(header::SET_COOKIE, value);
    }
}

fn response(outcome: Outcome, language: &str, method: &Method, allow: &'static str) -> Response {
    let en = language == "en";
    match outcome {
        Outcome::InvalidToken => (
            StatusCode::UNAUTHORIZED, Json(json!({
                "ok": false, "token_expired": true, "error": if en { "Invalid token" } else { "токен недействителен" },
            })),
        ).into_response(),
        Outcome::SignedOut => (
            StatusCode::UNAUTHORIZED, Json(json!({"ok": false, "error": if en { "Please sign in" } else { "нужна сессия" }})),
        ).into_response(),
        Outcome::Banned => {
            let error = if en { "Your account was hidden after reports from two different members. If you think this is a mistake, contact support for a manual review." }
                else { "Аккаунт скрыт после жалоб от двух разных участников. Если считаешь, что это ошибка, напиши в поддержку — разберём вручную." };
            (StatusCode::FORBIDDEN, Json(json!({"ok": false, "report_banned": true, "error": error}))).into_response()
        }
        Outcome::Allowed if *method == Method::POST && allow == "POST, OPTIONS" => Json(json!({"ok": true})).into_response(),
        Outcome::Allowed => method_response(method, allow),
        Outcome::Inbox(counts, mut notices) => {
            for notice in &mut notices { notice.body = notice_body(&notice.kind, &notice.body, en); }
            Json(json!({
                "ok": true, "likes_in": counts.likes_in, "unread": counts.unread,
                "unread_chats": counts.unread_chats, "notices": notices,
            })).into_response()
        }
    }
}

fn notice_ids(input: Value) -> Vec<i64> {
    input.get("ids").and_then(Value::as_array).into_iter().flatten().filter_map(|id| {
        match id {
            Value::Bool(v) => Some(i64::from(*v)),
            Value::Number(v) => v.as_i64().or_else(|| v.as_f64().map(|number| {
                if (-9_223_372_036_854_775_808.0..9_223_372_036_854_775_808.0).contains(&number) { number as i64 } else { 0 }
            })),
            Value::String(v) => v.trim().parse().ok().or_else(|| {
                let digits = v.trim().trim_start_matches(['+', '-']);
                (!digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())).then_some(0)
            }),
            _ => None,
        }
    }).collect()
}

pub(crate) fn notice_body(kind: &str, body: &str, english: bool) -> String {
    let (ru_before, ru_after, en_before, en_after) = match kind {
        "support" => return if english { "Support replied to your request. Read it in Support." }
            else { "Поддержка ответила на твоё обращение. Ответ — в разделе «Поддержка»." }.into(),
        "like" => ("", " лайкнул(а) тебя", "", " liked you"),
        "message" => ("", " написал(а)", "", " sent a message"),
        "match" => ("взаимно с ", "", "You and ", " liked each other"),
        "referral" => ("по твоей ссылке зарегистрировались — ещё ", " дней WIRING+", "Someone signed up with your link — another ", " days of WIRING+"),
        _ => return body.to_owned(),
    };
    let value = body.strip_prefix(ru_before).and_then(|v| v.strip_suffix(ru_after))
        .or_else(|| body.strip_prefix(en_before).and_then(|v| v.strip_suffix(en_after)));
    match value {
        Some(value) if !value.contains('\n') && (kind != "referral" || (!value.is_empty() && value.chars().all(char::is_numeric))) =>
            if english { format!("{en_before}{value}{en_after}") } else { format!("{ru_before}{value}{ru_after}") },
        _ => body.to_owned(),
    }
}
