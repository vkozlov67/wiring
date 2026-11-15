use std::{collections::{BTreeSet,HashMap},env,error::Error,fs,path::Path,sync::Arc,time::Duration};

use axum::{Json,http::{StatusCode,header},response::{IntoResponse,Response}};
use serde_json::{Value,json};
use tokio_postgres::Transaction;

use crate::{chat_write::{TransactionResult,Writer},places_http::{CountryIndex,PublicPlaces},profile_read::ProfileRules};

pub(crate) struct Ranker {
    api:Option<(reqwest::Client,String,String)>, labels:[HashMap<String,String>;2], slots:tokio::sync::Semaphore,
}

enum ResultCode { SignedOut,Banned,Guest,Locked,Ready{viewer:Value,cards:Vec<Value>,taste:bool} }

impl Ranker {
    pub(crate) fn new(directory:&Path)->Result<Self,Box<dyn Error>> {
        let key=env::var("JEV_API_KEY").unwrap_or_default().trim().to_owned();
        let api=if key.is_empty() {None} else {
            let http=reqwest::Client::builder().https_only(true).no_proxy().redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(4)).build()?;
            Some((http,key,env::var("JEV_MODEL").unwrap_or_else(|_|"jev-latest".into()).trim().to_owned()))
        };
        let mut labels=[HashMap::new(),HashMap::new()];
        for (i,lang) in ["ru","en"].into_iter().enumerate() {
            let source:Value=serde_json::from_slice(&fs::read(directory.join(format!("api-catalog-{lang}.json")))?)?;
            for group in ["neuro","vibe","intents"] { if let Some(rows)=source[group].as_array() { for row in rows {
                if let (Some(id),Some(label))=(row["id"].as_str(),row["label"].as_str()) {labels[i].insert(id.to_owned(),label.to_owned());}
            }}}
        }
        Ok(Self{api,labels,slots:tokio::sync::Semaphore::new(4)})
    }

    pub(crate) async fn serve(&self,db:&Writer,places:&PublicPlaces,profile:Arc<ProfileRules>,uid:i64,now:i64,lang:&'static str)
        ->Result<Response,StatusCode>
    {
        let country=places.country_index();
        let result=db.transaction(move |tx|Box::pin(async move {
            let row=tx.query_opt("SELECT email,COALESCE(report_banned_at,0)!=0,COALESCE(is_seed,0)!=0,
                COALESCE(jev_recommendations_unlocked,0)!=0 AND COALESCE(jev_feed_enabled,0)!=0,
                COALESCE(premium_until,0)>$2::bigint FROM users WHERE id=$1::bigint AND COALESCE(deleted_at,0)=0", &[&uid,&now]).await?;
            let Some(row)=row else {return Ok(ResultCode::SignedOut);};
            if row.get::<_,bool>(1) {return Ok(ResultCode::Banned);}
            tx.execute("UPDATE users SET last_seen=$2::bigint WHERE id=$1::bigint", &[&uid,&now]).await?;
            let email:&str=row.get(0);
            if email.ends_with("@wiring.guest") || email=="demo@wiring.app" {return Ok(ResultCode::Guest);}
            if row.get::<_,bool>(2)||!row.get::<_,bool>(3) {return Ok(ResultCode::Locked);}
            let premium:bool=row.get(4);
            let viewer=profile.card(tx,uid,now,lang).await?;
            let qa=email.trim().eq_ignore_ascii_case("wiringremlin@gmail.com");
            let like_ids=history(tx,uid,"like").await?;
            let mut likes=Vec::new();
            for id in like_ids {if let Some(card)=candidate(tx,&profile,&country,uid,id,now,lang,premium,qa,&viewer).await? {likes.push(card);}}
            let mut passes=Vec::new();
            if likes.len()>=3 {
                for id in history(tx,uid,"pass").await? {if let Some(card)=candidate(tx,&profile,&country,uid,id,now,lang,premium,qa,&viewer).await? {passes.push(card);}}
            }
            let mut shortlist=Vec::with_capacity(61);
            let mut cursor=0_i64;
            loop {
                let rows=tx.query("SELECT u.id::bigint FROM users u WHERE u.id>$1::bigint AND u.id<>$2::bigint
                    AND COALESCE(u.deleted_at,0)=0 AND u.age BETWEEN 18 AND 99 AND COALESCE(u.paused,0)=0
                    AND NOT EXISTS(SELECT 1 FROM swipes WHERE from_id=$2::bigint AND to_id=u.id)
                    AND NOT EXISTS(SELECT 1 FROM feed_history WHERE user_id=$2::bigint AND other_id=u.id AND excluded_at IS NOT NULL)
                    AND (COALESCE(u.photo,'')<>'' OR EXISTS(SELECT 1 FROM photos WHERE user_id=u.id)) ORDER BY u.id LIMIT 256", &[&cursor,&uid]).await?;
                if rows.is_empty() {break;}
                for row in rows {
                    cursor=row.get(0);
                    if let Some(card)=candidate(tx,&profile,&country,uid,cursor,now,lang,premium,qa,&viewer).await? {
                        let score=if likes.len()>=3 {taste_score(&likes,&passes,&card)} else {priority(&viewer,&card)};
                        shortlist.push((score,cursor,card));
                        if shortlist.len()>60 {
                            let worst=shortlist.iter().enumerate().min_by_key(|(_,item)|(item.0,-item.1)).map(|(i,_)|i).unwrap_or_default();
                            shortlist.swap_remove(worst);
                        }
                    }
                }
            }
            shortlist.sort_by_key(|item|(-item.0,item.1));
            Ok(ResultCode::Ready{viewer,cards:shortlist.into_iter().map(|(_,_,card)|card).collect(),taste:likes.len()>=3})
        })).await?.0;
        let reject=|en,ru|(StatusCode::FORBIDDEN,Json(json!({"ok":false,"error":if lang=="en" {en} else {ru}}))).into_response();
        Ok(match result {
            ResultCode::SignedOut=>crate::account_settings::signed_out(lang),
            ResultCode::Banned=>crate::account_settings::report_ban_response(lang),
            ResultCode::Guest=>reject("Complete your profile first","сначала собери свой профиль"),
            ResultCode::Locked=>reject("Activate the promo code and enable recommendations in your profile","активируй промокод и включи подбор в профиле"),
            ResultCode::Ready{viewer,mut cards,taste}=>{
                let source=if taste {"taste"} else if let Some(order)=self.rank(&viewer,&cards).await {
                    let mut input=cards.into_iter().map(Some).collect::<Vec<_>>();
                    cards=order.into_iter().filter_map(|i|input[i].take()).collect();
                    "api"
                } else {"local"};
                for card in &mut cards {card["recommendation_reasons"]=json!(self.reasons(&viewer,card,lang));}
                ([(header::CACHE_CONTROL,"no-store")],Json(json!({"ok":true,"cards":cards,"has_more":false,"generation":0,"recommendation_source":source}))).into_response()
            }
        })
    }

    async fn rank(&self,viewer:&Value,cards:&[Value])->Option<Vec<usize>> {
        if cards.len()<2 {return None;}
        let (http,key,model)=self.api.as_ref()?;
        let _slot=self.slots.try_acquire().ok()?;
        let mut state=serde_json::Map::new(); let mut questions=serde_json::Map::new();
        for (i,card) in cards.iter().enumerate() {
            let signal=format!("pair_{i}");
            state.insert(signal.clone(),pair_signals(viewer,card));
            questions.insert(format!("candidate_{i}"),json!({"type":"noul",
                "instructions":format!("Estimate the probability that this pair would mutually choose to like each other using only the structured signals in {signal}. Weight shared_neuro and shared_vibe strongly. Use same_city, age_gap_band and shared_intent_ids as softer signals. Eligibility and mutual looking rules are already satisfied. Do not invent tags or profile details."),
                "criteria":{"true":"Both people would likely choose to like each other after viewing each other's profile.",
                "false":"At least one person would likely not choose to like the other."}}));
        }
        let mut response=http.post("https://api.typesafe.ai/v1/systemone").bearer_auth(key).header("Accept","application/json")
            .json(&json!({"model":model,"state":state,"questions":questions})).send().await.ok()?;
        if response.status().as_u16()!=200 {return None;}
        let mut data=Vec::new();
        while let Some(chunk)=response.chunk().await.ok()? {
            if data.len()+chunk.len()>1024*1024 {return None;}
            data.extend_from_slice(&chunk);
        }
        let data:Value=serde_json::from_slice(&data).ok()?;
        let mut scores=Vec::new();
        for i in 0..cards.len() {
            let answer=&data["answers"][format!("candidate_{i}")];
            let score=answer["noul"].as_f64().filter(|v|(0.0..=1.0).contains(v))?;
            if answer["type"]!="noul" {return None;}
            scores.push((score,i));
        }
        scores.sort_by(|a,b|b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
        Some(scores.into_iter().map(|(_,i)|i).collect())
    }

    fn reasons(&self,viewer:&Value,card:&Value,lang:&str)->Vec<String> {
        let signal=pair_signals(viewer,card);
        let en=lang=="en";
        let text=|en_text:&'static str,ru:&'static str|if en {en_text} else {ru};
        let labels=|key:&str,limit:usize|strings(&signal[key],limit).iter().map(|tag|self.labels[usize::from(en)].get(tag).unwrap_or(tag).as_str()).collect::<Vec<_>>().join(", ");
        let mut out=Vec::new();
        let shared=signal["shared_neuro"].as_array().map_or(0,Vec::len);
        if shared>0 {
            let tail=if shared>4 {format!(" (+{})",shared-4)} else {String::new()};
            out.push(format!("{}: {}{tail}",text("Shared self-reported diagnoses","Общие отметки о диагнозах"),labels("shared_neuro",4)));
        }
        if !strings(&signal["shared_vibe"],1).is_empty() {out.push(format!("{}: {}",text("Shared vibe","Совпадает вайб"),labels("shared_vibe",3)));}
        if signal["same_city"]==true {out.push(text("Same city — easier to meet in person","Один город — проще встретиться офлайн").into());}
        if !strings(&signal["shared_intent_ids"],1).is_empty() {out.push(format!("{}: {}",text("Similar goals","Схожие цели"),labels("shared_intent_ids",3)));}
        if matches!(signal["age_gap_band"].as_str(),Some("0-2"|"3-5")) {out.push(text("Similar age","Близкий возраст").into());}
        if out.is_empty() {out.push(text("Your profiles fit each other’s visibility preferences","Анкета подходит под ваши взаимные предпочтения видимости").into());}
        out.truncate(3); out
    }
}

async fn history(tx:&Transaction<'_>,uid:i64,direction:&str)->TransactionResult<Vec<i64>> {
    Ok(tx.query("SELECT s.to_id::bigint FROM swipes s JOIN users u ON u.id=s.to_id WHERE s.from_id=$1::bigint AND s.direction=$2
        AND COALESCE(u.deleted_at,0)=0 ORDER BY s.created_at DESC LIMIT 40", &[&uid,&direction]).await?.into_iter().map(|r|r.get(0)).collect())
}

async fn candidate(tx:&Transaction<'_>,profile:&ProfileRules,country:&CountryIndex,uid:i64,id:i64,now:i64,lang:&str,premium:bool,qa:bool,viewer:&Value)
    ->TransactionResult<Option<Value>>
{
    let row=tx.query_opt("SELECT email,COALESCE(profile_setup_required,0)!=0,COALESCE(special_data_consent_at,0)!=0
        FROM users u WHERE u.id=$2::bigint AND COALESCE(u.deleted_at,0)=0 AND COALESCE(u.is_seed,0)=0 AND COALESCE(u.paused,0)=0
        AND COALESCE(u.report_banned_at,0)=0 AND (COALESCE(u.incognito,0)=0 OR EXISTS(SELECT 1 FROM swipes WHERE from_id=$2::bigint AND to_id=$1::bigint AND direction='like'))
        AND NOT EXISTS(SELECT 1 FROM blocks WHERE (from_id=$1::bigint AND to_id=$2::bigint) OR (from_id=$2::bigint AND to_id=$1::bigint))
        AND (NOT $3 OR NOT EXISTS(SELECT 1 FROM snoozes WHERE user_id=$1::bigint AND other_id=$2::bigint AND until_at>$4::bigint))", &[&uid,&id,&premium,&now]).await?;
    let Some(row)=row else {return Ok(None);};
    let email:&str=row.get(0);
    if email.ends_with("@wiring.guest")||email=="demo@wiring.app"||row.get::<_,bool>(1)||!row.get::<_,bool>(2) {return Ok(None);}
    let card=profile.card(tx,id,now,lang).await?;
    let name=card["name"].as_str().unwrap_or("").trim();
    let age=card["age"].as_i64().unwrap_or(0);
    let gender=card["gender"].as_str().unwrap_or("");
    if !(2..=32).contains(&name.chars().count()) || matches!(name.to_lowercase().as_str(),"участник"|"participant") || !(18..=99).contains(&age) || !profile.gender_ok(gender) ||
        card["has_photo"]!=true || strings(&card["neuro"],1).is_empty() || !visibility(&card,viewer,country) || !visibility(viewer,&card,country)
        || !looking(viewer,&card) || !qa && !looking(&card,viewer) {return Ok(None);}
    Ok(Some(card))
}

fn visibility(owner:&Value,viewer:&Value,country:&CountryIndex)->bool {
    let min=owner["seek_min_age"].as_i64().unwrap_or(18).clamp(18,99);
    let max=owner["seek_max_age"].as_i64().unwrap_or(99).clamp(18,99);
    let age=viewer["age"].as_i64().unwrap_or(0);
    let place=owner["seek_place"].as_str().unwrap_or("").trim();
    let city=viewer["city"].as_str().unwrap_or("").trim();
    (min.min(max)..=min.max(max)).contains(&age) && (place.is_empty()||place==city||place==country.country(city)) &&
        !strings(&owner["hide_tags"],24).iter().any(|tag|strings(&viewer["vibe"],24).contains(tag))
}

fn looking(viewer:&Value,candidate:&Value)->bool {
    let gender=candidate["gender"].as_str().unwrap_or("");
    matches!(gender,"hidden"|"other"|"nb") || match viewer["looking_for"].as_str() {Some("men")=>gender=="man",Some("women")=>gender=="woman",_=>true}
}

fn strings(value:&Value,max:usize)->Vec<String> {
    let mut out=Vec::new();
    if let Some(list)=value.as_array() {for tag in list.iter().take(max).filter_map(Value::as_str).map(str::trim) {
        if !tag.is_empty()&&!out.iter().any(|s|s==tag) {out.push(tag.to_owned());}
    }}
    out
}

fn pair_signals(a:&Value,b:&Value)->Value {
    let av=strings(&a["vibe"],24); let bv=strings(&b["vibe"],24);
    let an=strings(&a["neuro"],24); let bn=strings(&b["neuro"],24);
    let ai=strings(&a["intents"],8); let bi=strings(&b["intents"],8);
    let overlap=|a:&Vec<String>,b:&Vec<String>|a.iter().filter(|tag|b.contains(tag)).cloned().collect::<BTreeSet<_>>();
    let shared_intents=overlap(&ai,&bi);
    let city=|a:&Value|a["city"].as_str().unwrap_or("").trim().to_lowercase();
    let city_a=city(a); let city_b=city(b);
    let gap=a["age"].as_i64().unwrap_or(0).abs_diff(b["age"].as_i64().unwrap_or(0));
    json!({"same_city":!city_a.is_empty() && city_a==city_b,
        "age_gap_band":match gap {0..=2=>"0-2",3..=5=>"3-5",6..=10=>"6-10",_=>"11+"},
        "viewer_neuro":an,"candidate_neuro":bn,"shared_neuro":overlap(&an,&bn),
        "viewer_vibe":av,"candidate_vibe":bv,"shared_vibe":overlap(&av,&bv),
        "shared_intents_count":shared_intents.len(),"shared_intent_ids":shared_intents})
}

fn priority(a:&Value,b:&Value)->i32 {
    let s=pair_signals(a,b);
    (8*strings(&s["shared_vibe"],4).len()+6*strings(&s["shared_intent_ids"],3).len()+2*strings(&s["shared_neuro"],3).len()) as i32
        +3*i32::from(s["same_city"]==true)+match s["age_gap_band"].as_str(){Some("0-2")=>2,Some("3-5")=>1,_=>0}
}

fn taste_score(likes:&[Value],passes:&[Value],card:&Value)->i32 {
    let mean=|samples:&[Value]|if samples.is_empty(){0.0}else{samples.iter().map(|row|priority(row,card)).sum::<i32>() as f64/samples.len() as f64};
    (mean(likes)*10.0-mean(passes)*6.0).round_ties_even() as i32
}
