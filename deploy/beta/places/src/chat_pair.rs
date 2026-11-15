use std::sync::Arc;

use axum::{Json, extract::State, http::{HeaderMap, StatusCode}};
use serde::{Deserialize, Serialize};
use tokio_postgres::{Transaction, types::ToSql};

use crate::chat_write::{TransactionResult, Writer, lock_pair, pair_access};

#[derive(Deserialize)]
pub(crate) struct PairChange {
    uid: i64,
    other_id: i64,
    now: i64,
    #[serde(flatten)]
    action: PairAction,
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
enum PairAction {
    Unmatch,
    Block,
    Report { reason: String, details: String },
}

#[derive(Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub(crate) enum PairOutcome {
    Hidden,
    Reported { id: i64, newly_banned: bool },
    NoMatch,
    NotFound,
}

pub(crate) async fn change(
    State(state): State<Arc<Writer>>,
    headers: HeaderMap,
    Json(input): Json<PairChange>,
) -> Result<Json<PairOutcome>, StatusCode> {
    state.authorize(&headers)?;
    if input.uid == input.other_id && !matches!(input.action, PairAction::Unmatch) {
        return Err(StatusCode::BAD_REQUEST);
    }
    state.transaction(move |tx| Box::pin(async move { change_pair(tx, input).await })).await
}

async fn change_pair(tx: &Transaction<'_>, input: PairChange) -> TransactionResult<PairOutcome> {
    let pair: [&(dyn ToSql + Sync); 2] = [&input.uid, &input.other_id];
    let timed_pair: [&(dyn ToSql + Sync); 3] = [&input.uid, &input.other_id, &input.now];
    lock_pair(tx, &pair).await?;
    if matches!(input.action, PairAction::Unmatch) {
        if !pair_access(tx, &pair).await?.1 {
            return Ok(PairOutcome::NoMatch);
        }
    } else {
        if tx.query_opt("SELECT id FROM users WHERE id = $1::bigint", &[&input.other_id]).await?.is_none() {
            return Ok(PairOutcome::NotFound);
        }
        tx.execute(
            "INSERT INTO blocks (from_id, to_id, created_at) VALUES ($1::bigint, $2::bigint, $3::bigint)
             ON CONFLICT (from_id, to_id) DO NOTHING",
            &timed_pair,
        ).await?;
    }

    tx.execute(
        "UPDATE messages SET deleted_at = $3::bigint
         WHERE ((from_id = $1::bigint AND to_id = $2::bigint) OR (from_id = $2 AND to_id = $1))
           AND deleted_at IS NULL",
        &timed_pair,
    ).await?;
    tx.execute(
        "UPDATE reads SET deleted_at = $3::bigint
         WHERE (user_id = $1::bigint AND other_id = $2::bigint) OR (user_id = $2 AND other_id = $1)",
        &timed_pair,
    ).await?;
    tx.execute(
        "INSERT INTO swipes (from_id, to_id, direction, created_at)
         VALUES ($1::bigint, $2::bigint, 'pass', $3::bigint)
         ON CONFLICT (from_id, to_id) DO UPDATE SET direction = 'pass', created_at = excluded.created_at",
        &timed_pair,
    ).await?;
    tx.execute("DELETE FROM swipes WHERE from_id = $2::bigint AND to_id = $1::bigint", &pair).await?;
    tx.execute(
        "DELETE FROM chat_openers WHERE (user_id = $1::bigint AND other_id = $2::bigint)
            OR (user_id = $2 AND other_id = $1)",
        &pair,
    ).await?;

    if let PairAction::Report { reason, details } = input.action {
        // Different reporters use different pair locks. Serialize the ban decision on the target.
        tx.query_one("SELECT id FROM users WHERE id = $1::bigint FOR NO KEY UPDATE", &[&input.other_id]).await?;
        let row = tx.query_one(
            "INSERT INTO reports (from_id, to_id, reason, details, created_at)
             VALUES ($1::bigint, $2::bigint, $3, $4, $5::bigint) RETURNING id::bigint",
            &[&input.uid, &input.other_id, &reason, &details, &input.now],
        ).await?;
        let newly_banned = tx.execute(
            "UPDATE users SET report_banned_at = $2::bigint
             WHERE id = $1::bigint AND report_banned_at IS NULL AND COALESCE(is_seed, 0) = 0
               AND (SELECT count(DISTINCT from_id) FROM reports WHERE to_id = $1) >= 2",
            &[&input.other_id, &input.now],
        ).await? > 0;
        return Ok(PairOutcome::Reported { id: row.get(0), newly_banned });
    }
    Ok(PairOutcome::Hidden)
}

pub(crate) async fn hide_pair(tx: &Transaction<'_>, uid: i64, other_id: i64, now: i64, block: bool) -> TransactionResult<PairOutcome> {
    let action = if block { PairAction::Block } else { PairAction::Unmatch };
    change_pair(tx, PairChange { uid, other_id, now, action }).await
}

pub(crate) async fn report_pair(tx: &Transaction<'_>, uid: i64, other_id: i64, now: i64, reason: String, details: String) -> TransactionResult<PairOutcome> {
    change_pair(tx, PairChange { uid, other_id, now, action: PairAction::Report { reason, details } }).await
}
