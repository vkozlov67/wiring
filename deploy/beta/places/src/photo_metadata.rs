use std::collections::HashSet;

use axum::{Json,http::StatusCode,response::{IntoResponse,Response}};
use serde_json::{Value,json};

use crate::{chat_write::Writer, premium_http::truthy, profile_read::{profile_albums,profile_photos}};

enum Outcome { SignedOut, Banned, Guest, InvalidOrder, ChangedOrder, PhotoMissing, InvalidAlbum, AlbumMissing, Done(Value) }

pub(crate) async fn delete(db:&Writer, media:&crate::media_http::Media,uid:i64,id:Option<i64>,now:i64,lang:&str)
    -> Result<Response,StatusCode>
{
    enum ResultCode { SignedOut,Banned,Guest,Missing,Last,Deleted(String,Value) }
    let outcome=db.transaction(move |tx| Box::pin(async move {
        let row=tx.query_opt("SELECT email,COALESCE(report_banned_at,0)!=0 FROM users WHERE id=$1::bigint
            AND COALESCE(deleted_at,0)=0 FOR UPDATE", &[&uid]).await?;
        let Some(row)=row else {return Ok(ResultCode::SignedOut);};
        if row.get::<_,bool>(1) {return Ok(ResultCode::Banned);}
        tx.execute("UPDATE users SET last_seen=$2::bigint WHERE id=$1::bigint", &[&uid,&now]).await?;
        let email:&str=row.get(0);
        if email.ends_with("@wiring.guest")||email=="demo@wiring.app" {return Ok(ResultCode::Guest);}
        let Some(id)=id else {return Ok(ResultCode::Missing);};
        let rows=tx.query("SELECT id::bigint,path FROM photos WHERE user_id=$1::bigint ORDER BY sort_order,id FOR UPDATE", &[&uid]).await?;
        let Some(photo)=rows.iter().find(|row|row.get::<_,i64>(0)==id) else {return Ok(ResultCode::Missing);};
        let Some(first)=rows.iter().find(|row|row.get::<_,i64>(0)!=id) else {return Ok(ResultCode::Last);};
        let path:String=photo.get(1);
        let first_id:i64=first.get(0);
        let first_path:&str=first.get(1);
        tx.execute("DELETE FROM photos WHERE id=$1::bigint AND user_id=$2::bigint", &[&id,&uid]).await?;
        tx.execute("UPDATE photos SET is_primary=CASE WHEN id=$2::bigint THEN 1 ELSE 0 END WHERE user_id=$1::bigint", &[&uid,&first_id]).await?;
        tx.execute("UPDATE users SET photo=$2 WHERE id=$1::bigint", &[&uid,&first_path]).await?;
        let photos=profile_photos(tx,uid).await?;
        Ok(ResultCode::Deleted(path,json!({"ok":true,"albums":profile_albums(tx,uid,&photos).await?,"photos":photos})))
    })).await?.0;
    let error=|status,en,ru| (status,Json(json!({"ok":false,"error":if lang=="en"{en}else{ru}}))).into_response();
    Ok(match outcome {
        ResultCode::SignedOut=>crate::account_settings::signed_out(lang),
        ResultCode::Banned=>crate::account_settings::report_ban_response(lang),
        ResultCode::Guest=>error(StatusCode::FORBIDDEN,"Complete your profile first","сначала собери свой профиль"),
        ResultCode::Missing=>error(StatusCode::NOT_FOUND,"Photo not found","фото не найдено"),
        ResultCode::Last=>error(StatusCode::BAD_REQUEST,"Add at least one photo","нужно хотя бы одно фото"),
        ResultCode::Deleted(path,payload)=>{media.erase_profile_upload(uid,&path);Json(payload).into_response()},
    })
}

pub(crate) async fn patch(db:&Writer, uid:i64, photo:&str, fields:Value, now:i64, lang:&str) -> Result<Response,StatusCode> {
    let order = photo=="order";
    let photo = photo.parse::<i64>().ok();
    let result = db.transaction(move |tx|Box::pin(async move {
        let row = tx.query_opt("SELECT email,COALESCE(report_banned_at,0)!=0 FROM users WHERE id=$1::bigint
            AND COALESCE(deleted_at,0)=0 FOR UPDATE", &[&uid]).await?;
        let Some(row) = row else { return Ok(Outcome::SignedOut); };
        if row.get::<_,bool>(1) { return Ok(Outcome::Banned); }
        tx.execute("UPDATE users SET last_seen=$2::bigint WHERE id=$1::bigint", &[&uid,&now]).await?;
        let email:&str = row.get(0);
        if email.ends_with("@wiring.guest") || email=="demo@wiring.app" { return Ok(Outcome::Guest); }
        let mut sync = order;
        if order {
            let Some(ids) = fields["ids"].as_array().and_then(|ids|ids.iter().map(Value::as_i64).collect::<Option<Vec<_>>>())
                else { return Ok(Outcome::InvalidOrder); };
            let unique:HashSet<_> = ids.iter().copied().collect();
            if unique.len()!=ids.len() { return Ok(Outcome::InvalidOrder); }
            let owned:HashSet<i64> = tx.query("SELECT id::bigint FROM photos WHERE user_id=$1::bigint FOR UPDATE", &[&uid]).await?
                .iter().map(|row|row.get(0)).collect();
            if unique!=owned { return Ok(Outcome::ChangedOrder); }
            for (index,id) in ids.iter().enumerate() {
                tx.execute("UPDATE photos SET sort_order=$3,is_primary=$4 WHERE id=$1::bigint AND user_id=$2::bigint",
                    &[id,&uid,&(index as i32),&i32::from(index==0)]).await?;
            }
        } else {
            let Some(photo) = photo else { return Ok(Outcome::PhotoMissing); };
            if tx.query_opt("SELECT id FROM photos WHERE id=$1::bigint AND user_id=$2::bigint FOR UPDATE", &[&photo,&uid]).await?.is_none() {
                return Ok(Outcome::PhotoMissing);
            }
            if let Some(value) = fields.get("album_id").filter(|v|!v.is_null()) {
                let album = match value {
                    Value::String(s)=>s.trim().parse::<i64>().ok(),
                    Value::Number(n)=>n.as_i64().or_else(||n.as_f64().filter(|n|(-9_223_372_036_854_775_808.0..9_223_372_036_854_775_808.0).contains(n)).map(|n|n as i64)),
                    Value::Bool(v)=>Some(i64::from(*v)), _=>None,
                };
                let Some(album) = album else { return Ok(Outcome::InvalidAlbum); };
                if tx.query_opt("SELECT id FROM albums WHERE id=$1::bigint AND user_id=$2::bigint", &[&album,&uid]).await?.is_none() {
                    return Ok(Outcome::AlbumMissing);
                }
                tx.execute("UPDATE photos SET album_id=$3::bigint WHERE id=$1::bigint AND user_id=$2::bigint", &[&photo,&uid,&album]).await?;
            }
            if truthy(&fields["is_primary"]) {
                tx.execute("UPDATE photos SET is_primary=CASE WHEN id=$2::bigint THEN 1 ELSE 0 END WHERE user_id=$1::bigint", &[&uid,&photo]).await?;
                sync = true;
            }
        }
        if sync {
            tx.execute("UPDATE users SET photo=COALESCE((SELECT path FROM photos WHERE user_id=$1::bigint
                ORDER BY is_primary DESC,sort_order,id LIMIT 1),'') WHERE id=$1::bigint", &[&uid]).await?;
        }
        let photos = profile_photos(tx,uid).await?;
        let body = if order { json!({"ok":true,"photos":photos}) }
            else { json!({"ok":true,"albums":profile_albums(tx,uid,&photos).await?,"photos":photos}) };
        Ok(Outcome::Done(body))
    })).await?.0;
    let error = |status,en,ru| (status,Json(json!({"ok":false,"error":if lang=="en" {en} else {ru}}))).into_response();
    Ok(match result {
        Outcome::SignedOut => crate::account_settings::signed_out(lang),
        Outcome::Banned => crate::account_settings::report_ban_response(lang),
        Outcome::Guest => error(StatusCode::FORBIDDEN,"Complete your profile first","сначала собери свой профиль"),
        Outcome::InvalidOrder => error(StatusCode::BAD_REQUEST,"Provide the order of all photos","укажи порядок всех фото"),
        Outcome::ChangedOrder => error(StatusCode::CONFLICT,"Your photo list has changed. Refresh your profile.","список фото изменился, обнови профиль"),
        Outcome::PhotoMissing => error(StatusCode::NOT_FOUND,"Photo not found","фото не найдено"),
        Outcome::InvalidAlbum => error(StatusCode::BAD_REQUEST,"Invalid album","не тот альбом"),
        Outcome::AlbumMissing => error(StatusCode::NOT_FOUND,"Album not found","альбом не найден"),
        Outcome::Done(body) => Json(body).into_response(),
    })
}
