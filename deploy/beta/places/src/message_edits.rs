use axum::{Json, http::StatusCode, response::{IntoResponse, Response}};
use serde_json::{Value, json};

use crate::chat_write::{MessageAction, MutateMessage, Mutation, REACTIONS, Writer, mutate_message};

enum ResultCode {
    SignedOut,
    Banned,
    Allowed,
}

pub(crate) async fn delete(db:&Writer,media:&crate::media_http::Media,uid:i64,id:Option<i64>,now:i64,lang:&str) -> Result<Response,StatusCode> {
    let account = db.transaction(move |tx| Box::pin(async move {
        let row=tx.query_opt("SELECT COALESCE(report_banned_at,0)!=0 FROM users WHERE id=$1::bigint AND COALESCE(deleted_at,0)=0", &[&uid]).await?;
        let Some(row)=row else { return Ok(ResultCode::SignedOut); };
        if row.get::<_,bool>(0) { return Ok(ResultCode::Banned); }
        tx.execute("UPDATE users SET last_seen=$2::bigint WHERE id=$1::bigint", &[&uid,&now]).await?;
        Ok(ResultCode::Allowed)
    })).await?.0;
    match account {
        ResultCode::SignedOut=>return Ok(crate::account_settings::signed_out(lang)),
        ResultCode::Banned=>return Ok(crate::account_settings::report_ban_response(lang)),
        ResultCode::Allowed=>{},
    }
    if let Some(message_id)=id {
        let mutation = db.transaction(move |tx| Box::pin(async move {
            mutate_message(tx,&MutateMessage{uid,message_id,changed_at:now,action:MessageAction::Delete}).await
        })).await?.0;
        if let Mutation::Deleted{uploads}=mutation {
            media.erase_chat_uploads(uploads);
            return Ok(Json(json!({"ok":true})).into_response());
        }
    }
    let text=if lang=="en" {"This message cannot be deleted"} else {"это сообщение нельзя удалить"};
    Ok((StatusCode::NOT_FOUND,Json(json!({"ok":false,"error":text}))).into_response())
}

pub(crate) async fn mutate(db: &Writer, uid: i64, id: Option<i64>, reaction: bool, fields: Value, now: i64, lang: &str)
    -> Result<Response, StatusCode>
{
    let action = if reaction {
        match fields.get("emoji") {
            Some(Value::Null) => Some(MessageAction::React { emoji: None }),
            Some(Value::String(emoji)) if REACTIONS.contains(&emoji.as_str()) => Some(MessageAction::React { emoji: Some(emoji.clone()) }),
            _ => None,
        }
    } else {
        let raw = match &fields["body"] {
            Value::String(body) => body.clone(),
            Value::Number(body) if body.as_f64() != Some(0.0) => body.to_string(),
            Value::Bool(true) => "True".to_owned(),
            _ => String::new(),
        };
        let body = raw.trim();
        (1..=1000).contains(&body.chars().count()).then(|| MessageAction::Edit { body: body.to_owned() })
    };
    let result = db.transaction(move |tx| Box::pin(async move {
        let row = tx.query_opt("SELECT COALESCE(report_banned_at, 0) != 0 FROM users
            WHERE id = $1::bigint AND COALESCE(deleted_at, 0) = 0", &[&uid]).await?;
        let Some(row) = row else { return Ok(ResultCode::SignedOut); };
        if row.get::<_, bool>(0) { return Ok(ResultCode::Banned); }
        tx.execute("UPDATE users SET last_seen = $2::bigint WHERE id = $1::bigint", &[&uid, &now]).await?;
        Ok(ResultCode::Allowed)
    })).await?.0;
    let error = |status, en, ru| (status, Json(json!({"ok":false,"error":if lang == "en" { en } else { ru }}))).into_response();
    match result {
        ResultCode::SignedOut => return Ok(crate::account_settings::signed_out(lang)),
        ResultCode::Banned => return Ok(crate::account_settings::report_ban_response(lang)),
        ResultCode::Allowed => {},
    }
    let Some(action) = action else {
        return Ok(if reaction { error(StatusCode::BAD_REQUEST, "Invalid reaction", "Недопустимая реакция") }
            else { error(StatusCode::BAD_REQUEST, "Message must contain between 1 and 1000 characters", "сообщение: 1–1000 символов") });
    };
    // Commit last_seen before taking the pair lock, as Flask does. Reports lock the pair first.
    let mutation = if let Some(message_id) = id {
        db.transaction(move |tx| Box::pin(async move {
            mutate_message(tx, &MutateMessage { uid, message_id, changed_at: now, action }).await
        })).await?.0
    } else { Mutation::Unavailable };
    Ok(match mutation {
        Mutation::Unavailable if reaction => error(StatusCode::NOT_FOUND, "Message unavailable", "Сообщение недоступно"),
        Mutation::Unavailable => error(StatusCode::NOT_FOUND, "This message cannot be edited", "это сообщение нельзя изменить"),
        Mutation::Voice => error(StatusCode::BAD_REQUEST, "A voice message cannot be replaced with text", "голосовое нельзя переписать текстом"),
        Mutation::Edited => Json(json!({"ok":true})).into_response(),
        Mutation::Reacted { reactions } => Json(json!({"ok":true,"reactions":reactions})).into_response(),
        Mutation::Deleted { .. } | Mutation::Transcribed { .. } => unreachable!(),
    })
}
