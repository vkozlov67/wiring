use std::{env,error::Error,time::Duration};

use axum::http::StatusCode;
use serde_json::json;

use crate::chat_write::Writer;

pub(crate) struct Replies { client:reqwest::Client, email:Option<(String,String)>, origin:String }
pub(crate) enum ResultCode { Missing, Invalid(&'static str), Done }
struct Pending { id:i64,email:String,language:String,already_sent:bool }

impl Replies {
    pub(crate) fn new(origin:&str)->Result<Self,Box<dyn Error>> {
        let key=env::var("RESEND_API_KEY").unwrap_or_default().trim().to_owned();
        if key.is_empty()&&env::var("SMTP_HOST").is_ok_and(|value|!value.trim().is_empty()) {return Err("Admin replies require supported mail transport".into());}
        let email=(!key.is_empty()).then(|| (key,env::var("MAIL_FROM").unwrap_or_else(|_|"WIRING <noreply@wiring.date>".into())));
        let client=reqwest::Client::builder().no_proxy().https_only(true).timeout(Duration::from_secs(12)).redirect(reqwest::redirect::Policy::none()).build()?;
        Ok(Self{client,email,origin:origin.to_owned()})
    }

    pub(crate) async fn send(&self,db:&Writer,source:&str,id:i64,body:&str,request:&str,now:i64)->Result<ResultCode,StatusCode> {
        let body=body.trim();
        if !(1..=2000).contains(&body.chars().count())||body.contains('\0'){return Ok(ResultCode::Invalid("Ответ должен содержать от 1 до 2000 символов"));}
        if !(16..=80).contains(&request.len())||!request.bytes().all(|b|b.is_ascii_alphanumeric()||matches!(b,b'-'|b'_')) {
            return Ok(ResultCode::Invalid("Обнови страницу и повтори отправку"));
        }
        let source=match source {"report"=>"report","ticket"=>"ticket",_=>return Ok(ResultCode::Missing)};
        let key=request.to_owned(); let saved=body.to_owned();
        let result=db.transaction(move |tx|Box::pin(async move {
            let query=if source=="report" {"SELECT from_id::bigint,'' FROM reports WHERE id=$1::bigint FOR UPDATE"}
                else {"SELECT user_id::bigint,email FROM support_tickets WHERE id=$1::bigint FOR UPDATE"};
            let Some(row)=tx.query_opt(query,&[&id]).await? else{return Ok(Err(ResultCode::Missing));};
            let uid:Option<i64>=row.get(0);
            let direct:&str=row.get(1);
            let user=if let Some(uid)=uid {tx.query_opt("SELECT id::bigint,email,language FROM users WHERE id=$1::bigint AND COALESCE(deleted_at,0)=0",&[&uid]).await?}else{None};
            let uid:Option<i64>=user.as_ref().map(|user|user.get(0));
            let email=if direct.is_empty(){user.as_ref().map_or("",|user|user.get::<_,&str>(1))}else{direct}.trim();
            let email=if email.contains("@wiring.guest")||email=="demo@wiring.app"||email.ends_with("@wiring.demo"){""}else{email};
            if uid.is_none()&&email.is_empty(){return Ok(Err(ResultCode::Invalid("У обращения нет аккаунта или почты для ответа")));}
            let language=user.as_ref().map_or("ru",|user|user.get::<_,&str>(2)).to_owned();
            if let Some(existing)=tx.query_opt("SELECT id::bigint,source_kind,source_id::bigint,body,email,COALESCE(email_sent_at,0)<>0,
                COALESCE(email_attempted_at,0)>$2::bigint-30 FROM support_replies WHERE request_id=$1 FOR UPDATE",&[&key,&now]).await? {
                if existing.get::<_,&str>(1)!=source||existing.get::<_,i64>(2)!=id||existing.get::<_,&str>(3)!=saved {
                    return Ok(Err(ResultCode::Invalid("Этот запрос уже использован для другого ответа; обнови страницу")));
                }
                let row_id=existing.get::<_,i64>(0);
                let already_sent=existing.get::<_,bool>(5)||existing.get::<_,bool>(6);
                if !already_sent {tx.execute("UPDATE support_replies SET email_attempted_at=$2::bigint WHERE id=$1::bigint",&[&row_id,&now]).await?;}
                return Ok(Ok(Pending{id:row_id,email:existing.get(4),language,already_sent}));
            }
            let attempted=(!email.is_empty()).then_some(now);
            let Some(inserted)=tx.query_opt("INSERT INTO support_replies(source_kind,source_id,user_id,email,body,request_id,created_at,email_attempted_at)
                VALUES($1,$2::bigint,$3::bigint,$4,$5,$6,$7::bigint,$8::bigint) ON CONFLICT(request_id) DO NOTHING RETURNING id::bigint",
                &[&source,&id,&uid,&email,&saved,&key,&now,&attempted]).await? else {
                return Ok(Err(ResultCode::Invalid("Запрос уже обработан; обнови страницу")));
            };
            if let Some(uid)=uid {
                let notice="Поддержка ответила на твоё обращение. Ответ — в разделе «Поддержка».";
                tx.execute("INSERT INTO notifications(user_id,kind,from_id,body,created_at,\"read\") VALUES($1::bigint,'support',0,$2,$3::bigint,0)",
                    &[&uid,&notice,&now]).await?;
            }
            Ok(Ok(Pending{id:inserted.get(0),email:email.to_owned(),language,already_sent:false}))
        })).await?.0;
        let pending=match result {Ok(value)=>value,Err(code)=>return Ok(code)};
        if !pending.already_sent&&!pending.email.is_empty() && let Some((key,sender))=self.email.as_ref() {
            let en=pending.language=="en";
            let subject=if en{"WIRING support reply"}else{"Ответ поддержки WIRING"};
            let text=format!("{body}\n\n{}: {}/support",if en{"Support"}else{"Поддержка"},self.origin);
            let response=self.client.post("https://api.resend.com/emails").bearer_auth(key).json(&json!({"from":sender,"to":[pending.email],"subject":subject,"text":text})).send().await;
            if response.is_ok_and(|response|response.status().is_success()) {
                let _=db.transaction(move |tx|Box::pin(async move {
                    tx.execute("UPDATE support_replies SET email_sent_at=$2::bigint WHERE id=$1::bigint",&[&pending.id,&now]).await?;
                    Ok(())
                })).await?;
            }
        }
        Ok(ResultCode::Done)
    }
}
