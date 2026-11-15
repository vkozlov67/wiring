use std::sync::Arc;

use axum::{Json, extract::State, http::{HeaderMap, StatusCode}};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use pbkdf2::sha2::{Digest, Sha256};
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;
use tokio_postgres::Transaction;

use crate::chat_write::{TransactionResult, Writer};

pub(crate) struct NewTokens {
    pub refresh: String,
    family: String,
}

impl NewTokens {
    pub(crate) fn generate() -> Result<Self, StatusCode> {
        let mut refresh = [0u8; 48];
        let mut family = [0u8; 12];
        getrandom::fill(&mut refresh).map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
        getrandom::fill(&mut family).map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
        Ok(Self {
            refresh: URL_SAFE_NO_PAD.encode(refresh),
            family: family.iter().map(|b| format!("{b:02x}")).collect(),
        })
    }

    pub(crate) async fn save(&self, tx: &Transaction<'_>, uid: i64, fingerprint: &str, device: &str, now: i64) -> TransactionResult<()> {
        tx.execute(
            "INSERT INTO mobile_refresh_tokens (token_hash, user_id, family, pw, device, created_at, expires_at)
             VALUES ($1, $2::bigint, $3, $4, $5, $6::bigint, $7::bigint)",
            &[&crate::mobile_http::token_hash(&self.refresh), &uid, &self.family, &fingerprint, &device,
              &now, &(now + crate::mobile_http::REFRESH_TTL)],
        ).await?;
        Ok(())
    }
}

#[derive(Deserialize)]
pub(crate) struct Rotation {
    old_hash: String,
    new_hash: String,
    now: i64,
    expires_at: i64,
}

#[derive(Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub(crate) enum RotationOutcome {
    Invalid,
    Rotated { user_id: i64, fingerprint: String },
}

#[derive(Deserialize)]
pub(crate) struct Logout {
    token_hash: String,
    push_token: String,
    now: i64,
}

#[derive(Serialize)]
pub(crate) struct LogoutOutcome {
    ok: bool,
}

#[derive(Deserialize)]
pub(crate) struct NewSession {
    uid: i64,
    token_hash: String,
    family: String,
    fingerprint: String,
    device: String,
    now: i64,
    expires_at: i64,
}

#[derive(Serialize)]
pub(crate) struct Issued {
    issued: bool,
}

pub(crate) async fn issue(
    State(state): State<Arc<Writer>>,
    headers: HeaderMap,
    Json(input): Json<NewSession>,
) -> Result<Json<Issued>, StatusCode> {
    state.authorize(&headers)?;
    if input.token_hash.len() != 64 || input.family.len() != 24 || input.fingerprint.len() != 16
        || input.device.chars().count() > 80 || input.expires_at <= input.now
    {
        return Err(StatusCode::BAD_REQUEST);
    }
    state.transaction(move |tx| Box::pin(async move {
        let user = tx.query_opt(
            "SELECT password_hash FROM users WHERE id = $1::bigint AND COALESCE(deleted_at, 0) = 0 FOR SHARE",
            &[&input.uid],
        ).await?;
        if !user.is_some_and(|row| password_fingerprint(row.get(0)) == input.fingerprint) {
            return Ok(Issued { issued: false });
        }
        tx.execute(
            "INSERT INTO mobile_refresh_tokens (token_hash, user_id, family, pw, device, created_at, expires_at)
             VALUES ($1, $2::bigint, $3, $4, $5, $6::bigint, $7::bigint)",
            &[&input.token_hash, &input.uid, &input.family, &input.fingerprint, &input.device, &input.now, &input.expires_at],
        ).await?;
        Ok(Issued { issued: true })
    })).await
}

pub(crate) async fn refresh(
    State(state): State<Arc<Writer>>,
    headers: HeaderMap,
    Json(input): Json<Rotation>,
) -> Result<Json<RotationOutcome>, StatusCode> {
    state.authorize(&headers)?;
    if input.old_hash.len() != 64 || input.new_hash.len() != 64 || input.expires_at <= input.now {
        return Err(StatusCode::BAD_REQUEST);
    }
    state.transaction(move |tx| Box::pin(async move { rotate(tx, input).await })).await
}

pub(crate) async fn rotate(tx: &Transaction<'_>, input: Rotation) -> TransactionResult<RotationOutcome> {
    let Some(row) = tx.query_opt(
        "SELECT family FROM mobile_refresh_tokens WHERE token_hash = $1", &[&input.old_hash],
    ).await? else {
        return Ok(RotationOutcome::Invalid);
    };
    let family: &str = row.get(0);
    lock_family(tx, family).await?;
    let Some(row) = tx.query_opt(
        "SELECT t.user_id::bigint, t.pw, t.device, t.expires_at::bigint,
            COALESCE(t.used_at, 0) != 0 OR COALESCE(t.revoked_at, 0) != 0,
            u.password_hash, COALESCE(u.deleted_at, 0) != 0
         FROM mobile_refresh_tokens t JOIN users u ON u.id = t.user_id WHERE t.token_hash = $1",
        &[&input.old_hash],
    ).await? else {
        return Ok(RotationOutcome::Invalid);
    };
    let fingerprint = password_fingerprint(row.get(5));
    let unusable = row.get::<_, bool>(4) || row.get::<_, bool>(6)
        || row.get::<_, i64>(3) < input.now || row.get::<_, &str>(1) != fingerprint;
    if unusable || tx.execute(
        "UPDATE mobile_refresh_tokens SET used_at = $2::bigint
         WHERE token_hash = $1 AND used_at IS NULL AND revoked_at IS NULL",
        &[&input.old_hash, &input.now],
    ).await? != 1 {
        revoke_family(tx, family, input.now).await?;
        return Ok(RotationOutcome::Invalid);
    }
    let user_id: i64 = row.get(0);
    let device: &str = row.get(2);
    tx.execute(
        "INSERT INTO mobile_refresh_tokens (token_hash, user_id, family, pw, device, created_at, expires_at)
         VALUES ($1, $2::bigint, $3, $4, $5, $6::bigint, $7::bigint)",
        &[&input.new_hash, &user_id, &family, &fingerprint, &device, &input.now, &input.expires_at],
    ).await?;
    Ok(RotationOutcome::Rotated { user_id, fingerprint })
}

pub(crate) async fn logout(
    State(state): State<Arc<Writer>>,
    headers: HeaderMap,
    Json(input): Json<Logout>,
) -> Result<Json<LogoutOutcome>, StatusCode> {
    state.authorize(&headers)?;
    if input.token_hash.len() != 64 {
        return Err(StatusCode::BAD_REQUEST);
    }
    state.transaction(move |tx| Box::pin(revoke(tx, input))).await
}

pub(crate) async fn revoke(tx: &Transaction<'_>, input: Logout) -> TransactionResult<LogoutOutcome> {
    if let Some(row) = tx.query_opt(
            "SELECT family, user_id::bigint FROM mobile_refresh_tokens WHERE token_hash = $1",
            &[&input.token_hash],
        ).await? {
            let family: &str = row.get(0);
            let uid: i64 = row.get(1);
            lock_family(tx, family).await?;
            revoke_family(tx, family, input.now).await?;
            if !input.push_token.is_empty() {
                tx.execute("DELETE FROM mobile_devices WHERE token = $1 AND user_id = $2::bigint",
                           &[&input.push_token, &uid]).await?;
            }
    }
    Ok(LogoutOutcome { ok: true })
}

pub(crate) fn rotation(old_hash: String, new_hash: String, now: i64, expires_at: i64) -> Rotation {
    Rotation { old_hash, new_hash, now, expires_at }
}

pub(crate) fn logout_request(token_hash: String, push_token: String, now: i64) -> Logout {
    Logout { token_hash, push_token, now }
}

async fn lock_family(tx: &Transaction<'_>, family: &str) -> Result<(), tokio_postgres::Error> {
    tx.query_one("SELECT pg_advisory_xact_lock(hashtextextended($1::text, 781065))", &[&family]).await?;
    Ok(())
}

async fn revoke_family(tx: &Transaction<'_>, family: &str, now: i64) -> Result<(), tokio_postgres::Error> {
    tx.execute(
        "UPDATE mobile_refresh_tokens SET revoked_at = $2::bigint WHERE family = $1 AND revoked_at IS NULL",
        &[&family, &now],
    ).await?;
    Ok(())
}

pub(crate) fn password_fingerprint(password_hash: &str) -> String {
    Sha256::digest(password_hash.as_bytes())[..8].iter().map(|byte| format!("{byte:02x}")).collect()
}

pub(crate) fn matches_access(claims: &crate::signed_session::AccessClaims, password_hash: &str, deleted: bool) -> bool {
    !deleted && bool::from(password_fingerprint(password_hash).as_bytes().ct_eq(claims.pw.as_bytes()))
}
