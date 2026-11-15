use axum::{
    Json, http::StatusCode,
    response::{IntoResponse, Response},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use pbkdf2::sha2::{Digest, Sha256};
use serde_json::{Value, json};
use subtle::ConstantTimeEq;

use crate::{
    login::Context,
    mobile_http::{string_field, token_hash},
    mobile_session::{NewTokens, password_fingerprint},
    password::hash_password,
    signed_session::{AccessClaims, MOBILE_TTL},
};

enum Verified {
    Invalid,
    MissingAccount,
    Account { uid: i64, profile: Value, fingerprint: String, tokens: Option<NewTokens> },
}

pub(crate) async fn exchange_native(ctx: Context<'_>, fields: Value) -> Result<Response, StatusCode> {
    let lang = ctx.locale;
    let stale = || error(lang, "Sign-in expired. Please start again.", "Вход устарел. Начни заново.");
    if !fields.is_null() && !fields.is_object() {
        return Ok(error(lang, "Invalid request.", "Некорректный запрос."));
    }
    let code = string_field(&fields, "code");
    let verifier = string_field(&fields, "code_verifier");
    if !(43..=128).contains(&verifier.len()) || !verifier.bytes().all(|b| b.is_ascii_alphanumeric() || b"._~-".contains(&b))
        || code.is_empty() || code.len() > 256
    { return Ok(stale()); }
    let hash = token_hash(&code);
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let now = ctx.stamp;
    let profile = ctx.profile;
    let tokens = NewTokens::generate()?;
    let result = ctx.db.transaction(move |tx| Box::pin(async move {
        let row = tx.query_opt("SELECT user_id::bigint, challenge FROM oauth_native_codes
             WHERE code_hash = $1 AND created_at >= $2::bigint FOR UPDATE", &[&hash, &(now - 180)]).await?;
        let Some(row) = row else { return Ok(Verified::Invalid); };
        if !bool::from(challenge.as_bytes().ct_eq(row.get::<_, &str>(1).as_bytes())) {
            return Ok(Verified::Invalid);
        }
        let uid: i64 = row.get(0);
        let user = tx.query_opt(
            "SELECT password_hash FROM users WHERE id = $1::bigint AND COALESCE(deleted_at, 0) = 0 FOR UPDATE",
            &[&uid],
        ).await?;
        let Some(user) = user else { return Ok(Verified::MissingAccount); };
        let fingerprint = password_fingerprint(user.get(0));
        tx.execute("DELETE FROM oauth_native_codes WHERE code_hash = $1", &[&hash]).await?;
        tokens.save(tx, uid, &fingerprint, "native OAuth", now).await?;
        let (profile, _) = profile.read(tx, uid, now, lang).await?;
        Ok(Verified::Account { uid, profile, fingerprint, tokens: Some(tokens) })
    })).await?.0;
    Ok(match result {
        Verified::Invalid => stale(),
        Verified::MissingAccount => (StatusCode::UNAUTHORIZED, Json(json!({
            "ok": false, "error": if lang == "en" { "Account unavailable" } else { "аккаунт недоступен" },
        }))).into_response(),
        Verified::Account { uid, profile, fingerprint, tokens: Some(tokens) } => Json(json!({
            "ok": true, "user": profile, "refresh_token": tokens.refresh, "expires_in": MOBILE_TTL,
            "access_token": ctx.keys.sign_access(&AccessClaims { uid, pw: fingerprint }, now as u64),
        })).into_response(),
        Verified::Account { tokens: None, .. } => unreachable!(),
    })
}

pub(crate) async fn verify_email(ctx: Context<'_>, fields: Value) -> Result<(Response, Option<i64>), StatusCode> {
    let token = string_field(&fields, "token").trim().to_owned();
    let mobile = fields.get("mobile") == Some(&Value::Bool(true));
    let lang = ctx.locale;
    if token.is_empty() { return Ok((missing(lang), None)); }
    if token.contains('\0') || token.len() > 256 { return Ok((expired(lang), None)); }
    let stamp = ctx.stamp;
    let profile = ctx.profile;
    let tokens = if mobile { Some(NewTokens::generate()?) } else { None };
    let result = ctx.db.transaction(move |tx| Box::pin(async move {
        let row = tx.query_opt(
            "SELECT user_id::bigint FROM email_verifications
             WHERE token = $1 AND COALESCE(used_at, 0) = 0 AND created_at >= $2::bigint FOR UPDATE",
            &[&token, &(stamp - 172_800)],
        ).await?;
        let Some(row) = row else { return Ok(Verified::Invalid); };
        let uid: i64 = row.get(0);
        let user = tx.query_opt(
            "SELECT password_hash FROM users WHERE id = $1::bigint AND COALESCE(deleted_at, 0) = 0 FOR UPDATE",
            &[&uid],
        ).await?;
        let Some(user) = user else { return Ok(Verified::MissingAccount); };
        let fingerprint = password_fingerprint(user.get(0));
        tx.execute("UPDATE users SET email_verified_at = COALESCE(email_verified_at, $2::bigint), last_seen = $2::bigint WHERE id = $1::bigint",
            &[&uid, &stamp]).await?;
        tx.execute("UPDATE email_verifications SET used_at = $2::bigint WHERE token = $1", &[&token, &stamp]).await?;
        if let Some(tokens) = &tokens {
            tokens.save(tx, uid, &fingerprint, "email verification", stamp).await?;
        }
        let (profile, _) = profile.read(tx, uid, stamp, lang).await?;
        Ok(Verified::Account { uid, profile, fingerprint, tokens })
    })).await?.0;
    Ok(match result {
        Verified::Invalid => (expired(lang), None),
        Verified::MissingAccount => (error(lang, "Account not found", "аккаунт не найден"), None),
        Verified::Account { uid, profile, fingerprint, tokens } => {
            let mut payload = json!({"ok": true, "user": profile});
            if let Some(tokens) = tokens {
                payload["access_token"] = ctx.keys.sign_access(&AccessClaims { uid, pw: fingerprint }, stamp as u64).into();
                payload["refresh_token"] = tokens.refresh.into();
                payload["expires_in"] = MOBILE_TTL.into();
                (Json(payload).into_response(), None)
            } else {
                (Json(payload).into_response(), Some(uid))
            }
        }
    })
}

pub(crate) async fn reset_password(ctx: Context<'_>, fields: Value) -> Result<Response, StatusCode> {
    let password = string_field(&fields, "password");
    let token = string_field(&fields, "token").trim().to_owned();
    let lang = ctx.locale;
    if password.chars().count() < 6 {
        return Ok(error(lang, "Password must have at least 6 characters", "пароль минимум 6 символов"));
    }
    if token.is_empty() { return Ok(missing(lang)); }
    if token.len() > 256 || token.contains('\0') { return Ok(expired(lang)); }
    let stamp = ctx.stamp;
    let lookup = token.clone();
    let exists = ctx.db.transaction(move |tx| Box::pin(async move {
        Ok(tx.query_opt("SELECT 1 FROM password_resets
            WHERE token = $1 AND COALESCE(used_at, 0) = 0 AND created_at >= $2::bigint",
            &[&lookup, &(stamp - 7200)]).await?.is_some())
    })).await?.0;
    if !exists { return Ok(expired(lang)); }
    let hash = hash_password(password).await?;
    let saved = ctx.db.transaction(move |tx| Box::pin(async move {
        let row = tx.query_opt("SELECT user_id::bigint FROM password_resets
            WHERE token = $1 AND COALESCE(used_at, 0) = 0 AND created_at >= $2::bigint FOR UPDATE",
            &[&token, &(stamp - 7200)]).await?;
        let Some(row) = row else { return Ok(false); };
        let uid: i64 = row.get(0);
        if tx.execute("UPDATE users SET password_hash = $2 WHERE id = $1::bigint", &[&uid, &hash]).await? != 1 {
            return Ok(false);
        }
        tx.execute("UPDATE password_resets SET used_at = $2::bigint WHERE token = $1", &[&token, &stamp]).await?;
        Ok(true)
    })).await?.0;
    Ok(if saved { Json(json!({"ok": true})).into_response() } else { expired(lang) })
}

fn error(lang: &str, en: &str, ru: &str) -> Response {
    (StatusCode::BAD_REQUEST, Json(json!({"ok": false, "error": if lang == "en" { en } else { ru }}))).into_response()
}
fn missing(lang: &str) -> Response { error(lang, "Missing token", "нет токена") }
fn expired(lang: &str) -> Response {
    error(lang, "This link has expired or was already used", "ссылка устарела или уже использована")
}
