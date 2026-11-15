use std::sync::Arc;

use axum::{Json, http::StatusCode, response::{IntoResponse, Response}};
use serde_json::{Value, json};

use crate::{chat_read::{MatchesRequest, fetch_matches}, chat_write::Writer, profile_read::ProfileRules};

enum ResultCode {
    SignedOut,
    Banned,
    Hidden,
    Missing,
    Output(Value),
}

pub(crate) async fn read(db: &Writer, profile: Arc<ProfileRules>, uid: i64, path: &str, now: i64, lang: &'static str)
    -> Result<Response, StatusCode>
{
    let person = path.strip_prefix("/api/people/");
    if person.is_some_and(|id| id.is_empty() || !id.bytes().all(|byte| byte.is_ascii_digit())) {
        return Ok(StatusCode::NOT_FOUND.into_response());
    }
    let matches = person.is_none();
    let other = person.and_then(|id| id.parse::<i64>().ok());
    let auth = db.transaction(move |tx| Box::pin(async move {
        let account = tx.query_opt("SELECT COALESCE(report_banned_at,0)!=0 FROM users
             WHERE id=$1::bigint AND COALESCE(deleted_at,0)=0", &[&uid]).await?;
        let Some(account) = account else { return Ok(Some(ResultCode::SignedOut)); };
        if account.get::<_, bool>(0) { return Ok(Some(ResultCode::Banned)); }
        tx.execute("UPDATE users SET last_seen=$2::bigint WHERE id=$1::bigint", &[&uid,&now]).await?;
        Ok(None)
    })).await?.0;
    let result = if let Some(auth) = auth { auth } else {
        db.transaction(move |tx| Box::pin(async move {
            if matches {
                let (voice, photo) = if lang == "en" { ("voice message", "photo") } else { ("голосовое", "фото") };
                let summaries = fetch_matches(tx, MatchesRequest { uid, voice_label: voice.to_owned(), photo_label: photo.to_owned() }).await?;
                let mut cards = Vec::with_capacity(summaries.len());
                for summary in summaries {
                    let mut card = profile.card(tx, summary.id, now, lang).await?;
                    if let (Some(card), Value::Object(summary)) = (card.as_object_mut(), serde_json::to_value(summary)?) {
                        card.extend(summary);
                        cards.push(Value::Object(std::mem::take(card)));
                    }
                }
                return Ok(ResultCode::Output(json!({"ok":true,"matches":cards})));
            }
            let Some(other) = other else { return Ok(ResultCode::Missing); };
            let own = uid == other;
            if !own {
                let row = tx.query_one("SELECT EXISTS (SELECT 1 FROM blocks
                    WHERE (from_id=$1::bigint AND to_id=$2::bigint) OR (from_id=$2::bigint AND to_id=$1::bigint))", &[&uid,&other]).await?;
                if row.get::<_, bool>(0) { return Ok(ResultCode::Hidden); }
                let row = tx.query_opt("SELECT email FROM users WHERE id=$1::bigint
                    AND COALESCE(deleted_at,0)=0 AND COALESCE(report_banned_at,0)=0", &[&other]).await?;
                let Some(row) = row else { return Ok(ResultCode::Missing); };
                let email: &str = row.get(0);
                if email.ends_with("@wiring.guest") || email == "demo@wiring.app" {
                    return Ok(ResultCode::Missing);
                }
            }
            let mut person = profile.person(tx, other, now, lang, own).await?;
            if person.is_null() { return Ok(ResultCode::Missing); }
            let (liked_you, you_liked) = if own { (false, false) } else {
                let row = tx.query_one("SELECT
                    EXISTS(SELECT 1 FROM swipes WHERE from_id=$2::bigint AND to_id=$1::bigint AND direction='like'),
                    EXISTS(SELECT 1 FROM swipes WHERE from_id=$1::bigint AND to_id=$2::bigint AND direction='like')", &[&uid,&other]).await?;
                (row.get::<_,bool>(0), row.get::<_,bool>(1))
            };
            person["matched"] = json!(liked_you && you_liked);
            person["liked_you"] = json!(liked_you);
            person["you_liked"] = json!(you_liked);
            Ok(ResultCode::Output(json!({"ok":true,"person":person})))
        })).await?.0
    };
    let error = |en, ru| (StatusCode::NOT_FOUND, Json(json!({"ok":false,"error":if lang == "en" { en } else { ru }}))).into_response();
    Ok(match result {
        ResultCode::SignedOut => crate::account_settings::signed_out(lang),
        ResultCode::Banned => crate::account_settings::report_ban_response(lang),
        ResultCode::Hidden => error("This person is hidden", "этот человек скрыт"),
        ResultCode::Missing => error("Person not found", "человек не найден"),
        ResultCode::Output(data) => Json(data).into_response(),
    })
}
