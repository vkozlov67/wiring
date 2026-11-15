use std::{collections::HashMap, env};

use axum::{
    Json, body::{Body, to_bytes},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde_json::{Map, Value, json};
use subtle::ConstantTimeEq;

use crate::{
    chat_write::Writer,
    mobile_http::{MAX_BODY, too_large, token_hash},
    oauth_identity::{self, Text},
    oauth_start::{b64, random, redirect, urlencode},
    password::hash_password,
    public_http::{header_text, html_escape},
    registration::{NewUser, Registration},
    signed_session::SessionKeys,
};

const OPEN: &str = "/api/auth/native/open";
const EXPIRED: Text = ("Sign-in expired. Please start again.","Вход устарел. Начни заново.");
const CANCEL: Text = ("Sign-in cancelled. You can try again.","Вход отменён. Можно попробовать снова.");
const NOT_CONFIGURED: Text = ("This sign-in method is not configured yet.","Этот способ входа пока не настроен.");
const EXISTING: Text = ("This email is already registered. Sign in with your password or use Forgot password.",
    "Эта почта уже зарегистрирована. Войди с паролем или восстанови его через «Забыли пароль».");
const NOT_FOUND: Text = ("No account yet. Choose Create profile and sign up with this service.",
    "Аккаунта ещё нет. Открой «Создать профиль» и зарегистрируйся через этот сервис.");
const DELETED: Text = ("The account was deleted and its recovery period has ended.","Аккаунт удалён, срок восстановления прошёл.");
const CONFLICT: Text = ("This account is already linked to another sign-in method. Please use the previous method.",
    "Этот аккаунт уже связан с другим входом. Используй прежний способ входа.");

pub(crate) struct Callback {
    http: reqwest::Client,
    secrets: HashMap<String,String>,
    registration: std::sync::Arc<Registration>,
}

pub(crate) struct Context<'a> {
    pub db: &'a Writer,
    pub keys: &'a SessionKeys,
    pub session: &'a mut Map<String, Value>,
    pub changed: &'a mut bool,
    pub headers: &'a HeaderMap,
    pub query: &'a str,
    pub stamp: i64,
    pub locale: &'static str,
}

struct Flow {
    verifier: String,
    mode: String,
    referral: String,
    redirect_uri: String,
    stamp: i64,
    challenge: Option<String>,
    native: Option<String>,
    apple_payload: Option<String>,
    managed: bool,
    client_id: Option<String>,
    consent: bool,
}

enum Outcome {
    Rejected(Text),
    SignedIn { uid: i64, onboard: bool, created: bool },
}

impl Callback {
    pub(crate) fn load()->Result<Self,Box<dyn std::error::Error>> {
        let mut secrets=HashMap::new();
        for provider in ["google","yandex","apple"] {
            let key=provider.to_ascii_uppercase();
            let value=env::var(format!("{key}_CLIENT_SECRET")).unwrap_or_default().trim().to_owned();
            if !value.is_empty() {secrets.insert(provider.into(),value);}
        }
        Ok(Self {http:oauth_identity::client()?,secrets,registration:std::sync::Arc::new(Registration::load()?)})
    }

    pub(crate) async fn finish(&self,mut ctx:Context<'_>,provider:&str,client:Option<&str>,post:bool,body:Body)
        ->Result<Response,StatusCode>
    {
        let Some((client,secret))=client.zip(self.secrets.get(provider)) else {return Ok(failure(&mut ctx,NOT_CONFIGURED,"login",None));};
        let lang=ctx.locale;
        let now=ctx.stamp;
        if post {
            if provider!="apple" {return Ok(error(StatusCode::METHOD_NOT_ALLOWED,lang,("Invalid request.","Некорректный запрос.")));}
            let form=match read_form(ctx.headers,body).await {Ok(form)=>form,Err(response)=>return Ok(response)};
            let state:String=form.get("state").map_or("",String::as_str).chars().take(512).collect();
            let field=|key|form.get(key).map_or("",String::as_str).chars().take(4096).collect::<String>();
            let payload=json!({"code":field("code"),"error":field("error")}).to_string();
            let hash=token_hash(&state);
            let stored=ctx.db.transaction(move |tx|Box::pin(async move {
                Ok(tx.execute("UPDATE oauth_flows SET callback_payload=$2
                    WHERE state_hash=$1 AND provider='apple' AND created_at >= $3::bigint AND callback_payload IS NULL",
                    &[&hash,&payload,&(now-600)]).await?==1)
            })).await?.0;
            if !stored {return Ok(error(StatusCode::BAD_REQUEST,lang,EXPIRED));}
            let mut response=redirect(&format!("/api/auth/apple/callback?{}",urlencode(&[("state",&state)])),lang);
            *response.status_mut()=StatusCode::SEE_OTHER;
            return Ok(response);
        }
        let values=parse_form(ctx.query.as_bytes());
        let state_value:String=values.get("state").map_or("",String::as_str).chars().take(512).collect();
        let browser=ctx.session.get("oauth_browser").and_then(Value::as_str).unwrap_or("").to_owned();
        if browser.is_empty() {return Ok(failure(&mut ctx,EXPIRED,"login",None));}
        let hash=token_hash(&state_value);
        let provider_key=provider.to_owned();
        let flow=ctx.db.transaction(move |tx|Box::pin(async move {
            let row=tx.query_opt("SELECT browser,verifier,mode,referral,redirect_uri,created_at::bigint,native_challenge,native_state,
                callback_payload,registration_managed!=0,analytics_client_id,analytics_consent!=0
                FROM oauth_flows WHERE state_hash=$1 AND provider=$2 FOR UPDATE",&[&hash,&provider_key]).await?;
            let Some(row)=row else {return Ok(None);};
            if !bool::from(browser.as_bytes().ct_eq(row.get::<_,&str>(0).as_bytes())) {return Ok(None);}
            tx.execute("DELETE FROM oauth_flows WHERE state_hash=$1",&[&hash]).await?;
            Ok(Some(Flow{verifier:row.get(1),mode:row.get(2),referral:row.get(3),redirect_uri:row.get(4),stamp:row.get(5),
                challenge:row.get(6),native:row.get(7),apple_payload:row.get(8),managed:row.get(9),client_id:row.get(10),consent:row.get(11)}))
        })).await?.0;
        let Some(flow)=flow else {return Ok(failure(&mut ctx,EXPIRED,"login",None));};
        ctx.session.remove("oauth_browser");
        *ctx.changed=true;
        let native=flow.native.as_deref();
        let mode=flow.mode.clone();
        if now-flow.stamp>600 || (provider=="apple" && flow.apple_payload.is_none()) {
            return Ok(failure(&mut ctx,EXPIRED,&mode,native));
        }
        let parsed=if provider=="apple" {flow.apple_payload.as_ref().and_then(|value|serde_json::from_str::<HashMap<String,String>>(value).ok())}
            else {Some(values)};
        let Some(parsed)=parsed else {return Ok(failure(&mut ctx,EXPIRED,&mode,native));};
        let code=parsed.get("code").map_or("",String::as_str);
        if code.is_empty() || code.len()>4096 || parsed.get("error").is_some_and(|s|!s.is_empty()) {
            return Ok(failure(&mut ctx,CANCEL,&mode,native));
        }
        let identity=match oauth_identity::exchange(&self.http,provider,client,secret,code,&flow.redirect_uri,&flow.verifier,now).await {
            Ok(value)=>value,Err(message)=>return Ok(failure(&mut ctx,message,&mode,native)),
        };
        let registration=self.registration.clone();
        let registration_managed=flow.managed && registration.managed();
        let is_native=flow.challenge.is_some();
        let native_code=if is_native {Some(random::<32>()?)} else {None};
        let native_hash=native_code.as_ref().map(|code|token_hash(code));
        let provider=provider.to_owned();
        let state_native=flow.native.clone();
        let password=if mode=="register" {hash_password(random::<48>()?).await?} else {String::new()};
        let outcome=ctx.db.transaction(move |tx|Box::pin(async move {
            tx.query_one("SELECT pg_advisory_xact_lock(hashtext($1))",&[&identity.email]).await?;
            let mut row=tx.query_opt("SELECT u.id::bigint,COALESCE(u.deleted_at,0)::bigint,u.onboard_done!=0,
                COALESCE(u.email_verified_at,0)!=0,u.is_seed!=0 FROM users u JOIN oauth_identities i ON i.user_id=u.id
                WHERE i.provider=$1 AND i.subject=$2 FOR UPDATE OF u",&[&provider,&identity.subject]).await?;
            let mut created=false;
            let link=row.is_none();
            if link {
                row=tx.query_opt("SELECT id::bigint,COALESCE(deleted_at,0)::bigint,onboard_done!=0,
                    COALESCE(email_verified_at,0)!=0,is_seed!=0 FROM users WHERE email=$1 FOR UPDATE",&[&identity.email]).await?;
                if row.as_ref().is_some_and(|r|!identity.authoritative || !r.get::<_,bool>(3) || r.get::<_,bool>(4)) {
                    return Ok(Outcome::Rejected(EXISTING));
                }
            }
            let (uid,deleted,onboard)=if let Some(row)=row {
                (row.get::<_,i64>(0),row.get::<_,i64>(1),row.get::<_,bool>(2))
            } else {
                if flow.mode!="register" {return Ok(Outcome::Rejected(NOT_FOUND));}
                let Some(uid)=registration.create(tx,NewUser{email:identity.email,name:identity.name,hash:password,
                    locale:lang,now,verified:true,marketing:false,channel:format!("{}_oauth_{provider}",if is_native {"native"} else {"web"}),
                    managed:registration_managed,consent:flow.consent,client_id:flow.client_id,referral:flow.referral}).await?
                else {return Ok(Outcome::Rejected(EXISTING));};
                created=true;
                (uid,0,false)
            };
            if deleted!=0 && now-deleted>7*86_400 {return Ok(Outcome::Rejected(DELETED));}
            if link {
                let saved=tx.execute("INSERT INTO oauth_identities(provider,subject,user_id)
                    VALUES($1,$2,$3::bigint) ON CONFLICT DO NOTHING",&[&provider,&identity.subject,&uid]).await?;
                if saved!=1 {
                    if created {return Err("Concurrent OAuth identity registration".into());}
                    return Ok(Outcome::Rejected(CONFLICT));
                }
            }
            tx.execute("UPDATE users SET deleted_at=NULL,last_seen=$2::bigint WHERE id=$1::bigint",&[&uid,&now]).await?;
            if let (Some(hash),Some(challenge))=(native_hash,flow.challenge) {
                tx.execute("DELETE FROM oauth_native_codes WHERE created_at < $1::bigint",&[&(now-180)]).await?;
                tx.execute("INSERT INTO oauth_native_codes(code_hash,user_id,challenge,created_at)
                    VALUES($1,$2::bigint,$3,$4::bigint)",&[&hash,&uid,&challenge,&now]).await?;
            }
            Ok(Outcome::SignedIn{uid,onboard,created})
        })).await;
        match outcome {
            Ok(Json(Outcome::Rejected(message)))=>Ok(failure(&mut ctx,message,&mode,state_native.as_deref())),
            Err(_)=>Ok(failure(&mut ctx,CONFLICT,&mode,state_native.as_deref())),
            Ok(Json(Outcome::SignedIn{uid,onboard,created}))=>{
                if let (Some(code),Some(state))=(native_code,state_native) {
                    return Ok(native_finish(&ctx,&[("code",&code),("state",&state)]));
                }
                ctx.session.clear();
                ctx.session.insert("_permanent".into(),true.into());
                ctx.session.insert("uid".into(),uid.into());
                if created {
                    ctx.session.insert("registration_created".into(),true.into());
                    if registration_managed {
                        ctx.session.insert("registration_receipt".into(),ctx.keys.sign_registration(uid,now as u64).into());
                    }
                }
                Ok(redirect(if onboard {"/feed"} else {"/me"},lang))
            }
        }
    }

    pub(crate) async fn open(&self,ctx:Context<'_>,body:Body)->Result<Response,StatusCode> {
        let form=match read_form(ctx.headers,body).await {Ok(form)=>form,Err(response)=>return Ok(response)};
        let state=form.get("state").map_or("",String::as_str);
        let code=form.get("code").map_or("",String::as_str);
        let message:String=form.get("error").map_or("",String::as_str).chars()
            .filter(|c|*c>'\u{1f}' && *c!='\u{7f}').take(300).collect();
        let lang=ctx.locale;
        let expired=||(StatusCode::BAD_REQUEST,[(header::CONTENT_TYPE,"text/html; charset=utf-8")],
            page(lang,&format!("<h1>{}</h1>",html_escape(translate(lang,EXPIRED))))).into_response();
        if !b64(state,16,100) {return Ok(expired());}
        let params=if !code.is_empty() {
            if !b64(code,20,128) {return Ok(expired());}
            let hash=token_hash(code);
            let now=ctx.stamp;
            let found=ctx.db.transaction(move |tx|Box::pin(async move {
                Ok(tx.query_opt("SELECT 1 FROM oauth_native_codes WHERE code_hash=$1 AND created_at >= $2::bigint",
                    &[&hash,&(now-180)]).await?.is_some())
            })).await?.0;
            if !found {return Ok(expired());}
            [("code",code),("state",state)]
        } else if !message.is_empty() {[("error",message.as_str()),("state",state)]}
        else {return Ok(expired());};
        if form.get("launch").is_some_and(|v|v=="1") {
            return Ok(redirect(&format!("intent://oauth/?{}#Intent;scheme=wiring;package=date.wiring.app;action=android.intent.action.VIEW;end",urlencode(&params)),lang));
        }
        Ok(form_page(lang,translate(lang,("Open the app","Открыть приложение")),
            translate(lang,("Tap the button once. WIRING will finish signing you in.","Нажми кнопку один раз. WIRING заберёт вход.")),
            translate(lang,("Open the app","Открыть приложение")),&params,true))
    }
}

fn translate<'a>(lang:&str,(en,ru):(&'a str,&'a str))->&'a str {if lang=="en" {en} else {ru}}
fn error(status:StatusCode,lang:&str,text:Text)->Response {(status,Json(json!({"ok":false,"error":translate(lang,text)}))).into_response()}
fn failure(ctx:&mut Context<'_>,text:Text,mode:&str,native:Option<&str>)->Response {
    let message=translate(ctx.locale,text);
    if let Some(state)=native {return native_finish(ctx,&[("error",message),("state",state)]);}
    ctx.session.insert("oauth_error".into(),message.into());
    *ctx.changed=true;
    redirect(if mode=="register" {"/register"} else {"/login"},ctx.locale)
}
fn native_finish(ctx:&Context<'_>,params:&[(&str,&str)])->Response {
    let lang=ctx.locale;
    let ua=header_text(ctx.headers,"user-agent").unwrap_or("");
    if !ua.contains("Android") && !ua.contains("X11; Linux") {
        return redirect(&format!("wiring://oauth?{}",urlencode(params)),lang);
    }
    let title=params.iter().find(|(key,_)|*key=="error").map(|(_,value)|*value)
        .unwrap_or_else(||translate(lang,("Sign-in confirmed","Вход подтверждён")));
    form_page(lang,title,translate(lang,("Tap the button. The next page opens without Google.",
        "Нажми кнопку. Откроется следующая страница, уже без Google.")),translate(lang,("Continue","Продолжить")),params,false)
}
fn form_page(lang:&str,title:&str,description:&str,button:&str,params:&[(&str,&str)],launch:bool)->Response {
    let fields:String=params.iter().map(|(key,value)|format!("<input type=\"hidden\" name=\"{}\" value=\"{}\">",html_escape(key),html_escape(value))).collect();
    let launch=if launch {"<input type=\"hidden\" name=\"launch\" value=\"1\">"} else {""};
    let main=format!("<h1>{}</h1><p>{}</p><form method=\"post\" action=\"{OPEN}\" autocomplete=\"off\">{fields}{launch}<button type=\"submit\">{}</button></form>",
        html_escape(title),html_escape(description),html_escape(button));
    ([(header::CONTENT_TYPE,"text/html; charset=utf-8")],page(lang,&main)).into_response()
}
fn page(lang:&str,content:&str)->String {
    format!(r#"<!doctype html>
<html lang="{lang}">
<head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<meta name="referrer" content="no-referrer"><title>WIRING</title>
<style>
body {{ margin:0; min-height:100vh; display:grid; place-items:center; background:#e9ebf3; color:#1c1b22;
font:18px/1.45 -apple-system,BlinkMacSystemFont,"Segoe UI",sans-serif; }}
main {{ width:min(100% - 32px, 420px); }}
h1 {{ font-size:22px; font-weight:650; margin:0 0 12px; }}
p {{ margin:0 0 20px; }}
button {{ display:block; width:100%; box-sizing:border-box; text-align:center; background:#6d5efc;
color:#fff; padding:14px 18px; border-radius:999px; border:0; font:inherit; }}
</style></head><body><main>{content}</main></body></html>"#)
}
fn parse_form(body:&[u8])->HashMap<String,String> {
    let mut out=HashMap::new();
    for (key,value) in form_urlencoded::parse(body) {out.entry(key.into_owned()).or_insert(value.into_owned());}
    out
}
async fn read_form(headers:&HeaderMap,body:Body)->Result<HashMap<String,String>,Response> {
    if !header_text(headers,"content-type").is_some_and(|v|v.split(';').next()==Some("application/x-www-form-urlencoded")) {
        return Ok(HashMap::new());
    }
    to_bytes(body,MAX_BODY).await.map(|bytes|parse_form(&bytes)).map_err(|_|too_large())
}
