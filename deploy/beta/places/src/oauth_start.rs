use std::{collections::HashSet, env, error::Error, sync::Arc, time::{SystemTime, UNIX_EPOCH}};

use axum::{
    Json, Router,
    body::{Body, to_bytes},
    extract::State,
    http::{HeaderValue, Method, Request, StatusCode, header},
    response::{IntoResponse, Response},
    routing::any,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use pbkdf2::sha2::{Digest, Sha256};
use serde_json::{Value, json};
use subtle::ConstantTimeEq;

use crate::{
    chat_write::Writer,
    login::LoginLimit,
    mobile_http::{MAX_BODY, method_response, string_field, token_hash, too_large, unavailable, upgrade},
    public_http::{header_text, localize, request_language},
    registration::cookie,
    signed_session::{SessionKeys, WEB_TTL},
};

const PROVIDERS: &[(&str,&str,&str)] = &[
    ("google","https://accounts.google.com/o/oauth2/v2/auth","openid email profile"),
    ("yandex","https://oauth.yandex.ru/authorize","login:email login:info"),
    ("apple","https://appleid.apple.com/auth/authorize","email"),
];

struct OAuth {
    db: Arc<Writer>,
    keys: SessionKeys,
    providers: Vec<(&'static str, &'static str, &'static str, String)>,
    allowed: HashSet<String>,
    site: String,
    secure: bool,
    min_version: String,
    limit: LoginLimit,
    callback: Option<crate::oauth_callback::Callback>,
}

pub(crate) fn routes(writer: Option<Arc<Writer>>) -> Result<Router, Box<dyn Error>> {
    if env::var("WIRING_PUBLIC_OAUTH_START").as_deref() != Ok("1") { return Ok(Router::new()); }
    let secret = env::var("APP_SECRET_KEY")?;
    if secret.trim().is_empty() { return Err("OAuth requires the existing signing key".into()); }
    let site = env::var("SITE_URL").unwrap_or_else(|_| "https://wiring.date".into()).trim_end_matches('/').to_owned();
    let allowed = env::var("OAUTH_ALLOWED_ORIGINS").unwrap_or_else(|_| site.clone()).split(',')
        .map(|s| s.trim().trim_end_matches('/')).filter(|s| !s.is_empty()).map(str::to_owned).collect();
    let providers = PROVIDERS.iter().filter_map(|&(name, url, scope)| {
        let key = name.to_ascii_uppercase();
        let client = env::var(format!("{key}_CLIENT_ID")).unwrap_or_default().trim().to_owned();
        let configured = env::var(format!("WIRING_{key}_OAUTH_ENABLED")).as_deref() == Ok("1");
        (configured && !client.is_empty()).then_some((name,url,scope,client))
    }).collect();
    let state = OAuth {
        db: writer.ok_or("OAuth requires PostgreSQL")?, keys: SessionKeys::new(secret.as_bytes()),
        providers, site, allowed, secure: env::var("SESSION_COOKIE_SECURE").is_ok_and(|s| matches!(s.to_lowercase().as_str(),"1"|"true"|"yes")),
        min_version: env::var("MOBILE_MIN_VERSION").unwrap_or_default().trim().to_owned(), limit: LoginLimit::new(),
        callback: (env::var("WIRING_PUBLIC_OAUTH_CALLBACK").as_deref()==Ok("1"))
            .then(crate::oauth_callback::Callback::load).transpose()?,
    };
    let mut router = Router::new().route("/api/auth/providers", any(serve))
        .route("/api/auth/{provider}/start", any(serve))
        .route("/api/auth/native/browser", any(serve));
    if state.callback.is_some() {
        router = router.route("/api/auth/{provider}/callback", any(serve))
            .route("/api/auth/native/open", any(serve));
    }
    Ok(router.with_state(Arc::new(state)))
}

async fn serve(State(state): State<Arc<OAuth>>, request: Request<Body>) -> Response {
    let (parts, body) = request.into_parts();
    let (locale, persist) = request_language(parts.uri.query(), &parts.headers);
    let now = SystemTime::now();
    let stamp = now.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    let mut session = cookie(&parts.headers,"wiring_session").and_then(|token|state.keys.read_web(token,stamp)).unwrap_or_default();
    let mut changed = false;
    let path = parts.uri.path();
    let method = &parts.method;
    let ua = header_text(&parts.headers, "user-agent").map(str::trim).filter(|value| !value.is_empty());
    let ip = header_text(&parts.headers,"x-forwarded-for").unwrap_or("x").split(',').next().unwrap_or("x").trim();
    let en = locale == "en";
    let error = |status,en_msg,ru_msg| (status,Json(json!({"ok":false,"error":if en {en_msg} else {ru_msg}}))).into_response();
    let stale = || error(StatusCode::BAD_REQUEST,"Sign-in expired. Please start again.","Вход устарел. Начни заново.");
    let is_get = path == "/api/auth/providers" || path == "/api/auth/native/browser";
    let is_callback = path.ends_with("/callback");
    let is_open = path == "/api/auth/native/open";
    let valid = if is_callback { matches!(*method,Method::GET|Method::HEAD|Method::POST) }
        else if is_get {matches!(*method,Method::GET|Method::HEAD)} else {*method == Method::POST};
    let response = async {
        if let Some(upgrade) = upgrade(header_text(&parts.headers,"x-app-version").unwrap_or(""),&state.min_version,locale) {
            return Ok(upgrade);
        }
        if *method != Method::OPTIONS {
            if let Some(ua) = ua {
                let uid = session.get("uid").and_then(Value::as_i64);
                let visit = crate::visit::Visit::new(ip, ua);
                if !state.db.transaction(move |tx| Box::pin(crate::visit::track(tx,uid,stamp as i64,visit))).await?.0 {
                    session.clear();
                    changed = true;
                }
            }
        }
        if !valid {
            return Ok(method_response(method, if is_callback {"GET, HEAD, POST, OPTIONS"}
                else if is_get {"GET, HEAD, OPTIONS"} else {"POST, OPTIONS"}));
        }
        if is_callback || is_open {
            let callback = state.callback.as_ref().expect("OAuth callback configured");
            let provider = path.trim_start_matches("/api/auth/").trim_end_matches("/callback");
            let client = state.providers.iter().find(|row| row.0 == provider).map(|row| row.3.as_str());
            let context = crate::oauth_callback::Context {
                db: &state.db, keys: &state.keys, session: &mut session, changed: &mut changed,
                headers: &parts.headers, query: parts.uri.query().unwrap_or(""), stamp: stamp as i64, locale,
            };
            return if is_open { callback.open(context, body).await }
                else { callback.finish(context, provider, client, *method==Method::POST, body).await };
        }
        if path == "/api/auth/providers" {
            let found = session.remove("oauth_error");
            changed |= found.is_some();
            let message = found.unwrap_or_else(||json!(""));
            let providers: Vec<_> = state.providers.iter().map(|(name,_,_,_)|name).collect();
            return Ok(Json(json!({"ok":true,"providers":providers,"error_message":message})).into_response());
        }
        if path == "/api/auth/native/browser" {
            let mut pairs = form_urlencoded::parse(parts.uri.query().unwrap_or("").as_bytes());
            let state_value = pairs.find(|(k,_)| k=="state").map(|(_,v)|v.into_owned());
            let ticket = form_urlencoded::parse(parts.uri.query().unwrap_or("").as_bytes()).find(|(k,_)| k=="ticket").map(|(_,v)|v.into_owned());
            let (Some(state_value),Some(ticket)) = (state_value,ticket) else { return Ok(stale()); };
            if !b64(&state_value,43,43) || !b64(&ticket,43,43) { return Ok(stale()); }
            let hash = token_hash(&state_value);
            let row = state.db.transaction(move |tx|Box::pin(async move {
                let row = tx.query_opt("SELECT browser,provider,redirect_uri,verifier FROM oauth_flows
                     WHERE state_hash=$1 AND created_at >= $2::bigint AND native_challenge IS NOT NULL",&[&hash,&(stamp as i64-600)]).await?;
                Ok(row.map(|row|(row.get::<_,String>(0),row.get::<_,String>(1),row.get::<_,String>(2),row.get::<_,String>(3))))
            })).await?.0;
            let Some((browser,provider,callback,verifier)) = row else { return Ok(stale()); };
            if !bool::from(browser.as_bytes().ct_eq(ticket.as_bytes())) { return Ok(stale()); }
            let Some(config) = state.providers.iter().find(|item|item.0==provider) else {return Ok(stale());};
            session.insert("oauth_browser".into(),browser.into());
            changed=true;
            return Ok(redirect(&provider_url(config,&callback,&state_value,&verifier),locale));
        }
        let provider = path.trim_start_matches("/api/auth/").trim_end_matches("/start");
        let Some(config) = state.providers.iter().find(|entry|entry.0==provider) else {
            return Ok(error(StatusCode::SERVICE_UNAVAILABLE,"This sign-in method is not configured yet.","Этот способ входа пока не настроен."));
        };
        let scheme = if header_text(&parts.headers,"x-forwarded-proto")==Some("https") {"https"} else {"http"};
        let host = header_text(&parts.headers,"host").unwrap_or("");
        let origin = format!("{scheme}://{host}");
        if header_text(&parts.headers,"origin").is_some_and(|raw| !raw.is_empty() && raw.trim_end_matches('/') != origin) {
            return Ok(error(StatusCode::FORBIDDEN,"Open sign in on the WIRING website.","Открой вход на сайте WIRING."));
        }
        if !state.limit.allow(ip,std::time::Instant::now()) {
            return Ok(error(StatusCode::TOO_MANY_REQUESTS,"Too many attempts. Please wait.","Слишком много попыток. Подожди."));
        }
        let content_type = header_text(&parts.headers,"content-type").unwrap_or("").split(';').next().unwrap_or("").trim().to_ascii_lowercase();
        let data: Value = if content_type=="application/json" || (content_type.starts_with("application/") && content_type.ends_with("+json")) {
            match to_bytes(body,MAX_BODY).await {Ok(body)=>serde_json::from_slice(&body).unwrap_or(Value::Null),Err(_)=>return Ok(too_large())}
        } else {Value::Null};
        if !data.is_null() && !data.is_object() {
            return Ok(error(StatusCode::BAD_REQUEST,"Invalid request.","Некорректный запрос."));
        }
        let mode = string_field(&data,"mode");
        if !matches!(mode.as_str(),"login"|"register") {
            return Ok(error(StatusCode::BAD_REQUEST,"Choose sign in or sign up.","Выбери вход или регистрацию."));
        }
        if mode=="register" && (data.get("age_confirm")!=Some(&Value::Bool(true)) || data.get("privacy_confirm")!=Some(&Value::Bool(true))) {
            return Ok(error(StatusCode::BAD_REQUEST,"Confirm that you are 18 or older and accept the rules.","Подтверди возраст 18+ и согласие с правилами."));
        }
        let native = data.get("native")==Some(&Value::Bool(true));
        let challenge = data.get("code_challenge").and_then(Value::as_str).filter(|s|native && b64(s,43,43)).map(str::to_owned);
        let native_state = data.get("native_state").and_then(Value::as_str).filter(|s|native && b64(s,16,100)).map(str::to_owned);
        if native && (challenge.is_none() || native_state.is_none()) {
            return Ok(error(StatusCode::BAD_REQUEST,"Invalid app confirmation.","Некорректное подтверждение приложения."));
        }
        let origin = if state.allowed.contains(&origin) { &origin } else { &state.site };
        let callback = format!("{origin}/api/auth/{provider}/callback");
        let state_value = random::<32>()?;
        let browser = random::<32>()?;
        let verifier = random::<48>()?;
        let referral: String = string_field(&data,"ref").chars().filter(|c| *c!='\0').take(100).collect();
        let consent = !native && cookie(&parts.headers,"wiring-analytics-consent")==Some("granted");
        let cid = cookie(&parts.headers,"_ym_uid").filter(|id|consent && (1..=20).contains(&id.len()) && id.bytes().all(|b|b.is_ascii_digit())).map(str::to_owned);
        let managed = !native && header_text(&parts.headers,"x-wiring-registration-tracking")==Some("server-v1");
        let provider = config.0;
        let hash=token_hash(&state_value);
        let (db_browser,db_verifier,db_callback)=(browser.clone(),verifier.clone(),callback.clone());
        let _ = state.db.transaction(move |tx|Box::pin(async move {
            tx.execute("DELETE FROM oauth_flows WHERE created_at < $1::bigint",&[&(stamp as i64-600)]).await?;
            tx.execute("INSERT INTO oauth_flows(state_hash,browser,provider,verifier,mode,referral,redirect_uri,created_at,native_challenge,
              native_state,registration_managed,analytics_client_id,analytics_consent)
              VALUES($1,$2,$3,$4,$5,$6,$7,$8::bigint,$9,$10,$11,$12,$13)",
              &[&hash,&db_browser,&provider,&db_verifier,&mode,&referral,&db_callback,&(stamp as i64),&challenge,&native_state,&i32::from(managed),&cid,&i32::from(consent)]).await?;
            Ok(())
        })).await?;
        let url = if native {
            format!("{origin}/api/auth/native/browser?{}", urlencode(&[("state",&state_value),("ticket",&browser)]))
        } else {
            session.insert("oauth_browser".into(),browser.into());
            changed=true;
            provider_url(config,&callback,&state_value,&verifier)
        };
        Ok::<_,StatusCode>(Json(json!({"ok":true,"url":url})).into_response())
    }.await;
    let mut response = response.unwrap_or_else(|_|unavailable(locale));
    localize(&mut response,&parts.headers,locale,persist,true);
    if valid || *method==Method::OPTIONS {
        response.headers_mut().insert(header::CACHE_CONTROL,HeaderValue::from_static("no-store"));
        response.headers_mut().insert(header::REFERRER_POLICY,HeaderValue::from_static("no-referrer"));
    }
    let secure=if state.secure {"; Secure"} else {""};
    let permanent=session.get("_permanent")==Some(&Value::Bool(true));
    let value=if changed && session.is_empty() {
        Some(format!("wiring_session=; Expires=Thu, 01 Jan 1970 00:00:00 GMT; Max-Age=0{secure}; HttpOnly; Path=/; SameSite=Lax"))
    } else if changed || permanent {
        let signed=state.keys.sign_web(&session,stamp);
        let expires=if permanent {format!("; Expires={}",httpdate::fmt_http_date(now+std::time::Duration::from_secs(WEB_TTL)))} else {String::new()};
        (signed.len()<=3800).then(||format!("wiring_session={signed}{expires}{secure}; HttpOnly; Path=/; SameSite=Lax"))
    } else {None};
    if let Some(value)=value.and_then(|value|HeaderValue::from_str(&value).ok()) {
        response.headers_mut().append(header::SET_COOKIE,value);
    }
    response
}

pub(crate) fn b64(value:&str,min:usize,max:usize)->bool {
    (min..=max).contains(&value.len()) && value.bytes().all(|b|b.is_ascii_alphanumeric() || b"-_".contains(&b))
}
pub(crate) fn random<const N:usize>()->Result<String,StatusCode> {
    let mut bytes=[0;N];
    getrandom::fill(&mut bytes).map_err(|_|StatusCode::SERVICE_UNAVAILABLE)?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}
pub(crate) fn urlencode(pairs:&[(&str,&str)])->String {
    form_urlencoded::Serializer::new(String::new()).extend_pairs(pairs.iter().copied()).finish()
}
fn provider_url(config:&(&str,&str,&str,String),callback:&str,state:&str,verifier:&str)->String {
    let challenge=URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let mut pairs=vec![("response_type","code"),("client_id",config.3.as_str()),("redirect_uri",callback),("state",state),("scope",config.2)];
    if config.0=="apple" { pairs.extend([("response_mode","form_post"),("nonce",verifier)]); }
    else { pairs.extend([("code_challenge",challenge.as_str()),("code_challenge_method","S256")]); }
    if config.0=="google" {pairs.push(("prompt","select_account"));}
    format!("{}?{}",config.1,urlencode(&pairs))
}
pub(crate) fn redirect(location:&str,_lang:&str)->Response {
    let url=crate::public_http::html_escape(location);
    let body=format!("<!doctype html>\n<html lang=en>\n<title>Redirecting...</title>\n<h1>Redirecting...</h1>\n<p>You should be redirected automatically to the target URL: <a href=\"{url}\">{url}</a>. If not, click the link.\n");
    (StatusCode::FOUND,[(header::LOCATION,location),(header::CONTENT_TYPE,"text/html; charset=utf-8")],body).into_response()
}
