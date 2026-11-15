use std::{sync::Arc, time::Instant};

use axum::{Json, http::StatusCode, response::{IntoResponse, Response}};
use serde_json::{Value, json};

use crate::{chat_write::Writer, login::LoginLimit, mobile_http::string_field, profile_read::ProfileRules};

enum Outcome {
    Missing, Banned, Guest, NotPremium, Limited, NoCode, UnknownCode, Spent, Used, NotReal, Unsupported,
    Updated { user: Value, until: Option<i64> },
}

pub(crate) async fn serve(db: &Writer, profile: Arc<ProfileRules>, limit: &LoginLimit, uid: i64, redeem: bool,
    fields: Value, now: i64, ip: &str, lang: &'static str) -> Result<Response, StatusCode>
{
    let gate = db.transaction(move |tx| Box::pin(async move {
        let row = tx.query_opt("SELECT email, COALESCE(report_banned_at,0)!=0 FROM users
            WHERE id=$1::bigint AND COALESCE(deleted_at,0)=0", &[&uid]).await?;
        let Some(row) = row else { return Ok(Some(Outcome::Missing)); };
        if row.get::<_,bool>(1) { return Ok(Some(Outcome::Banned)); }
        tx.execute("UPDATE users SET last_seen=$2::bigint WHERE id=$1::bigint", &[&uid,&now]).await?;
        let email:&str = row.get(0);
        Ok((email.ends_with("@wiring.guest") || email == "demo@wiring.app").then_some(Outcome::Guest))
    })).await?.0;
    let outcome = if let Some(gate) = gate { gate } else if redeem && !limit.allow(ip, Instant::now()) { Outcome::Limited } else {
        db.transaction(move |tx| Box::pin(async move {
            let mut promotion = None;
            if redeem {
                let code: String = string_field(&fields, "code").chars().flat_map(char::to_uppercase)
                    .filter(|c| c.is_alphanumeric() || matches!(c,'-'|'_')).collect();
                if code.is_empty() { return Ok(Outcome::NoCode); }
                let row = tx.query_opt("SELECT days::bigint, max_uses::bigint, uses::bigint, benefit
                    FROM promo_codes WHERE code=$1 FOR UPDATE", &[&code]).await?;
                let Some(row) = row else { return Ok(Outcome::UnknownCode); };
                let max: i64 = row.get(1);
                if max != 0 && row.get::<_,i64>(2) >= max { return Ok(Outcome::Spent); }
                if tx.query_opt("SELECT 1 FROM promo_redemptions WHERE code=$1 AND user_id=$2::bigint", &[&code,&uid]).await?.is_some() {
                    return Ok(Outcome::Used);
                }
                promotion = Some((code, row.get::<_,i64>(0), row.get::<_,String>(3)));
            }
            let row = tx.query_opt("SELECT COALESCE(premium_until,0)::bigint,is_seed!=0,incognito,paused
                FROM users WHERE id=$1::bigint AND COALESCE(deleted_at,0)=0 FOR UPDATE", &[&uid]).await?;
            let Some(row) = row else { return Ok(Outcome::Missing); };
            let premium: i64 = row.get(0);
            let until = if let Some((code, days, benefit)) = promotion {
                let until = match benefit.as_str() {
                    "recommendations" if row.get::<_,bool>(1) => return Ok(Outcome::NotReal),
                    "recommendations" => {
                        tx.execute("UPDATE users SET jev_recommendations_unlocked=1 WHERE id=$1::bigint", &[&uid]).await?;
                        premium
                    }
                    "plus" if days <= 0 => {
                        tx.execute("UPDATE users SET premium_until=0,incognito=0,paused=0 WHERE id=$1::bigint", &[&uid]).await?;
                        0
                    }
                    "plus" => {
                        let until = premium.max(now) + days * 86400;
                        tx.execute("UPDATE users SET premium_until=$2::bigint WHERE id=$1::bigint", &[&uid,&until]).await?;
                        until
                    }
                    _ => return Ok(Outcome::Unsupported),
                };
                tx.execute("UPDATE promo_codes SET uses=uses+1 WHERE code=$1", &[&code]).await?;
                tx.execute("INSERT INTO promo_redemptions(code,user_id,created_at) VALUES ($1,$2::bigint,$3::bigint)", &[&code,&uid,&now]).await?;
                Some(until)
            } else {
                if premium <= now { return Ok(Outcome::NotPremium); }
                let incognito = fields.get("incognito").map_or_else(|| row.get::<_,i32>(2), |v| i32::from(truthy(v)));
                let paused = fields.get("paused").map_or_else(|| row.get::<_,i32>(3), |v| i32::from(truthy(v)));
                tx.execute("UPDATE users SET incognito=$2,paused=$3 WHERE id=$1::bigint", &[&uid,&incognito,&paused]).await?;
                None
            };
            Ok(Outcome::Updated { user: profile.read(tx, uid, now, lang).await?.0, until })
        })).await?.0
    };
    let error = |status, en, ru| (status, Json(json!({"ok":false,"error":if lang == "en" { en } else { ru }}))).into_response();
    Ok(match outcome {
        Outcome::Missing => crate::account_settings::signed_out(lang),
        Outcome::Banned => crate::account_settings::report_ban_response(lang),
        Outcome::Guest => error(StatusCode::FORBIDDEN, "Complete your profile first", "сначала собери свой профиль"),
        Outcome::NotPremium => (StatusCode::FORBIDDEN, Json(json!({"ok":false,"need_plus":true,"error":if lang == "en" { "This requires WIRING+" } else { "это WIRING+" }}))).into_response(),
        Outcome::Limited => error(StatusCode::TOO_MANY_REQUESTS, "Too many attempts", "слишком много попыток"),
        Outcome::NoCode => error(StatusCode::BAD_REQUEST, "Enter a code", "нужен код"),
        Outcome::UnknownCode => error(StatusCode::BAD_REQUEST, "Code not found", "нет такого кода"),
        Outcome::Spent => error(StatusCode::BAD_REQUEST, "This code has reached its use limit", "код уже израсходован"),
        Outcome::Used => error(StatusCode::BAD_REQUEST, "You have already used this code", "этот код ты уже вводил(а)"),
        Outcome::NotReal => error(StatusCode::BAD_REQUEST, "Promo codes are available only to real accounts", "промокод доступен только реальным аккаунтам"),
        Outcome::Unsupported => error(StatusCode::BAD_REQUEST, "This promo code is not available yet", "этот промокод пока недоступен"),
        Outcome::Updated { user, until: Some(until) } => Json(json!({"ok":true,"user":user,"plus_until":until})).into_response(),
        Outcome::Updated { user, until: None } => Json(json!({"ok":true,"user":user})).into_response(),
    })
}

pub(crate) fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false, Value::Bool(v) => *v, Value::Number(n) => n.as_f64() != Some(0.0),
        Value::String(s) => !s.is_empty(), Value::Array(a) => !a.is_empty(), Value::Object(o) => !o.is_empty(),
    }
}
