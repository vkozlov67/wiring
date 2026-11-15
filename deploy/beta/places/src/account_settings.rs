use std::sync::Arc;

use axum::{Json,http::StatusCode,response::{IntoResponse,Response}};
use serde_json::{Value,json};

use crate::{chat_write::Writer,profile_read::ProfileRules};

pub(crate) fn signed_out(lang:&str)->Response {
    error(StatusCode::UNAUTHORIZED,lang,"Please sign in","нужна сессия")
}
pub(crate) fn report_ban_response(lang: &str) -> Response {
    let text = if lang == "en" { "Your account was hidden after reports from two different members. If you think this is a mistake, contact support for a manual review." }
        else { "Аккаунт скрыт после жалоб от двух разных участников. Если считаешь, что это ошибка, напиши в поддержку — разберём вручную." };
    (StatusCode::FORBIDDEN, Json(json!({"ok":false,"error":text,"report_banned":true}))).into_response()
}
fn error(status:StatusCode,lang:&str,en:&str,ru:&str)->Response {
    (status,Json(json!({"ok":false,"error":if lang=="en" {en} else {ru}}))).into_response()
}

enum Outcome {
    Missing,
    Banned,
    Guest,
    Locked,
    Disabled,
    BadField,
    Changed(Value,Option<&'static str>),
}

pub(crate) async fn update(db:&Writer,profile:Arc<ProfileRules>,uid:i64,path:&str,fields:Value,now:i64,lang:&'static str)
    ->Result<(Response,Option<&'static str>),StatusCode>
{
    let kind=match path {
        "/api/me/language"=>0, "/api/me/theme"=>1, "/api/me/consents"=>2, _=>3,
    };
    let configured=std::env::var("WIRING_JEV_CONFIGURED").as_deref()==Ok("1");
    let result=db.transaction(move |tx|Box::pin(async move {
        let row=tx.query_opt("SELECT COALESCE(report_banned_at,0)!=0,email,is_seed!=0,jev_recommendations_unlocked!=0
            FROM users WHERE id=$1::bigint FOR UPDATE",&[&uid]).await?;
        let Some(row)=row else {return Ok(Outcome::Missing);};
        if kind>=2 && row.get::<_,bool>(0) {return Ok(Outcome::Banned);}
        tx.execute("UPDATE users SET last_seen=$2::bigint WHERE id=$1::bigint",&[&uid,&now]).await?;
        match kind {
            0=>{
                let language=match fields["language"].as_str() {
                    Some("ru")=>"ru",Some("en")=>"en",_=>return Ok(Outcome::BadField),
                };
                tx.execute("UPDATE users SET language=$2 WHERE id=$1::bigint",&[&uid,&language]).await?;
                return Ok(Outcome::Changed(json!({"ok":true,"language":language}),Some(language)));
            }
            1=>{
                let Some(theme)=fields["theme"].as_str().filter(|s|matches!(*s,"pastel"|"mist"|"dusk"|"night"|"slate"))
                else {return Ok(Outcome::BadField);};
                tx.execute("UPDATE users SET theme=$2 WHERE id=$1::bigint",&[&uid,&theme]).await?;
                return Ok(Outcome::Changed(json!({"ok":true,"theme":theme}),None));
            }
            2=>{
                let yes=|value:&Value| value==&Value::Bool(true) || value.as_f64()==Some(1.0)
                    || value.as_str().is_some_and(|s|matches!(s,"1"|"true"|"True"|"yes"|"on"));
                let special=yes(&fields["special_data_consent"]).then_some(now);
                let photo=yes(&fields["photo_rights_consent"]).then_some(now);
                tx.execute("UPDATE users SET special_data_consent_at=$2::bigint,
                    photo_rights_consent_at=CASE WHEN $3 THEN $4::bigint ELSE photo_rights_consent_at END
                    WHERE id=$1::bigint",&[&uid,&special,&fields.get("photo_rights_consent").is_some(),&photo]).await?;
            }
            _=>{
                let email:&str=row.get(1);
                if email.ends_with("@wiring.guest") || email=="demo@wiring.app" {return Ok(Outcome::Guest);}
                if row.get::<_,bool>(2) || !row.get::<_,bool>(3) {return Ok(Outcome::Locked);}
                let Some(enabled)=fields["enabled"].as_bool() else {return Ok(Outcome::BadField);};
                if enabled && !configured {return Ok(Outcome::Disabled);}
                tx.execute("UPDATE users SET jev_feed_enabled=$2 WHERE id=$1::bigint",&[&uid,&i32::from(enabled)]).await?;
            }
        }
        let (profile,_)=profile.read(tx,uid,now,lang).await?;
        Ok(Outcome::Changed(json!({"ok":true,"user":profile}),None))
    })).await?.0;
    let response=match result {
        Outcome::Changed(result,language)=>return Ok((Json(result).into_response(),language)),
        Outcome::Missing=>error(StatusCode::UNAUTHORIZED,lang,"No profile","нет профиля"),
        Outcome::Banned=>report_ban_response(lang),
        Outcome::Guest=>error(StatusCode::FORBIDDEN,lang,"Complete your profile first","сначала собери свой профиль"),
        Outcome::Locked=>error(StatusCode::NOT_FOUND,lang,"Activate the recommendations promo code first","сначала активируй промокод для рекомендаций"),
        Outcome::Disabled=>error(StatusCode::SERVICE_UNAVAILABLE,lang,"Recommendations are unavailable right now","подбор пока недоступен"),
        Outcome::BadField=>{
            let (en,ru)=match kind {0=>("Unknown language","Неизвестный язык"),1=>("Unknown theme","неизвестная тема"),
                _=>("Specify whether the setting is enabled","укажи состояние переключателя")};
            error(StatusCode::BAD_REQUEST,lang,en,ru)
        }
    };
    Ok((response,None))
}

pub(crate) async fn pair(db:&Writer,uid:i64,block:bool,fields:Value,now:i64,lang:&'static str)->Result<Response,StatusCode> {
    use crate::chat_pair::PairOutcome;
    enum ResultCode { Missing, Banned, NoTarget, SelfBlock, Pair(PairOutcome) }
    let target=match &fields["user_id"] {
        Value::String(value)=>value.trim().parse::<i64>().ok(),
        Value::Number(value)=>value.as_i64().or_else(||value.as_f64().filter(|v|
            (-9_223_372_036_854_775_808.0..9_223_372_036_854_775_808.0).contains(v)).map(|v|v as i64)),
        Value::Bool(value)=>Some(i64::from(*value)),
        _=>None,
    };
    let result=db.transaction(move |tx|Box::pin(async move {
        let row=tx.query_opt("SELECT COALESCE(report_banned_at,0)!=0 FROM users
            WHERE id=$1::bigint AND COALESCE(deleted_at,0)=0",&[&uid]).await?;
        let Some(row)=row else {return Ok(ResultCode::Missing);};
        if row.get::<_,bool>(0) {return Ok(ResultCode::Banned);}
        let Some(target)=target else {return Ok(ResultCode::NoTarget);};
        if block && target==uid {return Ok(ResultCode::SelfBlock);}
        Ok(ResultCode::Pair(crate::chat_pair::hide_pair(tx,uid,target,now,block).await?))
    })).await?.0;
    Ok(match result {
        ResultCode::Missing=>signed_out(lang),
        ResultCode::Banned=>report_ban_response(lang),
        ResultCode::NoTarget=>error(StatusCode::BAD_REQUEST,lang,"Person not found","нет человека"),
        ResultCode::SelfBlock=>error(StatusCode::BAD_REQUEST,lang,"This is you","это ты"),
        ResultCode::Pair(PairOutcome::NotFound)=>error(StatusCode::NOT_FOUND,lang,"Person not found","человек не найден"),
        ResultCode::Pair(PairOutcome::NoMatch)=>error(StatusCode::BAD_REQUEST,lang,
            "You can unmatch only after a mutual like","размэтч возможен только после взаимного лайка"),
        ResultCode::Pair(PairOutcome::Hidden)=>Json(json!({"ok":true})).into_response(),
        ResultCode::Pair(PairOutcome::Reported{..})=>unreachable!(),
    })
}
