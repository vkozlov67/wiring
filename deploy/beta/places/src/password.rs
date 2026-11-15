use std::sync::Arc;

use axum::{Json, extract::State, http::{HeaderMap, StatusCode}};
use pbkdf2::{pbkdf2_hmac_array, sha2::Sha256};
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;

use crate::chat_write::Writer;

static PASSWORD_SLOTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);
const CURRENT_ROUNDS: u32 = 1_000_000;

pub(crate) async fn hash_password(password: String) -> Result<String, StatusCode> {
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    let mut seed = [0u8; 12];
    getrandom::fill(&mut seed).map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    let salt = URL_SAFE_NO_PAD.encode(seed);
    let slot = PASSWORD_SLOTS.try_acquire().map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    tokio::task::spawn_blocking(move || {
        let _slot = slot;
        let derived = pbkdf2_hmac_array::<Sha256, 32>(password.as_bytes(), salt.as_bytes(), CURRENT_ROUNDS);
        let hex: String = derived.iter().map(|byte| format!("{byte:02x}")).collect();
        format!("pbkdf2:sha256:{CURRENT_ROUNDS}${salt}${hex}")
    }).await.map_err(|_| StatusCode::SERVICE_UNAVAILABLE)
}

#[derive(Deserialize)]
pub(crate) struct PasswordRequest {
    hash: String,
    password: String,
}

#[derive(Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub(crate) enum Verification {
    Valid,
    Invalid,
    Unsupported,
}

pub(crate) async fn verify(
    State(state): State<Arc<Writer>>,
    headers: HeaderMap,
    Json(input): Json<PasswordRequest>,
) -> Result<Json<Verification>, StatusCode> {
    state.authorize(&headers)?;
    verify_account(input.hash, input.password).await.map(Json)
}

pub(crate) async fn verify_account(hash: String, password: String) -> Result<Verification, StatusCode> {
    let slot = PASSWORD_SLOTS.try_acquire().map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    tokio::task::spawn_blocking(move || {
        let _slot = slot;
        verify_werkzeug(&hash, &password)
    }).await.map_err(|_| StatusCode::SERVICE_UNAVAILABLE)
}

fn verify_werkzeug(hash: &str, password: &str) -> Verification {
    let mut fields = hash.split('$');
    let (Some(method), Some(salt), Some(expected), None) =
        (fields.next(), fields.next(), fields.next(), fields.next()) else {
        return Verification::Unsupported;
    };
    let Some(rounds) = method.strip_prefix("pbkdf2:sha256:").and_then(|count| count.parse::<u32>().ok()) else {
        return Verification::Unsupported;
    };
    if rounds == 0 || rounds > 2_000_000 || expected.len() != 64 {
        return Verification::Unsupported;
    }
    let derived = pbkdf2_hmac_array::<Sha256, 32>(password.as_bytes(), salt.as_bytes(), rounds);
    let hex: String = derived.iter().map(|byte| format!("{byte:02x}")).collect();
    if bool::from(hex.as_bytes().ct_eq(expected.as_bytes())) {
        Verification::Valid
    } else {
        Verification::Invalid
    }
}
