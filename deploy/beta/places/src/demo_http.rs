use std::sync::Arc;

use axum::{Json, http::StatusCode, response::{IntoResponse, Response}};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::json;

use crate::{chat_write::Writer, password::hash_password, profile_read::ProfileRules};

pub(crate) async fn create(db: &Writer, profile: Arc<ProfileRules>, now: i64, lang: &'static str)
    -> Result<(Response, i64), StatusCode>
{
    let mut random = [0u8; 26];
    getrandom::fill(&mut random).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let suffix = random[..8].iter().map(|byte| format!("{byte:02x}")).collect::<String>();
    let email = format!("guest-{suffix}@wiring.guest");
    let hash = hash_password(URL_SAFE_NO_PAD.encode(&random[8..])).await?;
    let (uid, user) = db.transaction(move |tx| Box::pin(async move {
        let uid = tx.query_one("INSERT INTO users (email,password_hash,name,age,city,gender,looking_for,bio,photo,
            email_verified_at,is_seed,created_at,last_seen)
            VALUES ($1,$2,'Гость',28,'онлайн / не важно','other','everyone',
                'Смотрю анкеты. Свой профиль соберу чуть позже.','',$3::bigint,0,$3::bigint,$3::bigint)
            RETURNING id::bigint", &[&email,&hash,&now]).await?.get::<_,i64>(0);
        let (neuro, vibe) = profile.tags(vec!["audhd".into()], vec!["selfdx".into(), "terminally-online".into()]);
        for (kind, tags) in [("neuro",neuro),("vibe",vibe)] {
            for tag in tags {
                tx.execute("INSERT INTO user_tags (user_id,kind,tag) VALUES ($1::bigint,$2,$3)", &[&uid,&kind,&tag]).await?;
            }
        }
        Ok((uid,profile.read(tx,uid,now,lang).await?.0))
    })).await?.0;
    Ok((Json(json!({"ok":true,"user":user})).into_response(),uid))
}
