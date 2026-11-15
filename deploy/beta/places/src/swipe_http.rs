use std::sync::Arc;

use axum::{Json,http::StatusCode,response::{IntoResponse,Response}};
use serde_json::{Value,json};

use crate::{chat_write::Writer,feed_write::{Decision,Direction,Swipe,SwipeResult,apply_swipe},
    message_send::{Delivery,demo,integer,take},profile_read::ProfileRules};

enum Outcome { Missing,Banned,NoTarget,SelfTarget,BadDirection,Blocked,NotFound,Premium,
    AlreadyMatched(Value),Snoozed(i64),Applied{data:Value,push:Option<(i64,&'static str,String)>} }

pub(crate) async fn serve(db:&Writer,delivery:&Delivery,profile:Arc<ProfileRules>,uid:i64,fields:Value,now:i64,lang:&'static str)
    ->Result<Response,StatusCode>
{
    let account=db.transaction(move |tx|Box::pin(async move {
        let row=tx.query_opt("SELECT name,email,COALESCE(report_banned_at,0)!=0 FROM users WHERE id=$1::bigint AND COALESCE(deleted_at,0)=0", &[&uid]).await?;
        let Some(row)=row else {return Ok(None);};
        let banned:bool=row.get(2);
        if !banned {tx.execute("UPDATE users SET last_seen=$2::bigint WHERE id=$1::bigint", &[&uid,&now]).await?;}
        Ok(Some((row.get::<_,String>(0),row.get::<_,String>(1),banned)))
    })).await?.0;
    let result=match account {
        None=>Outcome::Missing,Some((_,_,true))=>Outcome::Banned,
        Some((name,email,false))=>{
            let direction=match fields["direction"].as_str() {
                Some("like")=>Some(Direction::Like),Some("pass")=>Some(Direction::Pass),Some("snooze")=>Some(Direction::Snooze),_=>None,
            };
            match (integer(&fields["target_id"]),direction) {
                (None,_)=>Outcome::NoTarget,(_,None)=>Outcome::BadDirection,(Some(target),_) if target==uid=>Outcome::SelfTarget,
                (Some(target),Some(direction))=>{
                    let like=matches!(direction,Direction::Like);
                    db.transaction(move |tx|Box::pin(async move {
                        let pair:[&(dyn tokio_postgres::types::ToSql+Sync);2]=[&uid,&target];
                        crate::chat_write::lock_pair(tx,&pair).await?;
                        if crate::chat_write::pair_access(tx,&pair).await?.0 {return Ok(Outcome::Blocked);}
                        let target_row=tx.query_opt("SELECT email,language,COALESCE(is_seed,0)!=0,COALESCE(notify_enabled,0)!=0,COALESCE(notify_push,0)!=0
                            FROM users WHERE id=$1::bigint AND COALESCE(deleted_at,0)=0", &[&target]).await?;
                        let Some(target_row)=target_row else {return Ok(Outcome::NotFound);};
                        let seed=target_row.get::<_,bool>(2);
                        let seed_direction=if seed && like {
                            let seed_card=profile.card(tx,target,now,lang).await?;
                            let actor_card=profile.card(tx,uid,now,lang).await?;
                            Some(if seed_likes(&seed_card,&actor_card) {Decision::Like} else {Decision::Pass})
                        } else {None};
                        let result=apply_swipe(tx,Swipe{uid,target_id:target,now,direction,seed_direction}).await?;
                        Ok(match result {
                            SwipeResult::Blocked=>Outcome::Blocked,SwipeResult::NotFound=>Outcome::NotFound,
                            SwipeResult::PremiumRequired=>Outcome::Premium,SwipeResult::Snoozed{until}=>Outcome::Snoozed(until),
                            SwipeResult::AlreadyMatched=>Outcome::AlreadyMatched(profile.card(tx,target,now,lang).await?),
                            SwipeResult::Applied{matched,was_match,liked}=>{
                                let card=if matched {profile.card(tx,target,now,lang).await?} else {Value::Null};
                                let target_email:&str=target_row.get(0); let target_lang:&str=target_row.get(1);
                                if matched && !was_match {
                                    tx.execute("UPDATE notifications SET \"read\"=1
                                        WHERE kind='like' AND \"read\"=0 AND ((user_id=$1 AND from_id=$2) OR (user_id=$2 AND from_id=$1))",
                                        &pair).await?;
                                }
                                let mut push=None;
                                if like && !(matched&&was_match) && !seed && !demo(target_email) && target_row.get::<_,bool>(3) {
                                    let kind=if matched {"match"} else {"like"};
                                    let notice=match (matched,target_lang=="en") {
                                        (true,true)=>format!("You and {name} liked each other"),(true,false)=>format!("взаимно с {name}"),
                                        (false,true)=>format!("{name} liked you"),(false,false)=>format!("{name} лайкнул(а) тебя"),
                                    };
                                    let clipped=take(&notice,280);
                                    tx.execute("INSERT INTO notifications(user_id,kind,from_id,body,created_at,\"read\") VALUES($1::bigint,$2,$3::bigint,$4,$5::bigint,0)",
                                        &[&target,&kind,&uid,&clipped,&now]).await?;
                                    if target_row.get::<_,bool>(4) {push=Some((target,kind,notice));}
                                }
                                Outcome::Applied{data:json!({"ok":true,"matched":matched,"match":card,"guest_nudge":
                                    (email.ends_with("@wiring.guest")||email=="demo@wiring.app")&&liked>=3}),push}
                            }
                        })
                    })).await?.0
                }
            }
        }
    };
    let error=|status,en,ru|(status,Json(json!({"ok":false,"error":if lang=="en" {en} else {ru}}))).into_response();
    Ok(match result {
        Outcome::Missing=>crate::account_settings::signed_out(lang),Outcome::Banned=>crate::account_settings::report_ban_response(lang),
        Outcome::NoTarget=>error(StatusCode::BAD_REQUEST,"Missing target","нет цели"),
        Outcome::SelfTarget=>error(StatusCode::BAD_REQUEST,"This is you","это ты"),
        Outcome::BadDirection=>error(StatusCode::BAD_REQUEST,"like, pass or snooze","like, pass или snooze"),
        Outcome::Blocked=>error(StatusCode::FORBIDDEN,"This person is hidden","этот человек скрыт"),
        Outcome::NotFound=>error(StatusCode::NOT_FOUND,"Person not found","человек не найден"),
        Outcome::Premium=>(StatusCode::FORBIDDEN,Json(json!({"ok":false,"need_plus":true,"error":if lang=="en" {"Snoozing requires WIRING+"} else {"отложить — это WIRING+"}}))).into_response(),
        Outcome::AlreadyMatched(card)=>(StatusCode::CONFLICT,Json(json!({"ok":false,"matched":true,"match":card,"error":if lang=="en" {
            "This is already a mutual like. You can remove it only from the chat."} else {"это уже взаимный лайк — убрать можно только из чата"}}))).into_response(),
        Outcome::Snoozed(until)=>Json(json!({"ok":true,"matched":false,"match":Value::Null,"snoozed_until":until})).into_response(),
        Outcome::Applied{data,push}=>{if let Some((to,kind,notice))=push {delivery.push(db,to,uid,kind,&notice,now).await;}Json(data).into_response()}
    })
}

fn seed_likes(seed:&Value,candidate:&Value)->bool {
    let looking=|a:&Value,b:&Value|matches!(b["gender"].as_str(),Some("other"|"hidden")) || match a["looking_for"].as_str() {
        Some("men")=>b["gender"]=="man",Some("women")=>b["gender"]=="woman",_=>true,
    };
    if !looking(seed,candidate)||!looking(candidate,seed) {return false;}
    let overlap=|key:&str|seed[key].as_array().zip(candidate[key].as_array()).map_or(0,|(left,right)|left.iter().filter(|v|right.contains(v)).count()) as i32;
    let age=seed["age"].as_i64().unwrap_or_default().abs_diff(candidate["age"].as_i64().unwrap_or_default());
    let same_city=seed["city"].as_str().unwrap_or("").to_lowercase()==candidate["city"].as_str().unwrap_or("").to_lowercase();
    let first=seed["intent"].as_str().unwrap_or("dating"); let second=candidate["intent"].as_str().unwrap_or("dating");
    let intent=if first==second && matches!(first,"relationship"|"friends") {1}
        else if matches!((first,second),("relationship","friends")|("friends","relationship")) {-1} else {0};
    (overlap("neuro")*2).min(4)+overlap("vibe").min(3)+if age<=6 {1} else if age>12 {-1} else {0}+
        i32::from(same_city)*2+intent+i32::from([seed,candidate].iter().all(|p|p["communication"].as_str().is_some_and(|s|!s.is_empty())))>=3
}
