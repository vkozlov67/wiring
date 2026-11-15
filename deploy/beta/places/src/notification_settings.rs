use std::sync::Arc;

use axum::{Json, http::StatusCode, response::{IntoResponse, Response}};
use serde_json::{Value, json};

use crate::{chat_write::Writer, premium_http::truthy, profile_read::ProfileRules};

enum Outcome { Missing, Banned, Updated(Value) }

pub(crate) async fn patch(db: &Writer, profile: Arc<ProfileRules>, uid: i64, fields: Value, now: i64, lang: &'static str)
    -> Result<Response, StatusCode>
{
    let outcome = db.transaction(move |tx| Box::pin(async move {
        let row = tx.query_opt("SELECT COALESCE(report_banned_at,0)!=0,COALESCE(notify_enabled,1)!=0,COALESCE(notify_push,1)!=0
            FROM users WHERE id=$1::bigint AND COALESCE(deleted_at,0)=0 FOR UPDATE", &[&uid]).await?;
        let Some(row) = row else { return Ok(Outcome::Missing); };
        if row.get::<_,bool>(0) { return Ok(Outcome::Banned); }
        let enabled = fields.get("enabled").map_or_else(|| row.get::<_,bool>(1), truthy);
        let push = enabled && fields.get("push").map_or_else(|| row.get::<_,bool>(2), truthy);
        if !push {
            tx.execute("DELETE FROM push_subscriptions WHERE user_id=$1::bigint", &[&uid]).await?;
            tx.execute("DELETE FROM mobile_devices WHERE user_id=$1::bigint", &[&uid]).await?;
        }
        tx.execute("UPDATE users SET notify_enabled=$2,notify_push=$3,last_seen=$4::bigint WHERE id=$1::bigint",
            &[&uid,&i32::from(enabled),&i32::from(push),&now]).await?;
        Ok(Outcome::Updated(profile.person(tx, uid, now, lang, true).await?))
    })).await?.0;
    Ok(match outcome {
        Outcome::Missing => crate::account_settings::signed_out(lang),
        Outcome::Banned => crate::account_settings::report_ban_response(lang),
        Outcome::Updated(user) => Json(json!({"ok":true,"user":user})).into_response(),
    })
}
