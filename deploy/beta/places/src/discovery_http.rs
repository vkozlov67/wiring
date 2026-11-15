use std::{collections::{HashMap,HashSet},sync::Arc};

use axum::{Json,http::{StatusCode,header},response::{IntoResponse,Response}};
use serde_json::{Value,json};
use sha1::{Digest,Sha1};
use tokio_postgres::{Row,Transaction};

use crate::{chat_read::inbox_counts,chat_write::{TransactionResult,Writer},feed_write::{FeedClaim,claim_delivery},
    places_http::PublicPlaces,profile_read::ProfileRules};

struct Filters { neuro:Vec<String>, vibe:Vec<String>, intents:Vec<String>, city:String, country:String,
    unknown:bool, age_min:i32, age_max:i32, gender:String, skip:Vec<i64>, limit:i64, real:bool, hide_empty:bool, narrowed:bool }

impl Filters {
    fn new(query:&str, places:&PublicPlaces, profile:&ProfileRules, feed:bool) -> Self {
        let mut input=HashMap::new();
        for (key,value) in form_urlencoded::parse(query.as_bytes()) { input.entry(key).or_insert(value); }
        let text=|key:&str|input.get(key).map_or("",|value|value.as_ref());
        let int=|key:&str,default:i64|text(key).trim().parse::<i64>().ok().filter(|value|*value!=0).unwrap_or(default);
        let list=|kind:&str|text(kind).split(',').filter(|value|profile.filter_keys(kind).contains(*value)).map(str::to_owned).collect::<Vec<_>>();
        let neuro=list("neuro"); let vibe=list("vibe"); let intents=list("intent");
        let (mut age_min,mut age_max)=(int("min_age",18).clamp(18,99) as i32,int("max_age",99).clamp(18,99) as i32);
        if age_min>age_max { std::mem::swap(&mut age_min,&mut age_max); }
        let raw_city=text("city").trim();
        let city=places.qualified_country(raw_city).map(|_|raw_city.to_owned())
            .or_else(||places.known(raw_city).map(|(city,_)|city)).unwrap_or_default();
        let country=text("country").trim().to_owned();
        let gender=text("gender").trim();
        let gender=if feed && matches!(gender,"woman"|"man") { gender.to_owned() } else { String::new() };
        let skip=if feed { text("skip").split(',').filter(|p|!p.is_empty() && p.bytes().all(|b|b.is_ascii_digit()))
            .filter_map(|id|id.parse::<i64>().ok()).collect::<HashSet<_>>().into_iter().take(200).collect()
        } else { Vec::new() };
        let narrowed=!raw_city.is_empty() || !country.is_empty() || !gender.is_empty() || !neuro.is_empty() || !vibe.is_empty()
            || !intents.is_empty() || age_min!=18 || age_max!=99;
        Self { neuro,vibe,intents,unknown:!raw_city.is_empty() && city.is_empty(),city,country,age_min,age_max,gender,skip,
            limit:int("limit",30).clamp(1,30),real:feed && matches!(text("real"),"1"|"true"|"yes"),
            hide_empty:!matches!(text("hide_empty").to_ascii_lowercase().as_str(),"0"|"false"|"no"),narrowed }
    }
}

enum Outcome { SignedOut,Banned,Data(Value) }

pub(crate) async fn read(db:&Writer, places:&PublicPlaces, profile:Arc<ProfileRules>, uid:i64, feed:bool, query:&str, now:i64, lang:&'static str)
    -> Result<Response,StatusCode>
{
    let filters=Filters::new(query,places,&profile,feed);
    let viewer=db.transaction(move |tx|Box::pin(async move {
        let row=tx.query_opt("SELECT email,COALESCE(report_banned_at,0)!=0,COALESCE(is_seed,0)!=0,COALESCE(premium_until,0)>$2::bigint,
            COALESCE(age,0),COALESCE(city,''),COALESCE(gender,''),COALESCE(looking_for,'everyone') FROM users
            WHERE id=$1::bigint AND COALESCE(deleted_at,0)=0", &[&uid,&now]).await?;
        let Some(row)=row else { return Ok(None); };
        if !row.get::<_,bool>(1) { tx.execute("UPDATE users SET last_seen=$2::bigint WHERE id=$1::bigint", &[&uid,&now]).await?; }
        Ok(Some(row))
    })).await?.0;
    let outcome=if let Some(viewer)=viewer {
        if viewer.get::<_,bool>(1) { Outcome::Banned } else {
            let country_index=places.country_index();
            db.transaction(move |tx|Box::pin(async move {
                let viewer_email:&str=viewer.get(0);
                let qa=viewer_email.trim().eq_ignore_ascii_case("wiringremlin@gmail.com");
                let plus:bool=viewer.get(3);
                let include_delivered=qa || filters.narrowed;
                if !guest(viewer_email) && !viewer.get::<_,bool>(2) { track(tx,uid,feed,&filters,now).await?; }
                let rows=if filters.unknown { Vec::new() } else {
                    candidates(tx,uid,feed,plus,include_delivered,&filters,now).await?
                };
                let viewer_city:&str=viewer.get(5);
                let viewer_country=if feed { country_index.country(viewer_city) } else { "" };
                let viewer_vibes=if feed {
                    profile.tags(Vec::new(),tx.query("SELECT tag FROM user_tags WHERE user_id=$1::bigint AND kind='vibe' ORDER BY tag", &[&uid])
                        .await?.into_iter().map(|row|row.get(0)).collect()).1
                } else { Vec::new() };
                let mut eligible=Vec::new();
                for row in rows {
                    let email:&str=row.get(1);
                    if guest(email) || (!filters.country.is_empty() && country_index.country(row.get(4))!=filters.country) { continue; }
                    let (neuro,vibe)=profile.tags(row.get(15),row.get(16));
                    if (!filters.neuro.is_empty() && !neuro.iter().any(|tag|filters.neuro.contains(tag))) ||
                        (!filters.vibe.is_empty() && !vibe.iter().any(|tag|filters.vibe.contains(tag))) { continue; }
                    if !filters.intents.is_empty() && !intents(row.get(13),&vibe,&profile).iter().any(|tag|filters.intents.contains(tag)) { continue; }
                    if feed {
                        let gender:&str=row.get(5);
                        if filters.real && plus && demo(email) { continue; }
                        if !looking(viewer.get(7),gender) || (!qa && !looking(row.get(6),viewer.get(6))) { continue; }
                        let min=nonzero(row.get(8),18).clamp(18,99); let max=nonzero(row.get(9),99).clamp(18,99);
                        let age:i32=viewer.get(4); let place=row.get::<_,&str>(10).trim(); let hidden:&str=row.get(11);
                        if age<min.min(max) || age>min.max(max) || (!place.is_empty() && place!=viewer_city && place!=viewer_country)
                            || hidden.split(',').any(|tag|viewer_vibes.iter().any(|v|v==tag)) { continue; }
                        let name:&str=row.get(2);
                        if row.get::<_,bool>(7) || !(2..=32).contains(&name.trim().chars().count()) ||
                            matches!(name.trim().to_lowercase().as_str(),"участник"|"participant") || !profile.gender_ok(gender) ||
                            (!neuro.is_empty() && !row.get::<_,bool>(12)) || (filters.hide_empty && neuro.is_empty()) { continue; }
                    }
                    eligible.push((row.get::<_,i64>(0),row.get::<_,i64>(14)));
                }
                if !feed {
                    let mut likes=Vec::with_capacity(eligible.len());
                    for (id,liked_at) in eligible {
                        if plus {
                            let mut card=profile.person(tx,id,now,lang,false).await?;
                            if card.is_null() { continue; }
                            card["matched"]=false.into(); card["liked_at"]=liked_at.into(); likes.push(card);
                        } else { likes.push(json!({"hidden":true,"matched":false})); }
                    }
                    return Ok(Outcome::Data(json!({"ok":true,"likes":likes,"plus":plus})));
                }
                let page=claim_delivery(tx,FeedClaim { uid,now,limit:filters.limit,include_delivered,recover_unviewed:true,
                    candidates:eligible.into_iter().map(|(id,_)|id).collect() }).await?;
                let mut cards=Vec::with_capacity(page.ids.len());
                for id in page.ids { let card=profile.card(tx,id,now,lang).await?; if !card.is_null() { cards.push(card); } }
                let row=tx.query_one("SELECT count(*) FILTER(WHERE direction='like'),count(*) FILTER(WHERE direction='pass') FROM swipes WHERE from_id=$1::bigint", &[&uid]).await?;
                let counts=inbox_counts(tx,uid).await?;
                Ok(Outcome::Data(json!({"ok":true,"cards":cards,"has_more":page.has_more,"generation":page.generation,
                    "recycled":false,"jev_ranked":false,"jev_scores":"none","unseen":cards.len(),"liked":row.get::<_,i64>(0),"passed":row.get::<_,i64>(1),
                    "likes_in":counts.likes_in,"unread":counts.unread,"unread_chats":counts.unread_chats})))
            })).await?.0
        }
    } else { Outcome::SignedOut };
    Ok(match outcome {
        Outcome::SignedOut=>crate::account_settings::signed_out(lang),
        Outcome::Banned=>crate::account_settings::report_ban_response(lang),
        Outcome::Data(data) if feed=>([(header::CACHE_CONTROL,"no-store")],Json(data)).into_response(),
        Outcome::Data(data)=>Json(data).into_response(),
    })
}

async fn candidates(tx:&Transaction<'_>, uid:i64, feed:bool, plus:bool, delivered:bool, filters:&Filters, now:i64)
    -> TransactionResult<Vec<Row>>
{
    Ok(tx.query("SELECT u.id::bigint,u.email,u.name,u.age,u.city,COALESCE(u.gender,''),COALESCE(u.looking_for,'everyone'),
            COALESCE(u.profile_setup_required,0)!=0,u.seek_min_age,u.seek_max_age,COALESCE(u.seek_place,''),COALESCE(u.hide_tags,''),
            COALESCE(u.special_data_consent_at,0)>0,COALESCE(u.intent,''),COALESCE(incoming.created_at,0)::bigint,
            ARRAY(SELECT t.tag FROM user_tags t WHERE t.user_id=u.id AND t.kind='neuro' ORDER BY t.tag),
            ARRAY(SELECT t.tag FROM user_tags t WHERE t.user_id=u.id AND t.kind='vibe' ORDER BY t.tag)
        FROM users u LEFT JOIN swipes incoming ON incoming.from_id=u.id AND incoming.to_id=$1::bigint AND incoming.direction='like'
        WHERE u.id<>$1 AND COALESCE(u.deleted_at,0)=0 AND COALESCE(u.is_seed,0)=0
          AND ($2 OR incoming.from_id IS NOT NULL) AND u.age BETWEEN $3 AND $4 AND ($5='' OR u.city=$5)
          AND ($6='' OR u.gender=$6) AND (NOT $2 OR NOT(u.id=ANY($7::bigint[])))
          AND NOT EXISTS(SELECT 1 FROM swipes s WHERE s.from_id=$1 AND s.to_id=u.id)
          AND NOT EXISTS(SELECT 1 FROM blocks b WHERE (b.from_id=$1 AND b.to_id=u.id) OR (b.from_id=u.id AND b.to_id=$1))
          AND (NOT $2 OR (COALESCE(u.paused,0)=0 AND COALESCE(u.report_banned_at,0)=0
               AND (COALESCE(u.incognito,0)=0 OR incoming.from_id IS NOT NULL)
               AND NOT EXISTS(SELECT 1 FROM feed_history h WHERE h.user_id=$1 AND h.other_id=u.id AND (h.excluded_at IS NOT NULL OR (NOT $8 AND h.viewed_at IS NOT NULL)))
               AND (NOT $9 OR NOT EXISTS(SELECT 1 FROM snoozes s WHERE s.user_id=$1 AND s.other_id=u.id AND s.until_at>$10::bigint))))
          AND (COALESCE(u.photo,'')<>'' OR EXISTS(SELECT 1 FROM photos p WHERE p.user_id=u.id))
        ORDER BY incoming.created_at DESC", &[&uid,&feed,&filters.age_min,&filters.age_max,&filters.city,&filters.gender,&filters.skip,&delivered,&plus,&now]).await?)
}

async fn track(tx:&Transaction<'_>, uid:i64, feed:bool, filters:&Filters, now:i64) -> TransactionResult<()> {
    let source=if feed {"feed"} else {"likes"};
    let normalize=|values:&[String]| { let mut out=values.to_vec(); out.sort(); out.dedup(); out };
    let (neuro,vibe,intents)=(normalize(&filters.neuro),normalize(&filters.vibe),normalize(&filters.intents));
    let age=filters.age_min>18 || filters.age_max<99;
    if neuro.is_empty() && vibe.is_empty() && intents.is_empty() && filters.city.is_empty() && !age && !filters.real { return Ok(()); }
    let bounds=if age {format!("{}-{}",filters.age_min,filters.age_max)} else {String::new()};
    let key=format!("{source}|n:{}|v:{}|i:{}|c:{}|a:{bounds}|r:{}",neuro.join(","),vibe.join(","),intents.join(","),filters.city,i32::from(filters.real));
    let fingerprint:String=Sha1::digest(key).iter().take(12).map(|byte|format!("{byte:02x}")).collect();
    if tx.query_opt("SELECT 1 FROM filter_events WHERE user_id=$1::bigint AND fingerprint=$2 AND created_at>=$3::bigint LIMIT 1",
        &[&uid,&fingerprint,&(now-1800)]).await?.is_some() { return Ok(()); }
    let mut rows=Vec::new();
    for (kind,values) in [("neuro",neuro),("vibe",vibe),("intent",intents)] { for value in values { rows.push((kind,value)); } }
    if !filters.city.is_empty() { rows.push(("city",filters.city.chars().take(64).collect())); }
    if age { rows.push(("age",format!("{}–{}",filters.age_min,filters.age_max))); }
    if filters.real { rows.push(("real","1".into())); }
    for (kind,value) in rows {
        tx.execute("INSERT INTO filter_events(user_id,source,fingerprint,kind,value,created_at) VALUES($1::bigint,$2,$3,$4,$5,$6::bigint)",
            &[&uid,&source,&fingerprint,&kind,&value,&now]).await?;
    }
    Ok(())
}

fn intents(raw:&str,vibe:&[String],profile:&ProfileRules) -> Vec<String> {
    let items:Vec<String>=if raw.trim_start().starts_with('[') { serde_json::from_str(raw).unwrap_or_default() }
        else { raw.split([',',';','|']).map(|s|s.trim().to_owned()).collect() };
    let mut out:Vec<_>=items.into_iter().filter(|id|profile.filter_keys("intent").contains(id)).collect();
    if out.is_empty() { out.push("dating".into()); }
    if vibe.iter().any(|v|v=="bodydouble") && !out.iter().any(|v|v=="bodydouble") { out.push("bodydouble".into()); }
    out
}
fn guest(email:&str)->bool { email.ends_with("@wiring.guest") || email=="demo@wiring.app" }
fn demo(email:&str)->bool { let email=email.trim().to_lowercase(); guest(&email) || ["@example.com","@wiring.demo","@wiring.app"].iter().any(|s|email.ends_with(s)) }
fn nonzero(value:Option<i32>,default:i32)->i32 { value.filter(|v|*v!=0).unwrap_or(default) }
fn looking(preference:&str,gender:&str)->bool { matches!(gender,"hidden"|"other"|"nb") || match preference { "women"=>gender=="woman", "men"=>gender=="man", _=>true } }
