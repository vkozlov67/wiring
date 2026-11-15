use std::time::{Duration,Instant};

use axum::{Json, extract::Multipart,http::StatusCode,response::{IntoResponse,Response}};
use serde_json::json;

use crate::{chat_write::{self,SendMessage,Writer},login::LoginLimit,media_http::Media,message_send::{Delivery,demo,take}};

pub(crate) struct VoiceUpload { limit:LoginLimit }
enum ResultCode { SignedOut, Banned, Guest, Missing, Blocked, NoMatch, Reply, Sent(i64,Option<String>) }

impl VoiceUpload {
    pub(crate) fn new()->Self { Self{limit:LoginLimit::configured(Duration::from_secs(3600),40)} }

    pub(crate) async fn send(&self,db:&Writer,media:&Media,delivery:&Delivery,uid:i64,ip:&str,now:i64,lang:&str,form:Option<Multipart>)
        -> Result<Response,StatusCode>
    {
        let error=|status,en,ru|(status,Json(json!({"ok":false,"error":if lang=="en"{en}else{ru}}))).into_response();
        let bad=StatusCode::BAD_REQUEST;
        let account=db.transaction(move |tx|Box::pin(async move {
            let row=tx.query_opt("SELECT name,email,COALESCE(report_banned_at,0)!=0 FROM users WHERE id=$1::bigint AND COALESCE(deleted_at,0)=0", &[&uid]).await?;
            let Some(row)=row else {return Ok(Err(ResultCode::SignedOut));};
            if row.get::<_,bool>(2) {return Ok(Err(ResultCode::Banned));}
            tx.execute("UPDATE users SET last_seen=$2::bigint WHERE id=$1::bigint", &[&uid,&now]).await?;
            let email:&str=row.get(1);
            if email.ends_with("@wiring.guest")||email=="demo@wiring.app" {return Ok(Err(ResultCode::Guest));}
            Ok(Ok(row.get::<_,String>(0)))
        })).await?.0;
        let name=match account {
            Ok(name)=>name,
            Err(ResultCode::SignedOut)=>return Ok(crate::account_settings::signed_out(lang)),
            Err(ResultCode::Banned)=>return Ok(crate::account_settings::report_ban_response(lang)),
            Err(_)=>return Ok(error(StatusCode::FORBIDDEN,"Complete your profile first","сначала собери свой профиль")),
        };
        if !self.limit.allow(ip,Instant::now()) {return Ok(error(StatusCode::TOO_MANY_REQUESTS,"Too many voice messages. Please wait.","слишком много голосовых, подожди"));}
        let mut to=None;
        let mut reply=None;
        let mut duration=None;
        let mut file=None;
        if let Some(mut form)=form {
            loop {
                let field=match form.next_field().await {Ok(Some(field))=>field,Ok(None)=>break,Err(_)=>return Ok(crate::mobile_http::too_large())};
                match field.name() {
                    Some("to_id"|"reply_to_id"|"duration") if field.file_name().is_none()=>{
                        let key=field.name().unwrap_or_default().to_owned();
                        let value=field.text().await.map_err(|_|bad)?;
                        let out=match key.as_str(){"to_id"=>&mut to,"reply_to_id"=>&mut reply,_=>&mut duration};
                        if out.is_none(){*out=Some(value);}
                    }
                    Some("file") if field.file_name().is_some() && file.is_none()=>{
                        let mime=field.content_type().unwrap_or("").to_owned();
                        let bytes=field.bytes().await.map_err(|_|StatusCode::PAYLOAD_TOO_LARGE)?;
                        file=Some((mime,bytes));
                    }
                    _=>{},
                }
            }
        }
        let Some(other)=to.as_deref().and_then(|value|value.trim().parse::<i64>().ok()).filter(|id|*id!=0) else {return Ok(error(bad,"No recipient","нет адресата"));};
        let reply=match reply.as_deref().unwrap_or("") {
            ""|"0"=>None,
            value=>match value.trim().parse().ok(){Some(id)=>Some(id),None=>return Ok(error(bad,"Invalid response","некорректный ответ"))},
        };
        let check=db.transaction(move |tx|Box::pin(async move {
            let pair:[&(dyn tokio_postgres::types::ToSql+Sync);2]=[&uid,&other];
            let (blocked,matched)=chat_write::pair_access(tx,&pair).await?;
            Ok(if blocked{Some(ResultCode::Blocked)}else if !matched{Some(ResultCode::NoMatch)}else{None})
        })).await?.0;
        let unavailable=|state|match state {
            ResultCode::NoMatch=>error(StatusCode::FORBIDDEN,"You can message after a mutual like","написать можно после взаимного лайка"),
            ResultCode::Blocked=>error(StatusCode::FORBIDDEN,"This person is hidden","этот человек скрыт"),
            ResultCode::Reply=>error(bad,"The message you are replying to was not found","сообщение для ответа не найдено"),
            _=>error(StatusCode::NOT_FOUND,"Person not found","человек не найден"),
        };
        if let Some(state)=check {return Ok(unavailable(state));}
        let Some((mime,bytes))=file else {return Ok(error(bad,"Recording not found","нет записи"));};
        let Some(extension)=audio_kind(&bytes,&mime) else {return Ok(error(bad,"This is not a voice message","это не голосовое"));};
        let duration=duration.as_deref().and_then(|v|v.trim().parse::<i64>().ok()).unwrap_or(1).clamp(1,90);
        let path=media.save_chat_voice(uid,extension,&bytes)?;
        store_attachment(db,media,delivery,uid,other,reply,now,lang,name,String::new(),
            vec![path.clone()],chat_write::Media::Voice{path,duration}).await
    }
}

pub(crate) async fn store_attachment(db:&Writer,media:&Media,delivery:&Delivery,uid:i64,other:i64,reply:Option<i64>,now:i64,lang:&str,
    name:String,body:String,paths:Vec<String>,attachment:chat_write::Media)->Result<Response,StatusCode>
{
        let text=|status,en,ru|(status,Json(json!({"ok":false,"error":if lang=="en"{en}else{ru}}))).into_response();
        let outcome=db.transaction(move |tx|Box::pin(async move {
            let pair:[&(dyn tokio_postgres::types::ToSql+Sync);2]=[&uid,&other];
            chat_write::lock_pair(tx,&pair).await?;
            let row=tx.query_opt("SELECT email,language,COALESCE(is_seed,0)!=0,COALESCE(notify_enabled,0)!=0,COALESCE(notify_push,0)!=0 FROM users
                WHERE id=$1::bigint AND COALESCE(deleted_at,0)=0", &[&other]).await?;
            let Some(row)=row else {return Ok(ResultCode::Missing);};
            let notify=!row.get::<_,bool>(2) && row.get::<_,bool>(3) && !demo(row.get(0));
            let push=row.get::<_,bool>(4);
            let english=row.get::<_,&str>(1)=="en";
            let preview=if matches!(attachment,chat_write::Media::Voice{..}) {if english{"voice message"}else{"голосовое"}}
                else if body.is_empty(){if english{"photo"}else{"фото"}}else{body.as_str()};
            let snippet=preview.split_whitespace().collect::<Vec<_>>().join(" ");
            let snippet=if snippet.chars().count()>80 {format!("{}…",take(&snippet,79))}else{snippet};
            let notice=format!("{name}: {snippet}");
            let sent=chat_write::write_message(tx,&SendMessage{uid,other_id:other,body,reply_to_id:reply,client_id:None,created_at:now,
                notice_body:notify.then(||take(&notice,280)),media:Some(attachment)}).await?;
            Ok(match sent {
                chat_write::Outcome::Sent{id,..}=>ResultCode::Sent(id,(notify&&push).then_some(notice)),
                chat_write::Outcome::Duplicate{id}=>ResultCode::Sent(id,None),
                chat_write::Outcome::Blocked=>ResultCode::Blocked,
                chat_write::Outcome::NoMatch=>ResultCode::NoMatch,
                chat_write::Outcome::ReplyMissing=>ResultCode::Reply,
            })
        })).await;
        Ok(match outcome {
            Ok(Json(ResultCode::Sent(id,push)))=>{
                if let Some(notice)=push {delivery.push(db,other,uid,"message",&notice,now).await;}
                Json(json!({"ok":true,"id":id})).into_response()
            }
            Ok(Json(state))=>{
                media.erase_chat_uploads(paths);
                match state {
                    ResultCode::NoMatch=>text(StatusCode::FORBIDDEN,"You can message after a mutual like","написать можно после взаимного лайка"),
                    ResultCode::Blocked=>text(StatusCode::FORBIDDEN,"This person is hidden","этот человек скрыт"),
                    ResultCode::Reply=>text(StatusCode::BAD_REQUEST,"The message you are replying to was not found","сообщение для ответа не найдено"),
                    _=>text(StatusCode::NOT_FOUND,"Person not found","человек не найден"),
                }
            }
            // A lost commit response does not make it safe to remove a referenced file.
            Err(code)=>return Err(code),
        })
}

fn audio_kind(data:&[u8],mime:&str)->Option<&'static str> {
    if !(16..=8*1024*1024).contains(&data.len()){return None;}
    if data.starts_with(b"\x1a\x45\xdf\xa3"){return Some("webm");}
    if data.starts_with(b"RIFF") && &data[8..12]==b"WAVE"{return Some("wav");}
    if data.starts_with(b"OggS"){return Some("ogg");}
    if data.starts_with(b"ID3") || data[0]==0xff && data[1]&0xe0==0xe0{return Some("mp3");}
    if &data[4..8]==b"ftyp"{return Some("m4a");}
    match mime.split(';').next()?.trim().to_ascii_lowercase().as_str(){
        "audio/webm"|"video/webm"=>Some("webm"),"audio/mp4"|"audio/x-m4a"=>Some("m4a"),"audio/mpeg"=>Some("mp3"),
        "audio/ogg"=>Some("ogg"),"audio/wav"|"audio/x-wav"=>Some("wav"),"audio/aac"=>Some("aac"),_=>None}
}
