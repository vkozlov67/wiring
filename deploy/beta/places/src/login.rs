use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::{Value, json};
use subtle::ConstantTimeEq;

use crate::{
    chat_write::Writer,
    mobile_http::{REFRESH_TTL, string_field, token_hash},
    mobile_session::password_fingerprint,
    password::{Verification, verify_account},
    profile_read::ProfileRules,
    signed_session::{AccessClaims, MOBILE_TTL, SessionKeys},
};

const LOGIN_WINDOW: Duration = Duration::from_secs(300);
const MAX_CLIENTS: usize = 32_768;
const RECOVERY_SECONDS: i64 = 7 * 24 * 60 * 60;
const DUMMY_HASH: &str = "pbkdf2:sha256:1000000$unusedWiringLogin$0000000000000000000000000000000000000000000000000000000000000000";

pub(crate) struct LoginLimit {
    buckets: Mutex<(Instant, HashMap<String, VecDeque<Instant>>)>,
    window: Duration,
    max: usize,
}

impl LoginLimit {
    pub(crate) fn new() -> Self {
        Self::configured(LOGIN_WINDOW, 20)
    }

    pub(crate) fn configured(window: Duration, max: usize) -> Self {
        Self { buckets: Mutex::new((Instant::now(), HashMap::new())), window, max }
    }

    pub(crate) fn allow(&self, ip: &str, now: Instant) -> bool {
        let Ok(mut buckets) = self.buckets.lock() else { return false; };
        if now.saturating_duration_since(buckets.0) >= Duration::from_secs(15) {
            buckets.1.retain(|_, entries| entries.back().is_some_and(|stamp| now.saturating_duration_since(*stamp) < self.window));
            buckets.0 = now;
        }
        if buckets.1.len() >= MAX_CLIENTS && !buckets.1.contains_key(ip) { return false; }
        let attempts = buckets.1.entry(ip.to_owned()).or_default();
        while attempts.front().is_some_and(|stamp| now.saturating_duration_since(*stamp) >= self.window) {
            attempts.pop_front();
        }
        if attempts.len() >= self.max { return false; }
        attempts.push_back(now);
        true
    }
}

pub(crate) struct Context<'a> {
    pub db: &'a Writer,
    pub profile: Arc<ProfileRules>,
    pub keys: &'a SessionKeys,
    pub stamp: i64,
    pub locale: &'static str,
}

enum Outcome {
    Invalid,
    Expired,
    Unverified,
    Valid { uid: i64, user: Value, restored: bool, fingerprint: String },
}

pub(crate) async fn authenticate(
    context: Context<'_>, fields: Value, mobile: bool,
) -> Result<(Response, Option<i64>), StatusCode> {
    let email = string_field(&fields, "email").trim().to_lowercase();
    let password = string_field(&fields, "password");
    let lookup_email = email.clone();
    let found = if lookup_email.contains('\0') {
        None
    } else {
        context.db.transaction(move |tx| Box::pin(async move {
            let row = tx.query_opt("SELECT id::bigint, password_hash FROM users WHERE email = $1", &[&lookup_email]).await?;
            Ok(row.map(|row| (row.get::<_, i64>(0), row.get::<_, String>(1))))
        })).await?.0
    };
    let hash = found.as_ref().map(|(_, hash)| hash.as_str()).unwrap_or(DUMMY_HASH);
    let valid = matches!(verify_account(hash.to_owned(), password).await?, Verification::Valid);
    let Some((uid, hash)) = found.filter(|_| valid) else { return Ok((invalid(context.locale), None)); };

    let now = context.stamp;
    let lang = context.locale;
    let profile = context.profile;
    let device: String = string_field(&fields, "device").chars().filter(|c| *c != '\0').take(80).collect();
    let (refresh, family) = if mobile {
        let mut refresh = [0u8; 48];
        let mut family = [0u8; 12];
        getrandom::fill(&mut refresh).map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
        getrandom::fill(&mut family).map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
        (URL_SAFE_NO_PAD.encode(refresh), family.iter().map(|b| format!("{b:02x}")).collect())
    } else { (String::new(), String::new()) };
    let refresh_hash = token_hash(&refresh);
    let expected_email = email.clone();
    let outcome = context.db.transaction(move |tx| Box::pin(async move {
        let row = tx.query_opt(
            "SELECT password_hash, email, COALESCE(deleted_at, 0)::bigint, COALESCE(email_verified_at, 0) != 0
             FROM users WHERE id = $1::bigint FOR UPDATE", &[&uid],
        ).await?;
        let Some(row) = row else { return Ok(Outcome::Invalid); };
        let existing: &str = row.get(0);
        if !bool::from(existing.as_bytes().ct_eq(hash.as_bytes())) || row.get::<_, &str>(1) != expected_email {
            return Ok(Outcome::Invalid);
        }
        let deleted_at: i64 = row.get(2);
        if deleted_at != 0 && now.saturating_sub(deleted_at) > RECOVERY_SECONDS { return Ok(Outcome::Expired); }
        if deleted_at != 0 {
            tx.execute("UPDATE users SET deleted_at = NULL WHERE id = $1::bigint", &[&uid]).await?;
        }
        if !row.get::<_, bool>(3) && !expected_email.ends_with("@wiring.guest")
            && !expected_email.ends_with("@wiring.demo") && expected_email != "demo@wiring.app"
        {
            return Ok(Outcome::Unverified);
        }
        let fingerprint = password_fingerprint(existing);
        if mobile {
            tx.execute(
                "INSERT INTO mobile_refresh_tokens (token_hash, user_id, family, pw, device, created_at, expires_at)
                 VALUES ($1, $2::bigint, $3, $4, $5, $6::bigint, $7::bigint)",
                &[&refresh_hash, &uid, &family, &fingerprint, &device, &now, &(now + REFRESH_TTL)],
            ).await?;
        } else {
            tx.execute("UPDATE users SET last_seen = $2::bigint WHERE id = $1::bigint", &[&uid, &now]).await?;
        }
        let (user, _) = profile.read(tx, uid, now, lang).await?;
        Ok(Outcome::Valid { uid, user, restored: deleted_at != 0, fingerprint })
    })).await?.0;
    let result = match outcome {
        Outcome::Invalid => (invalid(lang), None),
        Outcome::Expired => (error(StatusCode::FORBIDDEN, lang, "The account was deleted and its recovery period has ended",
            "аккаунт удалён, срок восстановления прошёл", json!({"deleted": true})), None),
        Outcome::Unverified => (error(StatusCode::FORBIDDEN, lang, "Verify your email — we sent you a link",
            "подтверди почту — мы отправили ссылку", json!({"needs_email_verify": true, "email": email})), None),
        Outcome::Valid { uid, user, restored, fingerprint } => {
            let mut payload = json!({"ok": true, "user": user});
            if restored { payload["restored"] = true.into(); }
            if mobile {
                payload["access_token"] = context.keys.sign_access(&AccessClaims { uid, pw: fingerprint }, now as u64).into();
                payload["refresh_token"] = refresh.into();
                payload["expires_in"] = MOBILE_TTL.into();
            }
            (Json(payload).into_response(), (!mobile).then_some(uid))
        }
    };
    Ok(result)
}

fn invalid(lang: &str) -> Response {
    error(StatusCode::UNAUTHORIZED, lang, "Incorrect email or password", "неверная почта или пароль", json!({}))
}

pub(crate) fn limited(lang: &str) -> Response {
    error(StatusCode::TOO_MANY_REQUESTS, lang, "Too many attempts", "слишком много попыток", json!({}))
}

fn error(code: StatusCode, lang: &str, english: &str, russian: &str, mut data: Value) -> Response {
    data["ok"] = false.into();
    data["error"] = if lang == "en" { english } else { russian }.into();
    (code, Json(data)).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_endpoints_share_twenty_attempts_per_client() {
        let limit = LoginLimit::new();
        let now = Instant::now();
        for _ in 0..20 { assert!(limit.allow("192.0.2.1", now)); }
        assert!(!limit.allow("192.0.2.1", now + LOGIN_WINDOW - Duration::from_secs(1)));
        assert!(limit.allow("192.0.2.2", now));
        assert!(limit.allow("192.0.2.1", now + LOGIN_WINDOW));
    }
}
