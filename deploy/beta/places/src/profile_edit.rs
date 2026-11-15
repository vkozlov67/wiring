use std::{collections::HashSet,sync::Arc};

use axum::{Json,http::StatusCode,response::{IntoResponse,Response}};
use serde_json::{Value,json};

use crate::{chat_write::Writer,mobile_http::string_field,place_validation::Validation,places_http::PublicPlaces,
    premium_http::truthy,profile_read::ProfileRules};

type ErrorText = (&'static str,&'static str);
const MISSING:ErrorText=("No profile","нет профиля");

struct Input {
    draft:bool,name:String,age:i64,city:String,gender:String,looking:String,bio:String,job:String,communication:String,
    intents:Vec<String>,height:Option<i64>,neuro:Vec<String>,vibe:Vec<String>,prompts:Vec<(String,String)>,
    seek_min:i64,seek_max:i64,seek_place:String,hidden:Vec<String>,photo:String,primary:Option<i64>,
    special:bool,rights:bool,
}

enum Outcome { Missing,Banned,Conflict,NameWait(i64),Bad(ErrorText),Done(Value) }

pub(crate) async fn edit(db:&Writer, validation:&Validation, places:&PublicPlaces, profile:Arc<ProfileRules>,
    uid:i64, fields:Value, now:i64, lang:&'static str) -> Result<Response,StatusCode>
{
    let initial=Arc::clone(&profile);
    let snapshot=db.transaction(move |tx|Box::pin(async move {
        let row=tx.query_opt("SELECT COALESCE(report_banned_at,0)!=0,name_changed_at::bigint,photo_rights_consent_at::bigint,
            special_data_consent_at::bigint,name,COALESCE(age,0) FROM users WHERE id=$1::bigint AND COALESCE(deleted_at,0)=0", &[&uid]).await?;
        let Some(row)=row else { return Ok(None); };
        if row.get::<_,bool>(0) { return Ok(Some((Value::Null,None,None,None))); }
        tx.execute("UPDATE users SET last_seen=$2::bigint WHERE id=$1::bigint", &[&uid,&now]).await?;
        let mut current=initial.person(tx,uid,now,lang,true).await?;
        current["name"]=json!(row.get::<_,&str>(4));
        current["age"]=json!(row.get::<_,i32>(5));
        Ok(Some((current,row.get::<_,Option<i64>>(1),row.get::<_,Option<i64>>(2),row.get::<_,Option<i64>>(3))))
    })).await?.0;
    let Some((current,name_changed,rights,special))=snapshot else { return Ok(reply(Outcome::Missing,lang)); };
    if current.is_null() { return Ok(reply(Outcome::Banned,lang)); }
    let draft=truthy(&fields["draft"]);
    let text=|field:&str| {
        let value=if fields.get(field).is_some() { string_field(&fields,field) } else {string_field(&current,field)};
        value.trim().to_owned()
    };
    let mut city=text("city");
    if !city.is_empty() && (fields.get("city").is_some() || !draft) {
        match validation.resolve(db,places,uid,&city,"",now).await? {
            Ok((name,_))=>city=name,
            Err(problem)=>return Ok(problem.response(lang)),
        }
    }
    let mut seek_place=text("seek_place");
    if !seek_place.is_empty() && !places.country_known(&seek_place) {
        if let Some((name,_))=places.known(&seek_place) { seek_place=name; }
        else if !profile.city_ok(&seek_place) { return Ok(reply(Outcome::Bad(("Country or city from the list","страна или город из списка")),lang)); }
    }
    let input=match parse(&fields,&current,&profile,draft,city,seek_place) {
        Ok(input)=>input,
        Err(error)=>return Ok((StatusCode::BAD_REQUEST,Json(json!({"ok":false,"error":error.message(lang)}))).into_response()),
    };
    let result=db.transaction(move |tx|Box::pin(async move {
        let row=tx.query_opt("SELECT name,name_changed_at::bigint,COALESCE(profile_setup_required,0)!=0,COALESCE(report_banned_at,0)!=0,
            photo,special_data_consent_at::bigint,photo_rights_consent_at::bigint,is_seed!=0,email,COALESCE(age,0)
            FROM users WHERE id=$1::bigint AND COALESCE(deleted_at,0)=0 FOR UPDATE", &[&uid]).await?;
        let Some(row)=row else { return Ok(Outcome::Missing); };
        if row.get::<_,bool>(3) { return Ok(Outcome::Banned); }
        let old_name:&str=row.get(0);
        let compare_name=if matches!(old_name.trim().to_lowercase().as_str(),"участник"|"participant") {""} else {old_name};
        let mut original=profile.person(tx,uid,now,lang,true).await?;
        original["name"]=json!(old_name);
        original["age"]=json!(row.get::<_,i32>(9));
        let modified=current.as_object().is_some_and(|fields| fields.iter().any(|(key,value)|
            matches!(key.as_str(),"name"|"age"|"city"|"gender"|"looking_for"|"bio"|"job"|"communication"|"intents"|"height"|
                "neuro"|"vibe"|"prompts"|"seek_min_age"|"seek_max_age"|"seek_place"|"hide_tags"|"photos") && original[key]!=*value));
        if modified || row.get::<_,Option<i64>>(1)!=name_changed || row.get::<_,Option<i64>>(5)!=special || row.get::<_,Option<i64>>(6)!=rights {
            return Ok(Outcome::Conflict);
        }
        let available=name_changed.unwrap_or_default()+14*86400;
        if !row.get::<_,bool>(2) && !compare_name.trim().is_empty() && input.name!=old_name && name_changed.is_some_and(|v|v!=0) && available>now {
            return Ok(Outcome::NameWait(available));
        }
        let special_at=if input.special {Some(special.filter(|v|*v!=0).unwrap_or(now))} else {special};
        let rights_at=if input.rights {Some(rights.filter(|v|*v!=0).unwrap_or(now))} else {rights};
        let has_photo=!original["photos"].as_array().is_none_or(Vec::is_empty) || !row.get::<_,&str>(4).trim().is_empty() || !input.photo.is_empty();
        if !input.draft && !has_photo { return Ok(Outcome::Bad(("Add an image for your profile","Добавь изображение для анкеты"))); }
        if !input.draft && !input.neuro.is_empty() && special_at.unwrap_or_default()==0 {
            return Ok(Outcome::Bad(("Consent is required to process and show your selected traits","нужно согласие на обработку и показ выбранных особенностей")));
        }
        if let Some(primary)=input.primary {
            if tx.query_opt("SELECT id FROM photos WHERE id=$1::bigint AND user_id=$2::bigint", &[&primary,&uid]).await?.is_none() {
                return Ok(Outcome::Bad(("Photo not found","фото не найдено")));
            }
        }
        let changed=if input.name!=old_name {Some(now)} else {name_changed};
        let intents=input.intents.join(","); let hide=input.hidden.join(",");
        let email:&str=row.get(8);
        let complete=row.get::<_,bool>(7) || ((2..=32).contains(&input.name.chars().count()) && (18..=99).contains(&input.age) &&
            profile.gender_ok(&input.gender) && has_photo && (input.neuro.is_empty() || special_at.unwrap_or_default()!=0));
        tx.execute("UPDATE users SET name=$2,name_changed_at=$3::bigint,age=$4::bigint,city=$5,gender=$6,looking_for=$7,bio=$8,
            job=$9,intent=$10,height=$11::bigint,communication=$12,special_data_consent_at=$13::bigint,photo_rights_consent_at=$14::bigint,
            seek_min_age=$15::bigint,seek_max_age=$16::bigint,seek_place=$17,hide_tags=$18,
            profile_setup_required=CASE WHEN $19 THEN profile_setup_required ELSE 0 END,
            onboard_done=CASE WHEN $20 THEN $21 ELSE onboard_done END WHERE id=$1::bigint",
            &[&uid,&input.name,&changed,&input.age,&input.city,&input.gender,&input.looking,&input.bio,&input.job,&intents,&input.height,
              &input.communication,&special_at,&rights_at,&input.seek_min,&input.seek_max,&input.seek_place,&hide,&input.draft,
              &(!input.draft && !email.ends_with("@wiring.guest") && email!="demo@wiring.app"),&i32::from(complete)]).await?;
        tx.execute("DELETE FROM user_tags WHERE user_id=$1::bigint", &[&uid]).await?;
        for (kind,tags) in [("neuro",input.neuro),("vibe",input.vibe)] { for tag in tags {
            tx.execute("INSERT INTO user_tags(user_id,kind,tag) VALUES($1::bigint,$2,$3)", &[&uid,&kind,&tag]).await?;
        }}
        tx.execute("DELETE FROM user_prompts WHERE user_id=$1::bigint", &[&uid]).await?;
        for (order,(id,answer)) in input.prompts.into_iter().enumerate() {
            tx.execute("INSERT INTO user_prompts(user_id,prompt_id,answer,sort_order) VALUES($1::bigint,$2,$3,$4)", &[&uid,&id,&answer,&(order as i32)]).await?;
        }
        if !input.photo.is_empty() {
            let existing=tx.query_opt("SELECT id::bigint FROM photos WHERE user_id=$1::bigint AND path=$2", &[&uid,&input.photo]).await?;
            if existing.is_none() {
                let album=tx.query_opt("SELECT id::bigint FROM albums WHERE user_id=$1::bigint AND title='я' ORDER BY id LIMIT 1", &[&uid]).await?;
                let album:i64=if let Some(album)=album {album.get(0)} else {
                    tx.query_one("INSERT INTO albums(user_id,title,sort_order,created_at) SELECT $1::bigint,'я',count(*)::integer,$2::bigint
                        FROM albums WHERE user_id=$1::bigint RETURNING id::bigint", &[&uid,&now]).await?.get(0)
                };
                tx.execute("INSERT INTO photos(user_id,album_id,path,is_primary,sort_order,created_at) VALUES($1::bigint,$2::bigint,$3,0,0,$4::bigint)",
                    &[&uid,&album,&input.photo,&now]).await?;
            }
            tx.execute("UPDATE photos SET is_primary=CASE WHEN path=$2 THEN 1 ELSE 0 END WHERE user_id=$1::bigint", &[&uid,&input.photo]).await?;
        }
        if let Some(primary)=input.primary {
            tx.execute("UPDATE photos SET is_primary=CASE WHEN id=$2::bigint THEN 1 ELSE 0 END WHERE user_id=$1::bigint", &[&uid,&primary]).await?;
        }
        if !input.photo.is_empty() || input.primary.is_some() {
            tx.execute("UPDATE users SET photo=COALESCE((SELECT path FROM photos WHERE user_id=$1::bigint ORDER BY is_primary DESC,sort_order,id LIMIT 1),'')
                WHERE id=$1::bigint", &[&uid]).await?;
        }
        let user=profile.person(tx,uid,now,lang,true).await?;
        Ok(Outcome::Done(json!({"ok":true,"user":user,"draft":input.draft})))
    })).await?.0;
    Ok(reply(result,lang))
}

enum Invalid { Fixed(ErrorText), Tag(String), Profanity(ErrorText) }

impl Invalid {
    fn message(&self,lang:&str)->String {
        let en=lang=="en";
        match self {
            Self::Fixed((a,b))=>(if en {*a} else {*b}).to_owned(),
            Self::Tag(tag)=>if en {format!("Unknown tag: {tag}")} else {format!("неизвестный тег: {tag}")},
            Self::Profanity((a,b))=>if en {format!("The “{a}” field contains profanity. Please rephrase it.")}
                else {format!("в поле «{b}» есть мат — переформулируй")},
        }
    }
}

fn parse(data:&Value,current:&Value,profile:&ProfileRules,draft:bool,mut city:String,seek_place:String) -> Result<Input,Invalid> {
    let field=|key:&str|data.get(key).unwrap_or(&current[key]);
    let text=|key:&str| match field(key) {
        Value::String(s)=>s.trim().to_owned(), Value::Number(n) if n.as_f64()!=Some(0.0)=>n.to_string(),
        Value::Bool(true)=>"True".into(),_=>String::new(),
    };
    let invalid=|a,b|Invalid::Fixed((a,b));
    let name=text("name");
    let age=if draft && blank(field("age")) {0} else {integer(field("age")).ok_or_else(||invalid("Age must be a number","возраст — число"))?};
    let height=if blank(field("height")) || field("height").as_str()==Some("0") || field("height").as_f64()==Some(0.0) {None} else {
        let h=integer(field("height")).ok_or_else(||invalid("Height must be a number in centimetres","рост — число в см"))?;
        if !(140..=220).contains(&h) {return Err(invalid("Height must be between 140 and 220 cm","рост: 140–220 см"));} Some(h)
    };
    if !(draft && name.is_empty() || (2..=32).contains(&name.chars().count())) {return Err(invalid("Name must contain between 2 and 32 characters","имя: 2–32 символа"));}
    if !draft && matches!(name.to_lowercase().as_str(),"участник"|"participant") {return Err(invalid("Enter a name for your profile","Укажи имя для анкеты"));}
    if !(draft && age==0 || (18..=99).contains(&age)) {return Err(invalid("Adults only (18+)","только 18+"));}
    if !city.is_empty() {
        if !(2..=256).contains(&city.chars().count()) {
            if draft {city="—".into();} else {return Err(invalid("city: 2–256 characters","город: 2–256 символов"));}
        } else if !draft && !profile.city_ok(&city) {return Err(invalid("Choose a city from the list","выбери город из списка"));}
    }
    let bio=text("bio"); let job=text("job"); let communication=text("communication");
    for (value,max,error) in [(&bio,1200,("Bio must be no longer than 1200 characters","био до 1200 символов")),
        (&job,60,("Occupation must be no longer than 60 characters","занятие до 60 символов")),
        (&communication,500,("Communication preferences must be no longer than 500 characters","как тебе писать: до 500 символов"))] {
        if value.chars().count()>max {return Err(Invalid::Fixed(error));}
    }
    for (value,field) in [(&name,("Name","имя")),(&bio,("About me","о себе")),(&job,("Occupation","занятость")),
        (&communication,("how to message you","как тебе писать"))] { if profanity(value) {return Err(Invalid::Profanity(field));} }
    let mut gender=text("gender"); if matches!(gender.as_str(),"nb"|"other") {gender="hidden".into();}
    if !profile.gender_ok(&gender) {return Err(invalid("Choose a gender","выбери гендер"));}
    let looking=text("looking_for");
    if !matches!(looking.as_str(),"men"|"women"|"everyone"|"friends") {return Err(invalid("Who are you looking for?","кого ищешь?"));}
    let tags=|kind:&str| -> Result<Vec<String>,Invalid> {
        let raw=field(kind); if raw.is_null() {return Ok(Vec::new());}
        let list=raw.as_array().ok_or_else(||invalid("Tags must be a list","теги должны быть списком"))?;
        let mut result=Vec::new();
        for value in list {
            let value=value.as_str().filter(|tag|profile.filter_keys(kind).contains(*tag)).ok_or_else(||Invalid::Tag(value.as_str().map(str::to_owned).unwrap_or_else(||value.to_string())))?;
            if !result.iter().any(|v|v==value) {result.push(value.to_owned());}
        }
        Ok(result)
    };
    let (neuro,vibe)=profile.tags(tags("neuro")?,tags("vibe")?);
    let mut prompts=Vec::new(); let mut seen=HashSet::new();
    if !field("prompts").is_null() {
        let list=field("prompts").as_array().ok_or_else(||invalid("Prompts must be a list","промпты должны быть списком"))?;
        if list.len()>3 {return Err(invalid("You can add up to three prompts","можно три промпта"));}
        for item in list {
            if !item.is_object() {return Err(invalid("A prompt must be an object with an id and answer","промпт — объект с id и ответом"));}
            let id=string_field(item,"id");
            if !matches!(id.as_str(),"special"|"sensory"|"date"|"never"|"spoons"|"parallel"|"text"|"green"|"mask"|"obsessed"|"sunday"|"deal") {
                return Err(invalid("Unknown prompt","неизвестный промпт"));
            }
            if !seen.insert(id.clone()) {continue;}
            let answer=string_field(item,"answer").trim().to_owned();
            if !(4..=280).contains(&answer.chars().count()) {return Err(invalid("Prompt answers must contain between 4 and 280 characters","ответ промпта: 4–280 символов"));}
            if profanity(&answer) {return Err(invalid("The prompt contains profanity. Please rephrase it.","в промпте есть мат — переформулируй"));}
            prompts.push((id,answer));
        }
    }
    let raw=if field("intents").is_null() {field("intent")} else {field("intents")};
    let candidates:Vec<String>=match raw {
        Value::Array(items)=>items.iter().filter_map(Value::as_str).map(str::to_owned).collect(),
        Value::String(s)=>serde_json::from_str(s).unwrap_or_else(|_|s.split([',',';','|']).map(str::to_owned).collect()),
        _=>Vec::new(),
    };
    let mut intents=Vec::new();
    for key in candidates { let key=key.trim().to_owned(); if profile.filter_keys("intent").contains(&key) && !intents.contains(&key) {intents.push(key);} }
    if intents.is_empty() {intents.push("dating".into());}
    let limit=|key:&str,default:i64|if blank(field(key)) {Ok(default)} else {integer(field(key)).map(|age|age.clamp(18,99))
        .ok_or_else(||invalid("Visibility ages must be numbers","возраст видимости — числа"))};
    let (min,max)=(limit("seek_min_age",18)?,limit("seek_max_age",99)?);
    let mut hidden=Vec::new();
    let raw=field("hide_tags");
    let items:Vec<&str>=raw.as_array().map(|a|a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_else(||raw.as_str().unwrap_or("").split(',').collect());
    for tag in items {if profile.filter_keys("vibe").contains(tag) && !hidden.iter().any(|value|value==tag) {hidden.push(tag.to_owned());}}
    let candidate=string_field(data,"photo");
    let photo=if seed_photo(&candidate) {candidate} else {String::new()};
    let primary=if blank(&data["primary_photo_id"]) {None} else {Some(integer(&data["primary_photo_id"]).ok_or_else(||invalid("Invalid photo","не то фото"))?)};
    Ok(Input {draft,name,age,city,gender,looking,bio,job,communication,intents,height,neuro,vibe,prompts,seek_min:min.min(max),
        seek_max:min.max(max),seek_place,hidden,photo,primary,special:consent(&data["special_data_consent"]),rights:consent(&data["photo_rights_consent"])})
}

fn seed_photo(path:&str)->bool {
    if let Some(id)=path.strip_prefix("portraits/p0").and_then(|p|p.strip_suffix(".jpg")) { return id.len()==1 && matches!(id.as_bytes()[0],b'1'..=b'9'); }
    if let Some(id)=path.strip_prefix("people/").and_then(|p|p.strip_suffix(".jpg")) { return id.len()==2 && id.parse::<u8>().is_ok_and(|n|(1..=12).contains(&n)); }
    false
}
fn blank(value:&Value)->bool {value.is_null() || value.as_str()==Some("")}
fn integer(value:&Value)->Option<i64> {
    match value {
        Value::String(s)=>s.trim().parse().ok(), Value::Bool(v)=>Some(i64::from(*v)),
        Value::Number(n)=>n.as_i64().or_else(||n.as_f64().filter(|n|(-9_223_372_036_854_775_808.0..9_223_372_036_854_775_808.0).contains(n)).map(|n|n as i64)),
        _=>None,
    }
}
fn consent(value:&Value)->bool {value==&Value::Bool(true) || value.as_f64()==Some(1.0) || value.as_str().is_some_and(|v|matches!(v,"1"|"true"|"True"|"yes"|"on"))}

fn profanity(value:&str)->bool {
    let lower=value.to_lowercase();
    lower.split(|c:char| !(c.is_alphanumeric() || c=='_')).any(|word| {
        ["блять","сука","суки","сучку","сучке"].contains(&word) ||
        ["хуй","хуи","хуя","хую","хуе","хуё","пизд","пезд","ебан","ебат","ебал","ёбан","ёбат","ёбал","ебл","ёбл","бляд",
         "мудак","мудил","говн","дерьм","залуп","пидор","пидар","педик","гандон","гондон","дроч","fuck","shit","bitch","cunt","asshole"]
            .iter().any(|prefix|word.starts_with(prefix))
    })
}

fn date(stamp:i64)->String {
    let Some(time)=std::time::UNIX_EPOCH.checked_add(std::time::Duration::from_secs(stamp.max(0) as u64)) else {return "UTC".into();};
    let formatted=httpdate::fmt_http_date(time);
    let parts:Vec<_>=formatted.split_ascii_whitespace().collect();
    let month=["Jan","Feb","Mar","Apr","May","Jun","Jul","Aug","Sep","Oct","Nov","Dec"].iter().position(|value|*value==parts[2]).unwrap_or(0)+1;
    format!("{}.{month:02}.{} {} UTC",parts[1],parts[3],&parts[4][..5])
}

fn reply(outcome:Outcome,lang:&str)->Response {
    let text=|(en,ru):ErrorText|if lang=="en" {en} else {ru};
    match outcome {
        Outcome::Missing=>(StatusCode::UNAUTHORIZED,Json(json!({"ok":false,"error":text(MISSING)}))).into_response(),
        Outcome::Banned=>crate::account_settings::report_ban_response(lang),
        Outcome::Conflict=>(StatusCode::CONFLICT,Json(json!({"ok":false,"error":text(("Profile changed. Refresh and try again.","Анкета изменилась. Обнови страницу и попробуй снова."))}))).into_response(),
        Outcome::NameWait(available)=>{
            let date=date(available);
            let error=if lang=="en" {format!("You can change your name on {date}. There must be 14 days between changes.")}
                else {format!("Имя можно будет изменить {date}. Между сменами имени — 14 дней.")};
            (StatusCode::CONFLICT,Json(json!({"ok":false,"error":error,"name_change_available_at":available}))).into_response()
        }
        Outcome::Bad(error)=>{
            let status=if error.0=="Photo not found" {StatusCode::NOT_FOUND} else {StatusCode::BAD_REQUEST};
            (status,Json(json!({"ok":false,"error":text(error)}))).into_response()
        }
        Outcome::Done(data)=>Json(data).into_response(),
    }
}
