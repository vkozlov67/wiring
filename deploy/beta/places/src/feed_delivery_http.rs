use axum::{Json, http::StatusCode, response::{IntoResponse, Response}};
use serde_json::{Value, json};

use crate::{chat_write::Writer, feed_write::{FeedAction, FeedResult, FeedUpdate, update_delivery}};

pub(crate) async fn serve(db: &Writer, uid: i64, path: &str, fields: Value, now: i64, lang: &str)
    -> Result<Response, StatusCode>
{
    let error = |status, en, ru| (status, Json(json!({"ok":false,"error":if lang == "en" { en } else { ru }}))).into_response();
    let viewer = db.transaction(move |tx| Box::pin(async move {
        let row = tx.query_opt("SELECT COALESCE(report_banned_at,0)!=0,email FROM users
            WHERE id=$1::bigint AND COALESCE(deleted_at,0)=0", &[&uid]).await?;
        let Some(row) = row else { return Ok(None); };
        let banned: bool = row.get(0);
        if !banned {
            tx.execute("UPDATE users SET last_seen=$2::bigint WHERE id=$1::bigint", &[&uid, &now]).await?;
        }
        Ok(Some((banned, row.get::<_, String>(1))))
    })).await?.0;
    let Some((banned, email)) = viewer else { return Ok(crate::account_settings::signed_out(lang)); };
    if banned { return Ok(crate::account_settings::report_ban_response(lang)); }
    let action = match path {
        "/api/deck/restart" => return Ok(error(StatusCode::GONE, "excluded profiles stay hidden", "исключённые анкеты не возвращаются")),
        "/api/feed/view" => {
            let target_id = match &fields["target_id"] {
                Value::String(value) => value.trim().parse::<i64>().ok(),
                Value::Number(value) => value.as_i64().or_else(|| value.as_f64().filter(|n|
                    (-9_223_372_036_854_775_808.0..9_223_372_036_854_775_808.0).contains(n)).map(|n| n as i64)),
                Value::Bool(value) => Some(i64::from(*value)),
                _ => None,
            };
            let Some(target_id) = target_id else { return Ok(error(StatusCode::BAD_REQUEST, "Missing target", "нет цели")); };
            FeedAction::View { target_id }
        }
        _ => {
            let Some(generation) = fields["generation"].as_u64() else {
                return Ok(error(StatusCode::BAD_REQUEST, "Invalid feed version", "неверная версия ленты"));
            };
            // Matches qa_accounts.py; this exception bypasses the premium check for feed reset only.
            let qa = email.trim().eq_ignore_ascii_case("wiringremlin@gmail.com");
            FeedAction::Reset { generation: generation.min(i64::MAX as u64) as i64, qa }
        }
    };
    let result = db.transaction(move |tx| Box::pin(update_delivery(tx, FeedUpdate { uid, now, action }))).await?.0;
    Ok(match result {
        FeedResult::Viewed => Json(json!({"ok":true})).into_response(),
        FeedResult::NotDelivered => error(StatusCode::NOT_FOUND, "This profile was not delivered to you", "анкета не выдавалась"),
        FeedResult::PremiumRequired => error(StatusCode::FORBIDDEN, "Available only with WIRING+", "доступно только с WIRING+"),
        FeedResult::Reset { reset, generation } => Json(json!({"ok":true,"reset":reset,"generation":generation})).into_response(),
    })
}
