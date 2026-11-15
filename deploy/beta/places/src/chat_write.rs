use std::{env, error::Error, future::Future, pin::Pin, sync::Arc, time::Duration};

use axum::{Json, Router, extract::State, http::{HeaderMap, StatusCode}, routing::post};
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;
use tokio_postgres::{Config, NoTls, Transaction, types::ToSql};

pub(crate) struct Writer {
    db: Config,
    token: String,
    slots: tokio::sync::Semaphore,
}

pub(crate) type TransactionResult<T> = Result<T, Box<dyn Error + Send + Sync>>;
type TransactionFuture<'a, T> = Pin<Box<dyn Future<Output = TransactionResult<T>> + Send + 'a>>;

#[derive(Deserialize)]
pub(crate) struct SendMessage {
    pub(crate) uid: i64,
    pub(crate) other_id: i64,
    pub(crate) body: String,
    pub(crate) reply_to_id: Option<i64>,
    pub(crate) client_id: Option<String>,
    pub(crate) created_at: i64,
    pub(crate) notice_body: Option<String>,
    pub(crate) media: Option<Media>,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum Media {
    Photo { paths: Vec<String> },
    Voice { path: String, duration: i64 },
}

#[derive(Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub(crate) enum Outcome {
    Sent { id: i64, updated_notice: bool },
    Duplicate { id: i64 },
    Blocked,
    NoMatch,
    ReplyMissing,
}

pub(crate) const REACTIONS: [&str; 6] = ["❤️", "👍", "😂", "😮", "😢", "🎉"];

#[derive(Deserialize)]
pub(crate) struct MutateMessage {
    pub(crate) uid: i64,
    pub(crate) message_id: i64,
    pub(crate) changed_at: i64,
    #[serde(flatten)]
    pub(crate) action: MessageAction,
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub(crate) enum MessageAction {
    Edit { body: String },
    Delete,
    React { emoji: Option<String> },
    Transcribe { transcript: String },
}

#[derive(Serialize)]
pub(crate) struct Reaction {
    pub(crate) emoji: &'static str,
    pub(crate) count: i64,
    pub(crate) mine: bool,
}

#[derive(Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub(crate) enum Mutation {
    Edited,
    Deleted { uploads: Vec<String> },
    Reacted { reactions: Vec<Reaction> },
    Transcribed { transcript: String },
    Unavailable,
    Voice,
}

pub(crate) fn routes(state: Option<Arc<Writer>>) -> Router {
    let Some(state) = state else { return Router::new() };
    Router::new()
        .route("/chat/send", post(send))
        .route("/chat/mutate", post(mutate))
        .route("/chat/history", post(crate::chat_read::history))
        .route("/chat/inbox", post(crate::chat_read::inbox))
        .route("/chat/matches", post(crate::chat_read::matches))
        .route("/notices/query", post(crate::chat_read::notices))
        .route("/auth/password", post(crate::password::verify))
        .route("/auth/refresh", post(crate::mobile_session::refresh))
        .route("/auth/logout", post(crate::mobile_session::logout))
        .route("/auth/issue", post(crate::mobile_session::issue))
        .route("/profile/settings", post(crate::profile::settings))
        .route("/chat/pair", post(crate::chat_pair::change))
        .route("/feed/update", post(crate::feed_write::update))
        .route("/feed/claim", post(crate::feed_write::claim))
        .route("/feed/swipe", post(crate::feed_write::swipe))
        .with_state(state)
}

impl Writer {
    pub(crate) fn from_env() -> Result<Option<Arc<Self>>, Box<dyn Error>> {
        let url = env::var("WIRING_CHAT_DATABASE_URL").unwrap_or_default();
        if url.trim().is_empty() {
            return Ok(None);
        }
        let token = env::var("WIRING_CHAT_INTERNAL_TOKEN")?;
        if token.len() < 32 {
            return Err("Chat internal token must contain at least 32 characters".into());
        }
        let mut db: Config = url.parse()?;
        db.connect_timeout(Duration::from_secs(2));
        Ok(Some(Arc::new(Self { db, token, slots: tokio::sync::Semaphore::new(8) })))
    }

    pub(crate) fn authorize(&self, headers: &HeaderMap) -> Result<(), StatusCode> {
        let token = headers.get("x-wiring-chat-token").map_or(&[][..], |value| value.as_bytes());
        if bool::from(token.ct_eq(self.token.as_bytes())) {
            Ok(())
        } else {
            Err(StatusCode::UNAUTHORIZED)
        }
    }

    pub(crate) async fn transaction<T>(
        &self,
        operation: impl for<'a> FnOnce(&'a Transaction<'_>) -> TransactionFuture<'a, T> + Send,
    ) -> Result<Json<T>, StatusCode> {
        let _slot = self.slots.try_acquire().map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
        let result = async {
            let (mut client, connection) = self.db.connect(NoTls).await?;
            tokio::spawn(async move { let _ = connection.await; });
            let tx = client.transaction().await?;
            tx.batch_execute("SET LOCAL statement_timeout = '3s'; SET LOCAL lock_timeout = '2s'").await?;
            let result = operation(&tx).await?;
            tx.commit().await?;
            Ok::<_, Box<dyn Error + Send + Sync>>(result)
        }.await;
        result.map(Json).map_err(|error| {
            let code = error.downcast_ref::<tokio_postgres::Error>().and_then(|error| error.code());
            eprintln!("Chat database transaction failed (SQLSTATE {})", code.map_or("unknown", |code| code.code()));
            StatusCode::SERVICE_UNAVAILABLE
        })
    }
}

async fn send(
    State(state): State<Arc<Writer>>,
    headers: HeaderMap,
    Json(input): Json<SendMessage>,
) -> Result<Json<Outcome>, StatusCode> {
    state.authorize(&headers)?;
    if (input.media.is_none() && input.body.is_empty()) || input.body.chars().count() > 1000
        || input.client_id.as_ref().is_some_and(|id| id.is_empty() || id.chars().count() > 64)
    {
        return Err(StatusCode::BAD_REQUEST);
    }
    let valid_path = |path: &str| {
        let prefix = format!("chat/{}/", input.uid);
        path.strip_prefix(&prefix).is_some_and(|name| {
            !name.is_empty() && !name.contains("..") && !name.contains(['/', '\\'])
        })
    };
    match &input.media {
        Some(Media::Photo { paths }) if !(1..=11).contains(&paths.len()) || !paths.iter().all(|path| valid_path(path)) => {
            return Err(StatusCode::BAD_REQUEST);
        }
        Some(Media::Voice { path, duration }) if !valid_path(path) || *duration <= 0 => {
            return Err(StatusCode::BAD_REQUEST);
        }
        _ => {}
    }
    state.transaction(move |tx| Box::pin(async move { Ok(write_message(tx, &input).await?) })).await
}

pub(crate) async fn lock_pair(tx: &Transaction<'_>, pair: &[&(dyn ToSql + Sync)]) -> Result<(), tokio_postgres::Error> {
    tx.query_one(
        "SELECT pg_advisory_xact_lock(hashtextextended(LEAST($1::bigint, $2::bigint)::text || ':' || GREATEST($1, $2)::text, 439817))",
        pair,
    ).await?;
    Ok(())
}

pub(crate) async fn pair_access(tx: &Transaction<'_>, pair: &[&(dyn ToSql + Sync)]) -> Result<(bool, bool), tokio_postgres::Error> {
    let access = tx.query_one(
        "SELECT EXISTS(SELECT 1 FROM blocks WHERE (from_id = $1::bigint AND to_id = $2::bigint) OR (from_id = $2 AND to_id = $1)),
         (SELECT count(*) FROM users WHERE id IN ($1, $2) AND COALESCE(deleted_at, 0) = 0) = CASE WHEN $1 = $2 THEN 1 ELSE 2 END
         AND EXISTS(SELECT 1 FROM swipes WHERE from_id = $1 AND to_id = $2 AND direction = 'like')
         AND EXISTS(SELECT 1 FROM swipes WHERE from_id = $2 AND to_id = $1 AND direction = 'like')",
        pair,
    ).await?;
    Ok((access.get(0), access.get(1)))
}

pub(crate) async fn write_message(tx: &Transaction<'_>, input: &SendMessage) -> Result<Outcome, tokio_postgres::Error> {
    let pair: [&(dyn ToSql + Sync); 2] = [&input.uid, &input.other_id];
    lock_pair(tx, &pair).await?;
    let (blocked, matched) = pair_access(tx, &pair).await?;
    if blocked {
        return Ok(Outcome::Blocked);
    }
    if !matched {
        return Ok(Outcome::NoMatch);
    }
    if let Some(reply) = input.reply_to_id {
        let voice = matches!(input.media, Some(Media::Voice { .. }));
        let exists = tx.query_opt(
            "SELECT id FROM messages WHERE id = $3::bigint AND
             ((from_id = $1::bigint AND to_id = $2::bigint) OR (from_id = $2 AND to_id = $1))
             AND (NOT $4 OR COALESCE(deleted_at, 0) = 0)",
            &[&input.uid, &input.other_id, &reply, &voice],
        ).await?;
        if exists.is_none() {
            return Ok(Outcome::ReplyMissing);
        }
    }
    let inserted = if let Some(media) = &input.media {
        let (photo, extra, audio, duration) = match media {
            Media::Photo { paths } => (paths[0].as_str(), serde_json::json!(&paths[1..]).to_string(), "", 0),
            Media::Voice { path, duration } => ("", "[]".to_owned(), path.as_str(), *duration),
        };
        tx.query_opt(
            "INSERT INTO messages (from_id, to_id, body, created_at, reply_to_id, client_id, photo, extra_photos, audio, audio_duration)
             VALUES ($1::bigint, $2::bigint, $3, $4::bigint, $5::bigint, $6, $7, $8, $9, $10::bigint)
             ON CONFLICT (from_id, client_id) WHERE client_id IS NOT NULL DO NOTHING RETURNING id::bigint",
            &[&input.uid, &input.other_id, &input.body, &input.created_at, &input.reply_to_id, &input.client_id,
              &photo, &extra, &audio, &duration],
        ).await?
    } else {
        tx.query_opt(
            "INSERT INTO messages(from_id, to_id, body, created_at, reply_to_id, client_id)
             VALUES ($1::bigint, $2::bigint, $3, $4::bigint, $5::bigint, $6)
             ON CONFLICT (from_id, client_id) WHERE client_id IS NOT NULL DO NOTHING RETURNING id::bigint",
            &[&input.uid, &input.other_id, &input.body, &input.created_at, &input.reply_to_id, &input.client_id],
        ).await?
    };
    let Some(inserted) = inserted else {
        let duplicate = tx.query_one(
            "SELECT id::bigint FROM messages WHERE from_id = $1::bigint AND client_id = $2",
            &[&input.uid, &input.client_id],
        ).await?;
        return Ok(Outcome::Duplicate { id: duplicate.get(0) });
    };
    mark_read(tx, &pair).await?;

    let mut updated_notice = false;
    if let Some(body) = &input.notice_body {
        updated_notice = tx.execute(
            "UPDATE notifications SET body = $3, created_at = $4::bigint WHERE id = (
                 SELECT id FROM notifications WHERE user_id = $2::bigint AND from_id = $1::bigint
                     AND kind = 'message' AND \"read\" = 0 ORDER BY id LIMIT 1 FOR UPDATE
             )",
            &[&input.uid, &input.other_id, body, &input.created_at],
        ).await? > 0;
        if !updated_notice {
            tx.execute(
                "INSERT INTO notifications (user_id, kind, from_id, body, created_at, \"read\")
                 VALUES ($2::bigint, 'message', $1::bigint, $3, $4::bigint, 0)",
                &[&input.uid, &input.other_id, body, &input.created_at],
            ).await?;
        }
    }
    Ok(Outcome::Sent { id: inserted.get(0), updated_notice })
}

pub(crate) async fn mark_read(tx: &Transaction<'_>, pair: &[&(dyn ToSql + Sync)]) -> Result<(), tokio_postgres::Error> {
    tx.execute(
        "INSERT INTO reads(user_id, other_id, last_read_id, deleted_at)
         SELECT $1::bigint, $2::bigint, COALESCE(MAX(id), 0), NULL FROM messages
         WHERE ((from_id = $1 AND to_id = $2) OR (from_id = $2 AND to_id = $1))
             AND COALESCE(deleted_at, 0) = 0
         ON CONFLICT (user_id, other_id) DO UPDATE SET last_read_id = excluded.last_read_id, deleted_at = NULL",
        pair,
    ).await?;
    Ok(())
}

async fn mutate(
    State(state): State<Arc<Writer>>,
    headers: HeaderMap,
    Json(input): Json<MutateMessage>,
) -> Result<Json<Mutation>, StatusCode> {
    state.authorize(&headers)?;
    match &input.action {
        MessageAction::Edit { body } if body.is_empty() || body.chars().count() > 1000 => {
            return Err(StatusCode::BAD_REQUEST);
        }
        MessageAction::React { emoji: Some(emoji) } if !REACTIONS.contains(&emoji.as_str()) => {
            return Err(StatusCode::BAD_REQUEST);
        }
        _ => {}
    }
    state.transaction(move |tx| Box::pin(async move { mutate_message(tx, &input).await })).await
}

pub(crate) async fn mutate_message(
    tx: &Transaction<'_>,
    input: &MutateMessage,
) -> TransactionResult<Mutation> {
    let Some(participants) = tx.query_opt(
        "SELECT from_id::bigint, to_id::bigint FROM messages WHERE id = $1::bigint",
        &[&input.message_id],
    ).await? else {
        return Ok(Mutation::Unavailable);
    };
    let from_id: i64 = participants.get(0);
    let to_id: i64 = participants.get(1);
    let participant_action = matches!(input.action, MessageAction::React { .. } | MessageAction::Transcribe { .. });
    if input.uid != from_id && (!participant_action || input.uid != to_id) {
        return Ok(Mutation::Unavailable);
    }
    let pair: [&(dyn ToSql + Sync); 2] = [&from_id, &to_id];
    lock_pair(tx, &pair).await?;
    let (blocked, matched) = pair_access(tx, &pair).await?;
    if blocked || !matched {
        return Ok(Mutation::Unavailable);
    }
    let Some(row) = tx.query_opt(
        "SELECT photo, extra_photos, audio FROM messages WHERE id = $1::bigint
         AND COALESCE(deleted_at, 0) = 0 FOR UPDATE",
        &[&input.message_id],
    ).await? else {
        return Ok(Mutation::Unavailable);
    };

    match &input.action {
        MessageAction::Edit { body } => {
            if row.get::<_, Option<&str>>(2).is_some_and(|audio| !audio.trim().is_empty()) {
                return Ok(Mutation::Voice);
            }
            tx.execute(
                "UPDATE messages SET body = $2, edited_at = $3::bigint WHERE id = $1::bigint",
                &[&input.message_id, body, &input.changed_at],
            ).await?;
            Ok(Mutation::Edited)
        }
        MessageAction::Delete => {
            let mut uploads = Vec::new();
            if let Some(photo) = row.get::<_, Option<&str>>(0).filter(|photo| !photo.is_empty()) {
                uploads.push(photo.to_owned());
            }
            if let Some(extra) = row.get::<_, Option<&str>>(1).filter(|extra| !extra.is_empty()) {
                uploads.extend(serde_json::from_str::<Vec<String>>(extra)?);
            }
            if let Some(audio) = row.get::<_, Option<&str>>(2).filter(|audio| !audio.is_empty()) {
                uploads.push(audio.to_owned());
            }
            tx.execute(
                "UPDATE messages SET deleted_at = $2::bigint WHERE id = $1::bigint",
                &[&input.message_id, &input.changed_at],
            ).await?;
            Ok(Mutation::Deleted { uploads })
        }
        MessageAction::React { emoji } => {
            let reactions = set_reaction(tx, input.message_id, input.uid, emoji.as_deref()).await?;
            Ok(Mutation::Reacted { reactions })
        }
        MessageAction::Transcribe { transcript } => {
            if !row.get::<_, Option<&str>>(2).is_some_and(|audio| !audio.trim().is_empty()) {
                return Ok(Mutation::Unavailable);
            }
            let stored: String = tx.query_one(
                "UPDATE messages SET transcript = CASE WHEN btrim(transcript) = '' THEN $2 ELSE transcript END
                 WHERE id = $1::bigint RETURNING transcript",
                &[&input.message_id, transcript],
            ).await?.get(0);
            Ok(Mutation::Transcribed { transcript: stored })
        }
    }
}

async fn set_reaction(
    tx: &Transaction<'_>, message_id: i64, uid: i64, emoji: Option<&str>,
) -> Result<Vec<Reaction>, tokio_postgres::Error> {
    if let Some(emoji) = emoji {
        tx.execute(
            "INSERT INTO message_reactions (message_id, user_id, emoji) VALUES ($1, $2, $3)
             ON CONFLICT (message_id, user_id) DO UPDATE SET emoji = EXCLUDED.emoji",
            &[&message_id, &uid, &emoji],
        ).await?;
    } else {
        tx.execute(
            "DELETE FROM message_reactions WHERE message_id = $1 AND user_id = $2",
            &[&message_id, &uid],
        ).await?;
    }
    let groups = tx.query(
        "SELECT emoji, count(*), bool_or(user_id = $2) FROM message_reactions WHERE message_id = $1 GROUP BY emoji",
        &[&message_id, &uid],
    ).await?;
    Ok(REACTIONS.into_iter().filter_map(|emoji| {
        let group = groups.iter().find(|row| row.get::<_, &str>(0) == emoji)?;
        Some(Reaction { emoji, count: group.get(1), mine: group.get(2) })
    }).collect())
}
