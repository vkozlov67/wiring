use std::sync::Arc;

use axum::{Json, http::{Method, StatusCode}, response::{IntoResponse, Response}};
use serde_json::{Value, json};
use tokio_postgres::Transaction;

use crate::{chat_write::{TransactionResult, Writer}, profile_read::{ProfileRules, profile_albums, profile_photos}};

enum Account { Missing, Banned, Guest, Member }

async fn account(tx: &Transaction<'_>, uid: i64, now: i64) -> TransactionResult<Account> {
    let row = tx.query_opt("SELECT email, COALESCE(report_banned_at,0)!=0 FROM users
        WHERE id=$1::bigint AND COALESCE(deleted_at,0)=0", &[&uid]).await?;
    let Some(row) = row else { return Ok(Account::Missing); };
    if row.get::<_, bool>(1) { return Ok(Account::Banned); }
    tx.execute("UPDATE users SET last_seen=$2::bigint WHERE id=$1::bigint", &[&uid, &now]).await?;
    let email: &str = row.get(0);
    Ok(if email.ends_with("@wiring.guest") || email == "demo@wiring.app" { Account::Guest } else { Account::Member })
}

enum Outcome { Missing, Banned, Guest, NoAlbums, Title, NotFound, Albums(Vec<Value>) }

pub(crate) async fn serve(db: &Writer, uid: i64, method: &Method, id: Option<i64>, fields: Value, now: i64, lang: &str)
    -> Result<Response, StatusCode>
{
    let list = matches!(*method, Method::GET | Method::HEAD);
    let create = *method == Method::POST;
    let rename = *method == Method::PATCH;
    let title: String = fields["title"].as_str().unwrap_or("").trim().chars().take(32).collect();
    let result = db.transaction(move |tx| Box::pin(async move {
        match account(tx, uid, now).await? {
            Account::Missing => return Ok(Outcome::Missing),
            Account::Banned => return Ok(Outcome::Banned),
            Account::Guest if !list => return Ok(Outcome::Guest),
            _ => (),
        }
        if create { return Ok(Outcome::NoAlbums); }
        if !list {
            if rename && title.is_empty() { return Ok(Outcome::Title); }
            let Some(id) = id else { return Ok(Outcome::NotFound); };
            if rename {
                let updated = tx.execute("UPDATE albums SET title=$3 WHERE id=$1::bigint AND user_id=$2::bigint", &[&id, &uid, &title]).await?;
                if updated == 0 { return Ok(Outcome::NotFound); }
            } else {
                if tx.query_opt("SELECT id FROM albums WHERE id=$1::bigint AND user_id=$2::bigint FOR UPDATE", &[&id, &uid]).await?.is_none() {
                    return Ok(Outcome::NotFound);
                }
                let fallback = tx.query_opt("SELECT id::bigint FROM albums WHERE user_id=$1::bigint AND title='я' ORDER BY id LIMIT 1", &[&uid]).await?;
                let fallback = match fallback.map(|row| row.get::<_, i64>(0)) {
                    Some(current) if current == id => tx.query_opt("SELECT id::bigint FROM albums
                        WHERE user_id=$1::bigint AND id<>$2::bigint ORDER BY id LIMIT 1", &[&uid, &id]).await?.map(|row| row.get::<_,i64>(0)),
                    Some(other) => Some(other),
                    None => Some(tx.query_one("INSERT INTO albums(user_id,title,sort_order,created_at)
                        SELECT $1::bigint,'я',count(*)::integer,$2::bigint FROM albums WHERE user_id=$1::bigint RETURNING id::bigint", &[&uid,&now]).await?.get(0)),
                };
                tx.execute("UPDATE photos SET album_id=$3::bigint WHERE user_id=$1::bigint AND album_id=$2::bigint", &[&uid, &id, &fallback]).await?;
                tx.execute("DELETE FROM albums WHERE id=$1::bigint AND user_id=$2::bigint", &[&id,&uid]).await?;
            }
        }
        let photos = profile_photos(tx, uid).await?;
        Ok(Outcome::Albums(profile_albums(tx, uid, &photos).await?))
    })).await?.0;
    let error = |status, en, ru| (status, Json(json!({"ok":false,"error":if lang == "en" { en } else { ru }}))).into_response();
    Ok(match result {
        Outcome::Missing => crate::account_settings::signed_out(lang),
        Outcome::Banned => crate::account_settings::report_ban_response(lang),
        Outcome::Guest => error(StatusCode::FORBIDDEN, "Complete your profile first", "сначала собери свой профиль"),
        Outcome::NoAlbums => error(StatusCode::BAD_REQUEST, "one photo set, without albums", "один набор фото, без альбомов"),
        Outcome::Title => error(StatusCode::BAD_REQUEST, "Enter a title", "нужно название"),
        Outcome::NotFound => error(StatusCode::NOT_FOUND, "Album not found", "альбом не найден"),
        Outcome::Albums(albums) => Json(json!({"ok":true,"albums":albums})).into_response(),
    })
}

pub(crate) async fn skip(db: &Writer, profile: Arc<ProfileRules>, uid: i64, now: i64, lang: &'static str)
    -> Result<Response, StatusCode>
{
    let (account, user) = db.transaction(move |tx| Box::pin(async move {
        let account = account(tx, uid, now).await?;
        let user = if matches!(account, Account::Guest | Account::Member) { profile.read(tx, uid, now, lang).await?.0 } else { Value::Null };
        Ok((account, user))
    })).await?.0;
    Ok(match account {
        Account::Missing => crate::account_settings::signed_out(lang),
        Account::Banned => crate::account_settings::report_ban_response(lang),
        _ => Json(json!({"ok":true,"user":user})).into_response(),
    })
}
