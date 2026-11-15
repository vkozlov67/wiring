use std::{collections::HashMap, sync::Arc};

use axum::{Json, extract::State, http::{HeaderMap, StatusCode}};
use serde::{Deserialize, Serialize};
use tokio_postgres::{Row, Transaction, types::ToSql};

use crate::{
    chat_write::{REACTIONS, Reaction, TransactionResult, Writer, lock_pair, mark_read, pair_access},
    social::{Message, ReplySource, ThreadMessage, ThreadRequest, render_thread},
};

#[derive(Deserialize)]
pub(crate) struct HistoryRequest {
    pub(crate) uid: i64,
    pub(crate) other_id: i64,
    pub(crate) after_id: i64,
    pub(crate) before_id: i64,
    pub(crate) limit: i64,
    pub(crate) base_path: String,
    pub(crate) deleted_label: String,
    pub(crate) voice_label: String,
    pub(crate) photo_label: String,
}

#[derive(Serialize)]
pub(crate) struct HistoryMessage {
    #[serde(flatten)]
    message: ThreadMessage,
    reactions: Vec<Reaction>,
}

#[derive(Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub(crate) enum History {
    Available { messages: Vec<HistoryMessage>, has_more: bool },
    Blocked,
    NoMatch,
}

pub(crate) async fn history(
    State(state): State<Arc<Writer>>,
    headers: HeaderMap,
    Json(input): Json<HistoryRequest>,
) -> Result<Json<History>, StatusCode> {
    state.authorize(&headers)?;
    if input.after_id < 0 || input.before_id < 0 || !(0..=200).contains(&input.limit) {
        return Err(StatusCode::BAD_REQUEST);
    }
    state.transaction(move |tx| Box::pin(async move { fetch_history(tx, input).await })).await
}

pub(crate) async fn fetch_history(tx: &Transaction<'_>, input: HistoryRequest) -> TransactionResult<History> {
    let pair: [&(dyn ToSql + Sync); 2] = [&input.uid, &input.other_id];
    lock_pair(tx, &pair).await?;
    let (blocked, matched) = pair_access(tx, &pair).await?;
    if blocked {
        return Ok(History::Blocked);
    }
    if !matched {
        return Ok(History::NoMatch);
    }
    let descending = input.limit > 0 && input.after_id == 0;
    let order = if descending { "DESC" } else { "ASC" };
    let limit = (input.limit > 0).then_some(input.limit + 1);
    let mut rows = tx.query(
        &format!("SELECT id::bigint, from_id::bigint, COALESCE(body, ''), COALESCE(photo, ''),
                COALESCE(extra_photos, '[]'), COALESCE(audio, ''), COALESCE(audio_duration, 0)::bigint,
                COALESCE(edited_at, 0) != 0, created_at::bigint, reply_to_id::bigint
            FROM messages WHERE ((from_id = $1::bigint AND to_id = $2::bigint) OR (from_id = $2 AND to_id = $1))
                AND COALESCE(deleted_at, 0) = 0 AND id > $3::bigint AND ($4::bigint = 0 OR id < $4)
            ORDER BY id {order} LIMIT $5::bigint"),
        &[&input.uid, &input.other_id, &input.after_id, &input.before_id, &limit],
    ).await?;
    let has_more = limit.is_some_and(|limit| rows.len() == limit as usize);
    if has_more {
        rows.pop();
    }
    if descending {
        rows.reverse();
    }
    let messages = rows.iter().map(message).collect::<Result<Vec<_>, _>>()?;
    let replies = replies(tx, &input, &messages).await?;
    let mut reactions = reactions(tx, input.uid, &messages).await?;
    let peer_read_id: i64 = tx.query_opt(
        "SELECT last_read_id::bigint FROM reads WHERE user_id = $2::bigint AND other_id = $1::bigint
            AND COALESCE(deleted_at, 0) = 0",
        &pair,
    ).await?.map_or(0, |row| row.get(0));
    mark_read(tx, &pair).await?;
    tx.execute(
        "UPDATE notifications SET \"read\" = 1 WHERE user_id = $1::bigint AND from_id = $2::bigint
            AND kind IN ('message', 'match') AND \"read\" = 0",
        &pair,
    ).await?;
    let messages = render_thread(ThreadRequest {
        uid: input.uid, peer_read_id, base_path: input.base_path, messages, replies,
    }).into_iter().map(|message| HistoryMessage {
        reactions: reactions.remove(&message.id).unwrap_or_default(), message,
    }).collect();
    Ok(History::Available { messages, has_more })
}

fn message(row: &Row) -> Result<Message, serde_json::Error> {
    let photo: String = row.get(3);
    let extra: &str = row.get(4);
    let extra_count = if extra.is_empty() { 0 } else { serde_json::from_str::<Vec<String>>(extra)?.len() };
    Ok(Message {
        id: row.get(0), from_id: row.get(1), body: row.get(2),
        photo_count: usize::from(!photo.is_empty()) + extra_count, photo,
        audio: !row.get::<_, &str>(5).is_empty(), audio_duration: row.get(6),
        edited: row.get(7), created_at: row.get(8), reply_to_id: row.get(9),
    })
}

async fn replies(tx: &Transaction<'_>, input: &HistoryRequest, messages: &[Message]) -> TransactionResult<HashMap<i64, ReplySource>> {
    let ids: Vec<_> = messages.iter().filter_map(|message| message.reply_to_id).collect();
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let rows = tx.query(
        "SELECT id::bigint, from_id::bigint, COALESCE(deleted_at, 0) != 0,
            COALESCE(photo, ''), COALESCE(audio, ''), COALESCE(body, ''), COALESCE(transcript, '')
         FROM messages WHERE id = ANY($3::bigint[])
           AND ((from_id = $1::bigint AND to_id = $2::bigint) OR (from_id = $2 AND to_id = $1))",
        &[&input.uid, &input.other_id, &ids],
    ).await?;
    Ok(rows.into_iter().map(|row| {
        let gone: bool = row.get(2);
        let photo: &str = row.get(3);
        let audio: &str = row.get(4);
        let body = row.get::<_, &str>(5).trim();
        let transcript = row.get::<_, &str>(6).trim();
        let (text, placeholder) = if gone {
            ("", input.deleted_label.as_str())
        } else if !audio.trim().is_empty() {
            ("", input.voice_label.as_str())
        } else {
            (if body.is_empty() { transcript } else { body }, if photo.trim().is_empty() { "" } else { &input.photo_label })
        };
        (row.get(0), ReplySource {
            from_id: row.get(1), gone, photo: !photo.is_empty(), audio: !audio.is_empty(),
            text: text.to_owned(), placeholder: placeholder.to_owned(),
        })
    }).collect())
}

async fn reactions(tx: &Transaction<'_>, uid: i64, messages: &[Message]) -> TransactionResult<HashMap<i64, Vec<Reaction>>> {
    if messages.is_empty() {
        return Ok(HashMap::new());
    }
    let ids: Vec<_> = messages.iter().map(|message| message.id).collect();
    let rows = tx.query(
        "SELECT message_id, emoji, count(*), bool_or(user_id = $2)
         FROM message_reactions WHERE message_id = ANY($1::bigint[]) GROUP BY message_id, emoji",
        &[&ids, &uid],
    ).await?;
    let mut result: HashMap<i64, Vec<Reaction>> = HashMap::new();
    for row in rows {
        let name = row.get::<_, &str>(1);
        if let Some(emoji) = REACTIONS.into_iter().find(|emoji| *emoji == name) {
            result.entry(row.get(0)).or_default().push(Reaction { emoji, count: row.get(2), mine: row.get(3) });
        }
    }
    for reactions in result.values_mut() {
        reactions.sort_by_key(|reaction| REACTIONS.iter().position(|emoji| *emoji == reaction.emoji));
    }
    Ok(result)
}

#[derive(Deserialize)]
pub(crate) struct InboxRequest {
    uid: i64,
}

#[derive(Serialize)]
pub(crate) struct InboxCounts {
    pub likes_in: i64,
    pub unread: i64,
    pub unread_chats: i64,
}

pub(crate) async fn inbox(
    State(state): State<Arc<Writer>>,
    headers: HeaderMap,
    Json(input): Json<InboxRequest>,
) -> Result<Json<InboxCounts>, StatusCode> {
    state.authorize(&headers)?;
    state.transaction(move |tx| Box::pin(inbox_counts(tx, input.uid))).await
}

pub(crate) async fn inbox_counts(tx: &Transaction<'_>, uid: i64) -> TransactionResult<InboxCounts> {
        let counts = tx.query_one(
            "WITH visible_likes AS (
                SELECT u.id, u.email, u.is_seed, u.photo, outgoing.direction
                FROM swipes incoming JOIN users u ON u.id = incoming.from_id
                LEFT JOIN swipes outgoing ON outgoing.from_id = $1::bigint AND outgoing.to_id = u.id
                WHERE incoming.to_id = $1 AND incoming.direction = 'like' AND COALESCE(u.deleted_at, 0) = 0
                  AND NOT EXISTS (SELECT 1 FROM blocks b WHERE (b.from_id = $1 AND b.to_id = u.id)
                      OR (b.to_id = $1 AND b.from_id = u.id))
            ), unread_by_chat AS (
                SELECT count(*) AS unread FROM visible_likes peer
                LEFT JOIN reads r ON r.user_id = $1 AND r.other_id = peer.id AND COALESCE(r.deleted_at, 0) = 0
                JOIN messages m ON m.from_id = peer.id AND m.to_id = $1 AND m.id > COALESCE(r.last_read_id, 0)
                WHERE peer.direction = 'like' AND COALESCE(m.deleted_at, 0) = 0
                GROUP BY peer.id
            )
            SELECT (SELECT count(*) FROM visible_likes peer WHERE peer.direction IS NULL
                AND COALESCE(peer.is_seed, 0) = 0 AND COALESCE(peer.email, '') NOT LIKE '%@wiring.guest'
                AND COALESCE(peer.email, '') != 'demo@wiring.app'
                AND (COALESCE(peer.photo, '') != '' OR EXISTS (SELECT 1 FROM photos WHERE user_id = peer.id))),
                COALESCE(sum(unread), 0)::bigint, count(*) FROM unread_by_chat",
            &[&uid],
        ).await?;
        Ok(InboxCounts { likes_in: counts.get(0), unread: counts.get(1), unread_chats: counts.get(2) })
}

#[derive(Deserialize)]
pub(crate) struct MatchesRequest {
    pub(crate) uid: i64,
    pub(crate) voice_label: String,
    pub(crate) photo_label: String,
}

#[derive(Serialize)]
pub(crate) struct MatchSummary {
    pub(crate) id: i64,
    last_message: String,
    last_at: i64,
    last_from_id: i64,
    matched_at: i64,
    unread: i64,
}

pub(crate) async fn matches(
    State(state): State<Arc<Writer>>,
    headers: HeaderMap,
    Json(input): Json<MatchesRequest>,
) -> Result<Json<Vec<MatchSummary>>, StatusCode> {
    state.authorize(&headers)?;
    state.transaction(move |tx| Box::pin(fetch_matches(tx, input))).await
}

pub(crate) async fn fetch_matches(tx: &Transaction<'_>, input: MatchesRequest) -> TransactionResult<Vec<MatchSummary>> {
        let rows = tx.query(
            "SELECT u.id::bigint, greatest(sent.created_at, received.created_at)::bigint AS matched_at,
                    COALESCE(last.body, ''), COALESCE(last.photo, ''), COALESCE(last.audio, ''),
                    COALESCE(last.created_at, 0)::bigint AS last_at, COALESCE(last.from_id, 0)::bigint,
                    unread.count
             FROM users u
             JOIN swipes sent ON sent.to_id = u.id AND sent.from_id = $1::bigint AND sent.direction = 'like'
             JOIN swipes received ON received.from_id = u.id AND received.to_id = $1 AND received.direction = 'like'
             LEFT JOIN reads r ON r.user_id = $1 AND r.other_id = u.id AND COALESCE(r.deleted_at, 0) = 0
             LEFT JOIN LATERAL (
                SELECT body, photo, audio, created_at, from_id FROM messages m
                WHERE ((m.from_id = u.id AND m.to_id = $1) OR (m.from_id = $1 AND m.to_id = u.id))
                  AND COALESCE(m.deleted_at, 0) = 0 ORDER BY m.id DESC LIMIT 1
             ) last ON true
             CROSS JOIN LATERAL (
                SELECT count(*) FROM messages m WHERE m.from_id = u.id AND m.to_id = $1
                  AND m.id > COALESCE(r.last_read_id, 0) AND COALESCE(m.deleted_at, 0) = 0
             ) unread
             WHERE COALESCE(u.deleted_at, 0) = 0 AND NOT EXISTS (
                SELECT 1 FROM blocks b WHERE (b.from_id = $1 AND b.to_id = u.id) OR (b.to_id = $1 AND b.from_id = u.id)
             )
             ORDER BY greatest(COALESCE(last.created_at, 0), greatest(sent.created_at, received.created_at)) DESC,
                      matched_at DESC, u.id DESC",
            &[&input.uid],
        ).await?;
        Ok(rows.into_iter().map(|row| {
            let body = row.get::<_, &str>(2).trim();
            let photo = row.get::<_, &str>(3).trim();
            let audio = row.get::<_, &str>(4).trim();
            let preview = if !audio.is_empty() {
                &input.voice_label
            } else if body.is_empty() && !photo.is_empty() {
                &input.photo_label
            } else {
                body
            };
            MatchSummary {
                id: row.get(0), matched_at: row.get(1), last_message: preview.to_owned(),
                last_at: row.get(5), last_from_id: row.get(6), unread: row.get(7),
            }
        }).collect())
}

#[derive(Deserialize)]
pub(crate) struct NoticeRequest {
    uid: i64,
    #[serde(flatten)]
    operation: NoticeOperation,
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
enum NoticeOperation {
    List { limit: i64 },
    MarkRead { ids: Vec<i64> },
}

#[derive(Serialize)]
pub(crate) struct Notice {
    id: i64,
    pub kind: String,
    from_id: i64,
    pub body: String,
    created_at: i64,
}

pub(crate) async fn notices(
    State(state): State<Arc<Writer>>,
    headers: HeaderMap,
    Json(input): Json<NoticeRequest>,
) -> Result<Json<Vec<Notice>>, StatusCode> {
    state.authorize(&headers)?;
    state.transaction(move |tx| Box::pin(async move {
        match input.operation {
            NoticeOperation::List { limit } => unread_notices(tx, input.uid, limit.clamp(0, 100)).await,
            NoticeOperation::MarkRead { ids } => {
                mark_notices_read(tx, input.uid, ids).await?;
                Ok(Vec::new())
            }
        }
    })).await
}

pub(crate) async fn unread_notices(tx: &Transaction<'_>, uid: i64, limit: i64) -> TransactionResult<Vec<Notice>> {
    let rows = tx.query(
        "SELECT id::bigint, kind, from_id::bigint, body, created_at::bigint FROM notifications
         WHERE user_id = $1::bigint AND \"read\" = 0 ORDER BY id DESC LIMIT $2::bigint",
        &[&uid, &limit],
    ).await?;
    Ok(rows.into_iter().map(|row| Notice {
        id: row.get(0), kind: row.get(1), from_id: row.get(2), body: row.get(3), created_at: row.get(4),
    }).collect())
}

pub(crate) async fn mark_notices_read(tx: &Transaction<'_>, uid: i64, ids: Vec<i64>) -> TransactionResult<()> {
    tx.execute(
        "UPDATE notifications SET \"read\" = 1 WHERE user_id = $1::bigint AND \"read\" = 0
         AND (cardinality($2::bigint[]) = 0 OR id = ANY($2))",
        &[&uid, &ids],
    ).await?;
    Ok(())
}
