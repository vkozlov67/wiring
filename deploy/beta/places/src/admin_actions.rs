use std::{env,error::Error,sync::Arc,time::{SystemTime,UNIX_EPOCH}};

use axum::{Router,body::{Body,to_bytes},extract::State,http::{Method,Request,StatusCode,header},
    response::{IntoResponse,Response},routing::any};
use subtle::ConstantTimeEq;

use crate::{chat_write::Writer,public_http::header_text,signed_session::SessionKeys,spa_http::web_cookie};

struct Admin { db:Arc<Writer>,keys:SessionKeys,origin:String,enabled:bool,threshold:f32,
    replies:Option<crate::admin_reply::Replies>, reply_limit:crate::login::LoginLimit }
enum Action { Premium,ClearByName,ClearById(i64),Promo,Decision(i64),Import }
enum Outcome { Done(&'static str), Failed(StatusCode,&'static str) }

pub(crate) fn routes(writer:Option<Arc<Writer>>)->Result<Router,Box<dyn Error>> {
    if env::var("WIRING_PUBLIC_ADMIN_ACTIONS").as_deref()!=Ok("1") {return Ok(Router::new());}
    let key=env::var("APP_SECRET_KEY")?;
    if key.trim().is_empty(){return Err("Admin actions require signed sessions".into());}
    let origin=env::var("SITE_URL").unwrap_or_else(|_|"https://wiring.club".into()).trim_end_matches('/').to_owned();
    let parsed=reqwest::Url::parse(&origin)?;
    if parsed.origin().ascii_serialization()!=origin || parsed.scheme()!="https" {return Err("Admin actions require an HTTPS site origin".into());}
    let enabled=env::var("ADMIN_TOKEN").is_ok_and(|value|value.split(',').any(|token|!token.trim().is_empty()));
    let db=writer.ok_or("Admin actions require PostgreSQL")?;
    let threshold=env::var("SUPPORT_TASK_THRESHOLD").ok().and_then(|value|value.parse::<f32>().ok()).filter(|value|value.is_finite()).unwrap_or(6.0).clamp(0.0,10.0);
    let replies=(env::var("WIRING_PUBLIC_ADMIN_REPLIES").as_deref()==Ok("1")).then(||crate::admin_reply::Replies::new(&origin)).transpose()?;
    let mut router=Router::new().route("/admin/premium",any(serve)).route("/admin/report-ban-clear",any(serve))
        .route("/admin/users/{id}/report-ban-clear",any(serve)).route("/admin/promo",any(serve))
        .route("/admin/tasks/{id}/decision",any(serve)).route("/admin/tasks/import-support",any(serve));
    if replies.is_some(){router=router.route("/admin/replies/{kind}/{id}",any(serve));}
    Ok(router.with_state(Arc::new(Admin{db,keys:SessionKeys::new(key.as_bytes()),origin,enabled,threshold,replies,
        reply_limit:crate::login::LoginLimit::configured(std::time::Duration::from_secs(3600),20)})))
}

async fn serve(State(state):State<Arc<Admin>>,request:Request<Body>)->Response {
    let (parts,body)=request.into_parts();
    if parts.method!=Method::POST {return crate::mobile_http::method_response(&parts.method,"POST, OPTIONS");}
    let now=SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    let claims=web_cookie(&parts.headers).and_then(|cookie|state.keys.read_web(cookie,now));
    if !state.enabled || !claims.as_ref().and_then(|claims|claims.get("admin")).is_some_and(|value|value==true) {
        return forbidden();
    }
    // The old admin form has no general CSRF field. Current browsers send an Origin on form POST.
    // Prefer an explicit CSRF token once all admin forms have moved off the Flask template.
    if !header_text(&parts.headers,"origin").is_some_and(|origin|bool::from(origin.as_bytes().ct_eq(state.origin.as_bytes()))) {
        return forbidden();
    }
    let path=parts.uri.path();
    if let Some(replies)=state.replies.as_ref() && let Some(path)=path.strip_prefix("/admin/replies/") {
        let Some((kind,id))=path.split_once('/').and_then(|(kind,id)|id.parse::<i64>().ok().map(|id|(kind,id))) else{return StatusCode::NOT_FOUND.into_response();};
        let expected=claims.as_ref().and_then(|claims|claims.get("admin_reply_csrf")).and_then(serde_json::Value::as_str).unwrap_or_default();
        let Ok(body)=to_bytes(body,64*1024).await else{return StatusCode::PAYLOAD_TOO_LARGE.into_response();};
        let form:std::collections::HashMap<_,_>=form_urlencoded::parse(&body).collect();
        let value=|key:&str|form.get(key).map_or("",|value|value.as_ref());
        if expected.is_empty()||!bool::from(expected.as_bytes().ct_eq(value("csrf").as_bytes())){return forbidden();}
        if !state.reply_limit.allow(expected,std::time::Instant::now()){return html(StatusCode::TOO_MANY_REQUESTS,"Слишком много ответов — подожди немного");}
        return match replies.send(&state.db,kind,id,value("body"),value("request_id"),now as i64).await {
            Ok(crate::admin_reply::ResultCode::Done)=>{
                let target=if kind=="report"{"/admin#reports"}else{"/admin#support"};
                (StatusCode::FOUND,[(header::LOCATION,target),(header::CACHE_CONTROL,"no-store")],"").into_response()
            }
            Ok(crate::admin_reply::ResultCode::Missing)=>html(StatusCode::NOT_FOUND,"Обращение не найдено"),
            Ok(crate::admin_reply::ResultCode::Invalid(error))=>html(StatusCode::BAD_REQUEST,error),
            Err(_)=>html(StatusCode::SERVICE_UNAVAILABLE,"сервер временно недоступен"),
        };
    }
    let action=match path {
        "/admin/premium"=>Some(Action::Premium),"/admin/report-ban-clear"=>Some(Action::ClearByName),"/admin/promo"=>Some(Action::Promo),
        "/admin/tasks/import-support"=>Some(Action::Import),
        _=>path.strip_prefix("/admin/users/").and_then(|path|path.strip_suffix("/report-ban-clear")).and_then(|id|id.parse().ok()).map(Action::ClearById)
            .or_else(||path.strip_prefix("/admin/tasks/").and_then(|path|path.strip_suffix("/decision")).and_then(|id|id.parse().ok()).map(Action::Decision)),
    };
    let Some(action)=action else {return StatusCode::NOT_FOUND.into_response();};
    let Ok(body)=to_bytes(body,64*1024).await else {return StatusCode::PAYLOAD_TOO_LARGE.into_response();};
    let mut form=std::collections::HashMap::new();
    for (key,value) in form_urlencoded::parse(&body) {form.entry(key.into_owned()).or_insert(value.into_owned());}
    let threshold=state.threshold;
    let result=state.db.transaction(move |tx|Box::pin(async move {
        let get=|key:&str|form.get(key).map_or("",String::as_str).trim();
        let bad=|text|Outcome::Failed(StatusCode::BAD_REQUEST,text);
        let missing=|text|Outcome::Failed(StatusCode::NOT_FOUND,text);
        match action {
            Action::Import=>{
                crate::task_triage::import(tx,now as i64,threshold).await?;
                return Ok(Outcome::Done("/admin#tasks"));
            }
            Action::Decision(id)=>{
                let decision=get("decision").to_lowercase();
                if !matches!(decision.as_str(),"approved"|"rejected"|"done"|"candidate"){return Ok(bad("неизвестное решение"));}
                let count=tx.execute("UPDATE feature_tasks SET status=$2,updated_at=$3::bigint,
                    approved_at=CASE WHEN $2='approved' THEN $3::bigint ELSE approved_at END,
                    approved_by=CASE WHEN $2='approved' THEN 'admin' ELSE approved_by END WHERE id=$1::bigint",&[&id,&decision,&(now as i64)]).await?;
                return Ok(if count==1{Outcome::Done("/admin#tasks")}else{missing("задача не найдена")});
            }
            Action::Promo=>{
                let days=if get("days").is_empty(){Some(90)}else{get("days").parse::<i32>().ok()};
                let uses=if get("max_uses").is_empty(){Some(0)}else{get("max_uses").parse::<i32>().ok()};
                let (Some(days),Some(uses))=(days,uses) else {return Ok(bad("дни и лимит — числа"));};
                let code=get("code").to_uppercase().chars().filter(|c|c.is_alphanumeric() || matches!(c,'-'|'_')).collect::<String>();
                if !(4..=24).contains(&code.chars().count()){return Ok(bad("код: 4–24 символа"));}
                if !(1..=36500).contains(&days){return Ok(bad("дни: 1–36500"));}
                if uses<0{return Ok(bad("лимит использований не может быть меньше 0"));}
                let count=tx.execute("INSERT INTO promo_codes(code,days,max_uses,uses) VALUES ($1,$2,$3,0) ON CONFLICT(code) DO NOTHING",&[&code,&days,&uses]).await?;
                return Ok(if count==1{Outcome::Done("/admin")}else{bad("такой код уже есть")});
            }
            _=>(),
        }
        let by_id=matches!(action,Action::ClearById(_));
        let who=get("who");
        if !by_id && (who.is_empty() || who.contains('\0')){return Ok(bad("укажи почту или id"));}
        let id=if let Action::ClearById(id)=action {Some(id)}else{who.parse::<i64>().ok()};
        let row=tx.query_opt("SELECT id::bigint,email,COALESCE(is_seed,0)!=0,COALESCE(report_banned_at,0)!=0 FROM users
            WHERE id=$1::bigint OR ($3 AND (lower(email)=$2 OR (name=$4 AND COALESCE(is_seed,0)=0)))
            ORDER BY COALESCE(id=$1::bigint,false) DESC,(lower(email)=$2) DESC,id LIMIT 1 FOR UPDATE",&[&id,&who.to_lowercase(),&!by_id,&who]).await?;
        let Some(row)=row else {return Ok(missing("человека нет"));};
        let uid:i64=row.get(0);
        if matches!(action,Action::Premium){
            let email:&str=row.get(1);
            if email.ends_with("@wiring.guest")||email=="demo@wiring.app"||row.get::<_,bool>(2){return Ok(bad("премиум только живым аккаунтам"));}
            let days=if get("days").is_empty(){Some(30)}else{get("days").parse::<i64>().ok()};
            let Some(days)=days.filter(|value|*value <= 36500) else {return Ok(bad("дни — число"));};
            if days<=0 {tx.execute("UPDATE users SET premium_until=0,incognito=0,paused=0 WHERE id=$1::bigint",&[&uid]).await?;}
            else {tx.execute("UPDATE users SET premium_until=GREATEST(COALESCE(premium_until,0),$2::bigint)+$3::bigint WHERE id=$1::bigint",
                &[&uid,&(now as i64),&(days*86400)]).await?;}
            return Ok(Outcome::Done("/admin"));
        }
        if !row.get::<_,bool>(3)&&!by_id {return Ok(bad("автобана нет — анкета уже доступна"));}
        tx.execute("UPDATE users SET report_banned_at=NULL WHERE id=$1::bigint",&[&uid]).await?;
        Ok(Outcome::Done("/admin#bans"))
    })).await;
    let mut response=match result {
        Ok(axum::Json(Outcome::Done(target)))=>(StatusCode::FOUND,[(header::LOCATION,target)],"").into_response(),
        Ok(axum::Json(Outcome::Failed(status,text)))=>html(status,text),
        Err(_)=>html(StatusCode::SERVICE_UNAVAILABLE,"сервер временно недоступен"),
    };
    response.headers_mut().insert(header::CACHE_CONTROL,axum::http::HeaderValue::from_static("no-store"));
    response
}

fn forbidden()->Response {html(StatusCode::FORBIDDEN,"доступ закрыт")}
fn html(status:StatusCode,text:&str)->Response {
    (status,[(header::CONTENT_TYPE,"text/html; charset=utf-8"),(header::CACHE_CONTROL,"no-store")],
        format!("<!doctype html><html lang=ru><meta charset=utf-8><title>WIRING admin</title><p>{}</p><p><a href=/admin>Вернуться к управлению</a></p>",crate::public_http::html_escape(text))).into_response()
}
