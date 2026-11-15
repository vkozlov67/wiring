use std::{collections::HashMap,env,error::Error,fs,path::Path,sync::Arc,time::{Duration,SystemTime,UNIX_EPOCH}};

use axum::{Router,body::{Body,to_bytes},extract::State,http::{HeaderMap,HeaderValue,Method,Request,StatusCode,header},
    response::{IntoResponse,Response},routing::any};
use base64::{Engine,engine::general_purpose::URL_SAFE_NO_PAD};
use minijinja::{Environment,Value as Jinja};
use serde_json::{Value,json};
use subtle::ConstantTimeEq;

use crate::{admin_stats,chat_write::Writer,login::LoginLimit,public_http::{header_text,request_language},
    signed_session::SessionKeys,spa_http::web_cookie};

struct Panel {
    db:Arc<Writer>, keys:SessionKeys, tokens:Vec<String>, origin:String, limit:LoginLimit,
    templates:[Environment<'static>;2], messages:Arc<HashMap<String,String>>, catalogs:[Value;2], threshold:f32,
}

pub(crate) fn routes(writer:Option<Arc<Writer>>,directory:&Path)->Result<Router,Box<dyn Error>> {
    if env::var("WIRING_PUBLIC_ADMIN_PAGE").as_deref()!=Ok("1") {return Ok(Router::new());}
    let key=env::var("APP_SECRET_KEY")?;
    if key.trim().is_empty(){return Err("Admin page requires signed sessions".into());}
    let origin=env::var("SITE_URL").unwrap_or_else(|_|"https://wiring.club".into()).trim_end_matches('/').to_owned();
    let parsed=reqwest::Url::parse(&origin)?;
    if parsed.origin().ascii_serialization()!=origin || parsed.scheme()!="https" {return Err("Admin page requires an HTTPS site origin".into());}
    let tokens=env::var("ADMIN_TOKEN").unwrap_or_default().split(',').map(str::trim).filter(|token|!token.is_empty()).map(str::to_owned).collect();
    let messages:Arc<HashMap<String,String>>=Arc::new(serde_json::from_slice(&fs::read(directory.join("admin-i18n-en.json"))?)?);
    let template=fs::read_to_string(directory.join("admin.html"))?;
    let header=fs::read_to_string(directory.join("admin-header.html"))?;
    let make=|english|->Result<Environment<'static>,minijinja::Error> {
        let mut environment=Environment::new();
        environment.add_template_owned("admin.html",template.clone())?;
        environment.add_template_owned("components/header.html",header.clone())?;
        let messages=Arc::clone(&messages);
        environment.add_function("_",move |text:String|translate(&messages,english,&text));
        environment.add_function("reply_nonce",|| nonce(24).unwrap_or_default());
        environment.add_filter("format",|format:String,number:f64|if format=="%.1f"{format!("{number:.1}")}else{number.to_string()});
        Ok(environment)
    };
    let catalogs=[serde_json::from_slice(&fs::read(directory.join("api-catalog-ru.json"))?)?,
        serde_json::from_slice(&fs::read(directory.join("api-catalog-en.json"))?)?];
    let templates=[make(false)?,make(true)?];
    let panel=Panel{db:writer.ok_or("Admin page requires PostgreSQL")?,keys:SessionKeys::new(key.as_bytes()),tokens,origin,
        limit:LoginLimit::configured(Duration::from_secs(3600),12),templates,messages,catalogs,
        threshold:env::var("SUPPORT_TASK_THRESHOLD").ok().and_then(|raw|raw.parse::<f32>().ok()).filter(|n|n.is_finite()).unwrap_or(6.0).clamp(0.0,10.0)};
    Ok(Router::new().route("/admin",any(page)).route("/admin/export/marketing-audience",any(export)).with_state(Arc::new(panel)))
}

fn translate(messages:&HashMap<String,String>,english:bool,text:&str)->String {
    if !english {return text.to_owned();}
    let key=text.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed=text.trim();
    if trimmed.is_empty(){return text.to_owned();}
    messages.get(&key).map_or_else(||text.to_owned(),|value|format!("{}{value}{}",&text[..text.len()-text.trim_start().len()],&text[text.trim_end().len()..]))
}

fn nonce(size:usize)->Option<String> {
    let mut bytes=vec![0;size];getrandom::fill(&mut bytes).ok()?;
    Some(URL_SAFE_NO_PAD.encode(bytes))
}

fn signed(panel:&Panel,headers:&HeaderMap,now:u64)->serde_json::Map<String,Value> {
    web_cookie(headers).and_then(|cookie|panel.keys.read_web(cookie,now)).unwrap_or_default()
}

fn set_cookie(panel:&Panel,response:&mut Response,claims:&serde_json::Map<String,Value>,now:u64) {
    let token=panel.keys.sign_web(claims,now);
    if token.len()>3800{return;}
    let expires=if claims.get("_permanent")==Some(&Value::Bool(true)) {
        format!("; Expires={}",httpdate::fmt_http_date(UNIX_EPOCH+Duration::from_secs(now+crate::signed_session::WEB_TTL)))
    }else{String::new()};
    if let Ok(value)=HeaderValue::from_str(&format!("wiring_session={token}{expires}; Secure; HttpOnly; Path=/; SameSite=Lax")) {
        response.headers_mut().append(header::SET_COOKIE,value);
    }
}

fn no_store(mut response:Response)->Response {
    response.headers_mut().insert(header::CACHE_CONTROL,HeaderValue::from_static("no-store"));
    response.headers_mut().insert(header::REFERRER_POLICY,HeaderValue::from_static("no-referrer"));
    response
}

async fn page(State(state):State<Arc<Panel>>,request:Request<Body>)->Response {
    if state.tokens.is_empty(){return no_store(StatusCode::NOT_FOUND.into_response());}
    let (parts,body)=request.into_parts();
    let (lang,_)=request_language(parts.uri.query(),&parts.headers);
    let english=usize::from(lang=="en");
    let now=SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    let mut claims=signed(&state,&parts.headers,now);
    let query=parts.uri.query().unwrap_or_default();
    let logout=form_urlencoded::parse(query.as_bytes()).any(|(key,value)|key=="out"&&!value.is_empty());
    if logout && matches!(parts.method,Method::GET|Method::HEAD) {
        claims.remove("admin");claims.remove("admin_reply_csrf");claims.remove("admin_login_csrf");
        let mut response=no_store((StatusCode::FOUND,[(header::LOCATION,"/admin")],"").into_response());
        set_cookie(&state,&mut response,&claims,now);
        return response;
    }
    let mut error="";
    let mut status=StatusCode::OK;
    if parts.method==Method::POST {
        let origin=header_text(&parts.headers,"origin").unwrap_or_default();
        let ip=header_text(&parts.headers,"x-forwarded-for").unwrap_or("x").split(',').next().unwrap_or("x").trim();
        if !state.limit.allow(ip,std::time::Instant::now()) {
            error="подожди немного";status=StatusCode::TOO_MANY_REQUESTS;
        }else if let Ok(data)=to_bytes(body,64*1024).await {
            let parameters:HashMap<_,_>=form_urlencoded::parse(&data).collect();
            let token=parameters.get("token").map_or("",|v|v.as_ref());
            let csrf=claims.remove("admin_login_csrf").and_then(|value|value.as_str().map(str::to_owned)).unwrap_or_default();
            let provided=parameters.get("csrf").map_or("",|v|v.as_ref());
            if origin==state.origin && !csrf.is_empty() && bool::from(csrf.as_bytes().ct_eq(provided.as_bytes())) &&
                state.tokens.iter().fold(false,|matched,item|matched | bool::from(token.as_bytes().ct_eq(item.as_bytes()))) {
                claims.insert("admin".into(),Value::Bool(true));
                let mut response=no_store((StatusCode::SEE_OTHER,[(header::LOCATION,"/admin")],"").into_response());
                set_cookie(&state,&mut response,&claims,now);
                return response;
            }
            error="не тот токен или логин/пароль";status=StatusCode::FORBIDDEN;
        }else{return no_store(StatusCode::PAYLOAD_TOO_LARGE.into_response());}
    }else if !matches!(parts.method,Method::GET|Method::HEAD) {
        return no_store(crate::mobile_http::method_response(&parts.method,"GET, HEAD, POST, OPTIONS"));
    }
    let authed=claims.get("admin")==Some(&Value::Bool(true));
    let Some(csrf)=nonce(32) else{return no_store(StatusCode::SERVICE_UNAVAILABLE.into_response());};
    let mut result=json!({});
    if authed {
        claims.entry("admin_reply_csrf").or_insert(Value::String(csrf));
        let source=state.clone();
        let rows=state.db.transaction(move |tx|Box::pin(async move {
            let translation=|text:&str|translate(&source.messages,english!=0,text);
            admin_stats::dashboard(tx,now as i64,&source.catalogs[english],&translation).await
        })).await;
        let Ok(axum::Json(rows))=rows else{return no_store(StatusCode::SERVICE_UNAVAILABLE.into_response());};
        result=rows;
    }else{claims.insert("admin_login_csrf".into(),Value::String(csrf));}
    result["authed"]=json!(authed);result["language"]=json!(lang);result["local_admin_login"]=json!("");
    result["error"]=json!(translate(&state.messages,english!=0,error));result["task_threshold"]=json!(state.threshold);
    result["login_csrf"]=claims.get("admin_login_csrf").cloned().unwrap_or(Value::Null);
    result["reply_csrf"]=claims.get("admin_reply_csrf").cloned().unwrap_or(Value::Null);
    let html=match state.templates[english].get_template("admin.html").and_then(|template|template.render(Jinja::from_serialize(result))) {
        Ok(html)=>html,
        Err(error)=>{
            eprintln!("Admin template rendering failed ({:?}, line {:?})",error.kind(),error.line());
            return no_store(StatusCode::SERVICE_UNAVAILABLE.into_response());
        }
    };
    let mut response=no_store((status,[(header::CONTENT_TYPE,"text/html; charset=utf-8")],html).into_response());
    set_cookie(&state,&mut response,&claims,now);
    response
}

async fn export(State(state):State<Arc<Panel>>,method:Method,headers:HeaderMap)->Response {
    if !matches!(method,Method::GET|Method::HEAD){return no_store(crate::mobile_http::method_response(&method,"GET, HEAD, OPTIONS"));}
    let now=SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    if state.tokens.is_empty() || signed(&state,&headers,now).get("admin")!=Some(&Value::Bool(true)) {
        return no_store(StatusCode::FORBIDDEN.into_response());
    }
    let result=state.db.transaction(move |tx|Box::pin(async move {
        let mut text=String::from("email\tname\n");
        for row in tx.query("SELECT email,name FROM users WHERE deleted_at IS NULL AND COALESCE(is_seed,0)=0
          AND email NOT LIKE '%@wiring.guest' AND email<>'demo@wiring.app' AND btrim(email)<>'' ORDER BY lower(email)",&[]).await? {
            let clean=|input:&str|input.replace(['\t','\r','\n']," ").trim().to_owned();
            text.push_str(&format!("{}\t{}\n",clean(row.get(0)),clean(row.get(1))));
        }
        Ok(text)
    })).await;
    let Ok(axum::Json(text))=result else{return no_store(StatusCode::SERVICE_UNAVAILABLE.into_response());};
    no_store(([(header::CONTENT_TYPE,"text/tab-separated-values; charset=utf-8"),
        (header::CONTENT_DISPOSITION,"attachment; filename=\"wiring-marketing-audience.tsv\"")],text).into_response())
}
