use std::collections::HashMap;

use axum::Json;
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
pub struct FeedRequest {
    country: String,
    intents: Vec<String>,
    candidates: Vec<FeedCandidate>,
}

#[derive(Deserialize)]
struct FeedCandidate {
    id: i64,
    country: String,
    intents: Vec<String>,
    hidden: bool,
}

pub async fn feed(Json(request): Json<FeedRequest>) -> Json<Vec<i64>> {
    Json(
        request
            .candidates
            .into_iter()
            .filter(|candidate| {
                !candidate.hidden
                    && (request.country.is_empty() || candidate.country == request.country)
                    && (request.intents.is_empty()
                        || candidate.intents.iter().any(|intent| request.intents.contains(intent)))
            })
            .map(|candidate| candidate.id)
            .collect(),
    )
}

#[derive(Deserialize)]
pub struct ThreadRequest {
    pub(crate) uid: i64,
    pub(crate) peer_read_id: i64,
    pub(crate) base_path: String,
    pub(crate) messages: Vec<Message>,
    pub(crate) replies: HashMap<i64, ReplySource>,
}

#[derive(Deserialize)]
pub(crate) struct Message {
    pub(crate) id: i64,
    pub(crate) from_id: i64,
    pub(crate) body: String,
    pub(crate) photo: String,
    pub(crate) photo_count: usize,
    pub(crate) audio: bool,
    pub(crate) audio_duration: i64,
    pub(crate) edited: bool,
    pub(crate) created_at: i64,
    pub(crate) reply_to_id: Option<i64>,
}

#[derive(Deserialize)]
pub(crate) struct ReplySource {
    pub(crate) from_id: i64,
    pub(crate) gone: bool,
    pub(crate) audio: bool,
    pub(crate) photo: bool,
    pub(crate) text: String,
    pub(crate) placeholder: String,
}

#[derive(Serialize)]
pub struct ThreadMessage {
    pub(crate) id: i64,
    from_id: i64,
    mine: bool,
    body: String,
    photo: String,
    photo_url: String,
    photo_urls: Vec<String>,
    audio_url: String,
    audio_duration: i64,
    transcript: &'static str,
    edited: bool,
    created_at: i64,
    read: bool,
    reply_to: Option<Reply>,
}

#[derive(Serialize)]
struct Reply {
    id: i64,
    body: String,
    mine: bool,
    has_photo: bool,
    has_audio: bool,
    gone: bool,
}

pub async fn thread(Json(request): Json<ThreadRequest>) -> Json<Vec<ThreadMessage>> {
    Json(render_thread(request))
}

pub(crate) fn render_thread(request: ThreadRequest) -> Vec<ThreadMessage> {
    request
        .messages
        .into_iter()
        .map(|message| {
            let mine = message.from_id == request.uid;
            let media = format!("{}/api/messages/media/{}", request.base_path, message.id);
            let photo_url = if message.photo.is_empty() { String::new() } else { media.clone() };
            let first_index = usize::from(message.photo_count > 1);
            let photo_urls = (first_index..message.photo_count)
                .map(|index| format!("{media}?index={index}"))
                .collect();
            let reply_to = message.reply_to_id.and_then(|id| {
                request.replies.get(&id).map(|source| Reply {
                    id,
                    body: if source.text.is_empty() {
                        source.placeholder.clone()
                    } else {
                        source.text.chars().take(160).collect()
                    },
                    mine: source.from_id == request.uid,
                    has_photo: !source.gone && source.photo,
                    has_audio: !source.gone && source.audio,
                    gone: source.gone,
                })
            });
            ThreadMessage {
                id: message.id,
                from_id: message.from_id,
                mine,
                body: message.body,
                photo: message.photo,
                photo_url,
                photo_urls,
                audio_url: if message.audio { media } else { String::new() },
                audio_duration: message.audio_duration,
                transcript: "",
                edited: message.edited,
                created_at: message.created_at,
                read: mine && message.id <= request.peer_read_id,
                reply_to,
            }
        })
        .collect()
}
