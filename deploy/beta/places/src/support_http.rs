use std::{env, error::Error, path::Path, time::{Duration, Instant}};

use axum::{Json, http::StatusCode, response::{IntoResponse, Response}};
use serde_json::{Value,json};

use crate::{chat_pair::{PairOutcome,report_pair},chat_write::Writer,login::LoginLimit,message_send::{integer,take}};

pub(crate) struct Support {
    http: reqwest::Client,
    email: Option<(String,String,String)>,
    telegram: Option<(String,String)>,
    origin: String,
    reports: LoginLimit,
    tickets: LoginLimit,
    forms: Option<[[String;2];2]>,
}

enum Reporter { Missing, Banned, Ready }

impl Support {
    pub(crate) fn load(directory:&Path, form:bool) -> Result<Self,Box<dyn Error>> {
        let setting = |name:&str| env::var(name).unwrap_or_default().trim().to_owned();
        let inbox = setting("SUPPORT_INBOX");
        let key = setting("RESEND_API_KEY");
        let smtp = setting("SMTP_HOST");
        if !inbox.is_empty() && key.is_empty() && !smtp.is_empty() {
            return Err("Configured support SMTP transport is not available in Rust".into());
        }
        let email = (!inbox.is_empty() && !key.is_empty()).then(|| (inbox,key,env::var("MAIL_FROM").unwrap_or_else(|_| "WIRING <noreply@wiring.date>".into())));
        let token = setting("TELEGRAM_BOT_TOKEN");
        let chat = setting("TELEGRAM_ADMIN_CHAT");
        let telegram = (!token.is_empty() && !chat.is_empty()).then_some((token,chat));
        if telegram.as_ref().is_some_and(|(token,_)| !token.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b,b':'|b'_'|b'-'))) {
            return Err("Invalid support Telegram configuration".into());
        }
        Ok(Self {
            http:reqwest::Client::builder().https_only(true).no_proxy().redirect(reqwest::redirect::Policy::none()).timeout(Duration::from_secs(12)).build()?,
            email,telegram,origin:env::var("SITE_URL").unwrap_or_else(|_| "https://wiring.date".into()).trim_end_matches('/').to_owned(),
            reports:LoginLimit::configured(Duration::from_secs(3600),5), tickets:LoginLimit::configured(Duration::from_secs(3600),5),
            forms:if form {
                let read=|name,lang| -> Result<String,Box<dyn Error>> {Ok(String::from_utf8(crate::public_http::render_html(directory,name,lang)?.to_vec())?)};
                Some([[read("support-form","ru")?,read("support-done","ru")?],[read("support-form","en")?,read("support-done","en")?]])
            } else {None},
        })
    }

    pub(crate) async fn ticket(&self,db:&Writer,uid:Option<i64>,fields:Value,ip:&str,now:i64,lang:&str,wants_json:bool) -> Result<Response,StatusCode> {
        let label=|en,ru| if lang=="en"{en}else{ru};
        let input=|key:&str| fields[key].as_str().unwrap_or("").trim();
        let body=input("body");
        let email=input("email");
        let error=if !self.tickets.allow(ip,Instant::now()) {Some(label("Too frequent. Please wait a little.","слишком часто — подожди немного"))}
            else if body.chars().count()<8 {Some(label("Please add a little more detail","напиши чуть подробнее"))}
            else if body.chars().count()>2000 || [body,email,input("name")].iter().any(|v|v.contains('\0')) {Some(label("Too long","слишком длинно"))}
            else if !email.is_empty() && !email.split_once('@').is_some_and(|(local,host)|!local.is_empty() && !host.contains('@')
                && host.rsplit_once('.').is_some_and(|(domain,zone)|!domain.is_empty()&&!zone.is_empty()) && !email.contains(char::is_whitespace)) {
                Some(label("Invalid email address","почта странная"))
            }else{None};
        let identity=db.transaction(move |tx| Box::pin(async move {
            Ok(if let Some(uid)=uid {
                tx.query_opt("SELECT id::bigint,name,email FROM users WHERE id=$1::bigint AND COALESCE(deleted_at,0)=0", &[&uid]).await?
                    .map(|row|(row.get::<_,i64>(0),row.get::<_,String>(1),row.get::<_,String>(2)))
            }else{None})
        })).await?.0;
        let mut notice="";
        if error.is_none() {
            let name=if input("name").is_empty(){identity.as_ref().map_or("",|id|id.1.as_str())}else{input("name")};
            let owner=identity.as_ref().map(|id|id.0);
            let (saved_name,saved_mail,saved_body,saved_ip)=(take(name,80),take(email,120),body.to_owned(),take(ip,80));
            let _=db.transaction(move |tx|Box::pin(async move {
                tx.execute("INSERT INTO support_tickets(user_id,name,email,body,ip,created_at) VALUES ($1::bigint,$2,$3,$4,$5,$6::bigint)",
                    &[&owner,&saved_name,&saved_mail,&saved_body,&saved_ip,&now]).await?;
                Ok(())
            })).await?;
            let who=if name.is_empty() {owner.map_or_else(||label("guest","гость").to_owned(),|uid|format!("#{uid}"))}else{name.to_owned()};
            let contact=if email.is_empty() {owner.map_or_else(||label("no contact","без контакта").to_owned(),|uid|format!("{} #{uid}",label("account","аккаунт")))}else{email.to_owned()};
            self.notify(format!("{} {who}",label("WIRING support from","WIRING support от")),format!("{who} · {contact}\n\n{body}")).await;
            notice=if owner.is_some(){label("Sent. The reply will appear in Support in your account. If you provided an email, we will also send it there.",
                "отправили. Ответ появится в разделе «Поддержка» твоего аккаунта. Если указана почта, отправим и туда.")}
                else{label("Sent. We will reply by email if you provided one.","отправили. Ответим на почту, если ты её указал.")};
        }
        Ok(if wants_json {
            if let Some(error)=error {(StatusCode::BAD_REQUEST,Json(json!({"ok":false,"error":error}))).into_response()}
            else{Json(json!({"ok":true,"notice":notice})).into_response()}
        }else{
            let (name,email)=identity.as_ref().map_or(("",""),|id|(id.1.as_str(),id.2.as_str()));
            let escape=crate::public_http::html_escape;
            let form=&self.forms.as_ref().expect("support form configured")[usize::from(lang=="en")][usize::from(error.is_none())];
            let html=form.replace("__WIRING_SUPPORT_ERROR__",&escape(error.unwrap_or_default())).replace("__WIRING_SUPPORT_NOTICE__",&escape(notice))
                .replace("__WIRING_SUPPORT_NAME__",&escape(name)).replace("__WIRING_SUPPORT_EMAIL__",&escape(email));
            ([(axum::http::header::CONTENT_TYPE,"text/html; charset=utf-8"),(axum::http::header::CACHE_CONTROL,"no-store")],html).into_response()
        })
    }

    async fn notify(&self, subject: String, body: String) {
        let email = async {
            if let Some((to,key,from)) = self.email.as_ref() {
                let result = self.http.post("https://api.resend.com/emails").bearer_auth(key)
                    .json(&json!({"from":from,"to":[to],"subject":subject,"text":body})).send().await;
                if !result.is_ok_and(|response| response.status().is_success()) { eprintln!("Support email was not confirmed"); }
            }
        };
        let telegram = async {
            if let Some((token,chat)) = self.telegram.as_ref() {
                let text = format!("WIRING support: {}",take(&body,400));
                let result = self.http.post(format!("https://api.telegram.org/bot{token}/sendMessage")).timeout(Duration::from_secs(5))
                    .json(&json!({"chat_id":chat,"text":text})).send().await;
                if !result.is_ok_and(|response| response.status().is_success()) { eprintln!("Support Telegram notification was not confirmed"); }
            }
        };
        tokio::join!(email,telegram);
    }

    pub(crate) async fn report(&self,db:&Writer,uid:i64,fields:Value,now:i64,lang:&str) -> Result<Response,StatusCode> {
        let status=db.transaction(move |tx| Box::pin(async move {
            let row=tx.query_opt("SELECT COALESCE(report_banned_at,0)!=0 FROM users WHERE id=$1::bigint AND COALESCE(deleted_at,0)=0", &[&uid]).await?;
            let Some(row)=row else {return Ok(Reporter::Missing);};
            if row.get::<_,bool>(0) {return Ok(Reporter::Banned);}
            tx.execute("UPDATE users SET last_seen=$2::bigint WHERE id=$1::bigint", &[&uid,&now]).await?;
            Ok(Reporter::Ready)
        })).await?.0;
        match status {
            Reporter::Missing=>return Ok(crate::account_settings::signed_out(lang)),
            Reporter::Banned=>return Ok(crate::account_settings::report_ban_response(lang)),
            Reporter::Ready=>{},
        }
        let error=|code,en,ru| (code,Json(json!({"ok":false,"error":if lang=="en"{en}else{ru}}))).into_response();
        let Some(other)=integer(&fields["user_id"]) else {return Ok(error(StatusCode::BAD_REQUEST,"Person not found","нет человека"));};
        let reason=fields["reason"].as_str().unwrap_or("").trim();
        if !matches!(reason,"spam"|"fake"|"harassment"|"underage"|"other") {
            return Ok(error(StatusCode::BAD_REQUEST,"Choose a reason","выбери причину"));
        }
        if uid==other {return Ok(error(StatusCode::BAD_REQUEST,"This is you","это ты"));}
        let exists=db.transaction(move |tx|Box::pin(async move {
            Ok(tx.query_opt("SELECT id FROM users WHERE id=$1::bigint", &[&other]).await?.is_some())
        })).await?.0;
        if !exists {return Ok(error(StatusCode::NOT_FOUND,"Person not found","человек не найден"));}
        if !self.reports.allow(&uid.to_string(),Instant::now()) {
            return Ok(error(StatusCode::TOO_MANY_REQUESTS,"Too many reports. Please wait a little.","Слишком много жалоб — подожди немного"));
        }
        let details=take(fields["details"].as_str().unwrap_or("").trim(),500);
        let saved_reason=reason.to_owned();
        let result=db.transaction(move |tx| Box::pin(async move {
            report_pair(tx,uid,other,now,saved_reason,details).await
        })).await?.0;
        let (id,banned)=match result {
            PairOutcome::Reported{id,newly_banned}=>(id,newly_banned),
            _=>return Ok(error(StatusCode::NOT_FOUND,"Person not found","человек не найден")),
        };
        let origin=&self.origin;
        let subject=if lang=="en" {format!("WIRING · new report #{id}")} else {format!("WIRING · новая жалоба #{id}")};
        let body=if lang=="en" {format!("New report #{id}: member #{uid} reported member #{other}. Reason: {reason}. Review: {origin}/admin#reports")}
            else {format!("Новая жалоба #{id}: участник #{uid} на участника #{other}. Причина: {reason}.\nРазобрать: {origin}/admin#reports")};
        self.notify(subject,body).await;
        if banned {
            let body=if lang=="en" {format!("Account #{other} was hidden after reports from two different members. Review it: {origin}/admin#bans")}
                else {format!("Аккаунт #{other} скрыт после второй жалобы от разных участников. Проверь очередь: {origin}/admin#bans")};
            self.notify(format!("WIRING auto-ban · #{other}"),body).await;
        }
        Ok(Json(json!({"ok":true})).into_response())
    }
}
