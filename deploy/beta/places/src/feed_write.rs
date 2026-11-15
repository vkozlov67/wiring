use std::sync::Arc;

use axum::{Json, extract::State, http::{HeaderMap, StatusCode}};
use serde::{Deserialize, Serialize};
use tokio_postgres::{Transaction, types::ToSql};

use crate::chat_write::{TransactionResult, Writer, lock_pair, pair_access};

#[derive(Deserialize)]
pub(crate) struct Swipe {
    pub(crate) uid: i64,
    pub(crate) target_id: i64,
    pub(crate) now: i64,
    pub(crate) direction: Direction,
    pub(crate) seed_direction: Option<Decision>,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Direction { Like, Pass, Snooze }

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Decision { Like, Pass }

#[derive(Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub(crate) enum SwipeResult {
    Applied { matched: bool, was_match: bool, liked: i64 },
    Snoozed { until: i64 },
    Blocked,
    NotFound,
    PremiumRequired,
    AlreadyMatched,
}

pub(crate) async fn swipe(
    State(state): State<Arc<Writer>>,
    headers: HeaderMap,
    Json(input): Json<Swipe>,
) -> Result<Json<SwipeResult>, StatusCode> {
    state.authorize(&headers)?;
    if input.uid == input.target_id {
        return Err(StatusCode::BAD_REQUEST);
    }
    state.transaction(move |tx| Box::pin(async move { apply_swipe(tx, input).await })).await
}

pub(crate) async fn apply_swipe(tx: &Transaction<'_>, input: Swipe) -> TransactionResult<SwipeResult> {
    let pair: [&(dyn ToSql + Sync); 2] = [&input.uid, &input.target_id];
    lock_pair(tx, &pair).await?;
    let (blocked, was_match) = pair_access(tx, &pair).await?;
    if blocked {
        return Ok(SwipeResult::Blocked);
    }
    let Some(target) = tx.query_opt(
        "SELECT COALESCE(is_seed, 0) != 0 FROM users WHERE id = $1::bigint AND COALESCE(deleted_at, 0) = 0",
        &[&input.target_id],
    ).await? else {
        return Ok(SwipeResult::NotFound);
    };
    let direction = match input.direction {
        Direction::Like => "like",
        Direction::Pass if was_match => return Ok(SwipeResult::AlreadyMatched),
        Direction::Pass => {
            tx.execute(
                "INSERT INTO feed_history (user_id, other_id, delivered_at, excluded_at)
                 VALUES ($1::bigint, $2::bigint, $3::bigint, $3)
                 ON CONFLICT (user_id, other_id) DO UPDATE SET excluded_at = COALESCE(feed_history.excluded_at, excluded.excluded_at)",
                &[&input.uid, &input.target_id, &input.now],
            ).await?;
            "pass"
        }
        Direction::Snooze => {
            let premium = tx.query_one(
                "SELECT COALESCE(premium_until, 0) > $2::bigint FROM users WHERE id = $1::bigint",
                &[&input.uid, &input.now],
            ).await?.get::<_, bool>(0);
            if !premium {
                return Ok(SwipeResult::PremiumRequired);
            }
            let until = input.now + 7 * 24 * 60 * 60;
            tx.execute(
                "INSERT INTO snoozes (user_id, other_id, until_at, created_at)
                 VALUES ($1::bigint, $2::bigint, $3::bigint, $4::bigint)
                 ON CONFLICT (user_id, other_id) DO UPDATE SET until_at = excluded.until_at, created_at = excluded.created_at",
                &[&input.uid, &input.target_id, &until, &input.now],
            ).await?;
            return Ok(SwipeResult::Snoozed { until });
        }
    };
    tx.execute(
        "INSERT INTO swipes (from_id, to_id, direction, created_at) VALUES ($1::bigint, $2::bigint, $3, $4::bigint)
         ON CONFLICT (from_id, to_id) DO UPDATE SET direction = excluded.direction, created_at = excluded.created_at",
        &[&input.uid, &input.target_id, &direction, &input.now],
    ).await?;
    let mut matched = false;
    if direction == "like" {
        let back = tx.query_opt("SELECT direction FROM swipes WHERE from_id = $2::bigint AND to_id = $1::bigint", &pair).await?;
        matched = back.as_ref().is_some_and(|row| row.get::<_, &str>(0) == "like");
        if back.is_none() && target.get::<_, bool>(0) && let Some(decision) = input.seed_direction {
            let response = match decision { Decision::Like => "like", Decision::Pass => "pass" };
            tx.execute(
                "INSERT INTO swipes (from_id, to_id, direction, created_at) VALUES ($2::bigint, $1::bigint, $3, $4::bigint)",
                &[&input.uid, &input.target_id, &response, &input.now],
            ).await?;
            matched = response == "like";
        }
    }
    let liked = tx.query_one(
        "SELECT count(*) FROM swipes WHERE from_id = $1::bigint AND direction = 'like'", &[&input.uid],
    ).await?.get(0);
    Ok(SwipeResult::Applied { matched, was_match, liked })
}

#[derive(Deserialize)]
pub(crate) struct FeedUpdate {
    pub(crate) uid: i64,
    pub(crate) now: i64,
    #[serde(flatten)]
    pub(crate) action: FeedAction,
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub(crate) enum FeedAction {
    View { target_id: i64 },
    Reset { generation: i64, qa: bool },
}

#[derive(Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub(crate) enum FeedResult {
    Viewed,
    NotDelivered,
    Reset { reset: bool, generation: i64 },
    PremiumRequired,
}

#[derive(Deserialize)]
pub(crate) struct FeedClaim {
    pub(crate) uid: i64,
    pub(crate) now: i64,
    pub(crate) limit: i64,
    pub(crate) include_delivered: bool,
    #[serde(default)]
    pub(crate) recover_unviewed: bool,
    pub(crate) candidates: Vec<i64>,
}

#[derive(Serialize)]
pub(crate) struct FeedPage {
    pub(crate) ids: Vec<i64>,
    pub(crate) has_more: bool,
    pub(crate) generation: i64,
}

pub(crate) async fn claim(
    State(state): State<Arc<Writer>>,
    headers: HeaderMap,
    Json(input): Json<FeedClaim>,
) -> Result<Json<FeedPage>, StatusCode> {
    state.authorize(&headers)?;
    if !(1..=30).contains(&input.limit) {
        return Err(StatusCode::BAD_REQUEST);
    }
    state.transaction(move |tx| Box::pin(async move { claim_delivery(tx, input).await })).await
}

pub(crate) async fn claim_delivery(tx: &Transaction<'_>, input: FeedClaim) -> TransactionResult<FeedPage> {
    let viewer = tx.query_one(
        "SELECT feed_generation::bigint, COALESCE(premium_until, 0) > $2::bigint
         FROM users WHERE id = $1::bigint FOR UPDATE",
        &[&input.uid, &input.now],
    ).await?;
    let premium: bool = viewer.get(1);
    let rows = tx.query(
        "SELECT u.id::bigint, h.other_id IS NOT NULL AS delivered
         FROM users u
         LEFT JOIN feed_history h ON h.user_id = $1::bigint AND h.other_id = u.id
         WHERE u.id = ANY($2::bigint[]) AND u.id != $1 AND COALESCE(u.deleted_at, 0) = 0
           AND COALESCE(u.paused, 0) = 0 AND COALESCE(u.report_banned_at, 0) = 0
           AND COALESCE(u.is_seed, 0) = 0 AND h.excluded_at IS NULL
           AND ($3 OR h.other_id IS NULL OR ($6 AND h.viewed_at IS NULL))
           AND NOT EXISTS (SELECT 1 FROM swipes s WHERE s.from_id = $1 AND s.to_id = u.id)
           AND NOT EXISTS (SELECT 1 FROM blocks b WHERE (b.from_id = $1 AND b.to_id = u.id)
                OR (b.from_id = u.id AND b.to_id = $1))
           AND (COALESCE(u.incognito, 0) = 0 OR EXISTS (SELECT 1 FROM swipes s
                WHERE s.from_id = u.id AND s.to_id = $1 AND s.direction = 'like'))
           AND (NOT $4 OR NOT EXISTS (SELECT 1 FROM snoozes s
                WHERE s.user_id = $1 AND s.other_id = u.id AND s.until_at > $5::bigint))
         ORDER BY delivered, h.viewed_at IS NOT NULL, random()",
        &[&input.uid, &input.candidates, &input.include_delivered, &premium, &input.now, &input.recover_unviewed],
    ).await?;
    let mut ids = Vec::with_capacity(input.limit as usize);
    let mut has_more = false;
    for row in rows {
        if ids.len() == input.limit as usize {
            has_more = true;
            break;
        }
        let id: i64 = row.get(0);
        if !row.get::<_, bool>(1) {
            let inserted = tx.execute(
                "INSERT INTO feed_history (user_id, other_id, delivered_at)
                 VALUES ($1::bigint, $2::bigint, $3::bigint) ON CONFLICT (user_id, other_id) DO NOTHING",
                &[&input.uid, &id, &input.now],
            ).await?;
            if inserted == 0 {
                continue;
            }
        }
        ids.push(id);
    }
    Ok(FeedPage { ids, has_more, generation: viewer.get(0) })
}

pub(crate) async fn update(
    State(state): State<Arc<Writer>>,
    headers: HeaderMap,
    Json(input): Json<FeedUpdate>,
) -> Result<Json<FeedResult>, StatusCode> {
    state.authorize(&headers)?;
    state.transaction(move |tx| Box::pin(async move { update_delivery(tx, input).await })).await
}

pub(crate) async fn update_delivery(tx: &Transaction<'_>, input: FeedUpdate) -> TransactionResult<FeedResult> {
    match input.action {
        FeedAction::View { target_id } => {
            let viewed = tx.execute(
                "UPDATE feed_history SET viewed_at = COALESCE(viewed_at, $3::bigint)
                 WHERE user_id = $1::bigint AND other_id = $2::bigint",
                &[&input.uid, &target_id, &input.now],
            ).await?;
            Ok(if viewed > 0 { FeedResult::Viewed } else { FeedResult::NotDelivered })
        }
        FeedAction::Reset { generation, qa } => {
            let Some(viewer) = tx.query_opt(
                "SELECT feed_generation::bigint, COALESCE(premium_until, 0)::bigint FROM users
                 WHERE id = $1::bigint AND COALESCE(deleted_at, 0) = 0 FOR UPDATE",
                &[&input.uid],
            ).await? else {
                return Ok(FeedResult::PremiumRequired);
            };
            let current: i64 = viewer.get(0);
            let premium_until: i64 = viewer.get(1);
            if !qa && premium_until <= input.now {
                return Ok(FeedResult::PremiumRequired);
            }
            if current != generation {
                return Ok(FeedResult::Reset { reset: false, generation: current });
            }
            tx.execute(
                "DELETE FROM feed_history WHERE user_id = $1::bigint AND excluded_at IS NULL",
                &[&input.uid],
            ).await?;
            tx.execute(
                "UPDATE users SET feed_generation = feed_generation + 1 WHERE id = $1::bigint",
                &[&input.uid],
            ).await?;
            Ok(FeedResult::Reset { reset: true, generation: current + 1 })
        }
    }
}
