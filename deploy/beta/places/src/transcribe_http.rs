use std::{env,error::Error,time::{Duration,Instant}};

use axum::{Json,http::StatusCode,response::{IntoResponse,Response}};
use serde_json::{Value,json};

use crate::{chat_write::{MessageAction,MutateMessage,Mutation,Writer,mutate_message,pair_access},
    login::LoginLimit,media_http::Media,message_send::take};

const MAX_AUDIO:usize=8*1024*1024;

pub(crate) struct Transcription {
    http:reqwest::Client,
    key:String,
    model:String,
    user_limit:LoginLimit,
    ip_limit:LoginLimit,
    slots:tokio::sync::Semaphore,
}

enum Voice { NoAccount,Banned,Missing,Present(String,String) }

impl Transcription {
    pub(crate) fn new()->Result<Self,Box<dyn Error>> {
        Ok(Self {http:reqwest::Client::builder().https_only(true).no_proxy().redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(45)).build()?,
            key:env::var("OPENAI_API_KEY").unwrap_or_default().trim().to_owned(),
            model:env::var("OPENAI_TRANSCRIBE_MODEL").unwrap_or_else(|_| "whisper-1".into()).trim().to_owned(),
            user_limit:LoginLimit::configured(Duration::from_secs(3600),30),
            ip_limit:LoginLimit::configured(Duration::from_secs(3600),60),slots:tokio::sync::Semaphore::new(4)})
    }

    pub(crate) async fn serve(&self,db:&Writer,media:&Media,uid:i64,id:Option<i64>,ip:&str,now:i64,lang:&str)->Result<Response,StatusCode> {
        let error=|status,en,ru|(status,Json(json!({"ok":false,"error":if lang=="en"{en}else{ru}}))).into_response();
        let missing=||error(StatusCode::NOT_FOUND,"Voice message not found","голосовое не найдено");
        let account=db.transaction(move |tx| Box::pin(async move {
            let row=tx.query_opt("SELECT COALESCE(report_banned_at,0)!=0 FROM users WHERE id=$1::bigint AND COALESCE(deleted_at,0)=0",&[&uid]).await?;
            let Some(row)=row else {return Ok(Voice::NoAccount);};
            if row.get::<_,bool>(0) {return Ok(Voice::Banned);}
            tx.execute("UPDATE users SET last_seen=$2::bigint WHERE id=$1::bigint",&[&uid,&now]).await?;
            Ok(Voice::Missing)
        })).await?.0;
        match account {
            Voice::NoAccount=>return Ok(crate::account_settings::signed_out(lang)),
            Voice::Banned=>return Ok(crate::account_settings::report_ban_response(lang)),
            _=>(),
        }
        let instant=Instant::now();
        if !self.user_limit.allow(&uid.to_string(),instant) || !self.ip_limit.allow(ip,instant) {
            return Ok(error(StatusCode::TOO_MANY_REQUESTS,"Too many transcription requests. Please wait.","слишком много расшифровок, подожди"));
        }
        let Some(id)=id else {return Ok(missing());};
        let voice=db.transaction(move |tx| Box::pin(async move {
            let row=tx.query_opt("SELECT from_id::bigint,to_id::bigint,COALESCE(audio,''),COALESCE(transcript,'')
                FROM messages WHERE id=$1::bigint AND COALESCE(deleted_at,0)=0", &[&id]).await?;
            let Some(row)=row else {return Ok(Voice::Missing);};
            let (a,b):(i64,i64)=(row.get(0),row.get(1));
            let audio:String=row.get(2);
            if (uid!=a && uid!=b) || audio.trim().is_empty() {return Ok(Voice::Missing);}
            let pair:[&(dyn tokio_postgres::types::ToSql+Sync);2]=[&a,&b];
            let (blocked,matched)=pair_access(tx,&pair).await?;
            if blocked || !matched { return Ok(Voice::Missing); }
            Ok(Voice::Present(audio,row.get(3)))
        })).await?.0;
        let Voice::Present(relative,existing)=voice else {return Ok(missing());};
        if !existing.trim().is_empty() {return Ok(Json(json!({"ok":true,"transcript":existing.trim()})).into_response());}
        let Some(file)=media.chat_upload(&relative) else {return Ok(error(StatusCode::NOT_FOUND,"Recording not found","запись не найдена"));};
        if self.key.is_empty() {return Ok(error(StatusCode::SERVICE_UNAVAILABLE,"Transcription is not configured","расшифровка не настроена"));}
        let _slot=self.slots.try_acquire().map_err(|_|StatusCode::SERVICE_UNAVAILABLE)?;
        let failed=||error(StatusCode::SERVICE_UNAVAILABLE,"Could not transcribe","не удалось расшифровать");
        let data=tokio::task::spawn_blocking(move || {
            use std::io::Read;
            let mut data=Vec::new();
            std::fs::File::open(file)?.take((MAX_AUDIO+1) as u64).read_to_end(&mut data)?;
            Ok::<_,std::io::Error>(data)
        }).await.ok().and_then(Result::ok);
        let Some(data)=data.filter(|data|data.len()<=MAX_AUDIO) else {return Ok(failed());};
        if data.is_empty() {return Ok(error(StatusCode::SERVICE_UNAVAILABLE,"The voice recording is empty","пустое голосовое"));}
        let Some(transcript)=self.transcribe(&relative,&data).await else {return Ok(failed());};
        if transcript.is_empty() {return Ok(error(StatusCode::SERVICE_UNAVAILABLE,"No words could be heard in the recording","в записи не слышно слов"));}
        let saved=db.transaction(move |tx|Box::pin(async move {
            mutate_message(tx,&MutateMessage{uid,message_id:id,changed_at:now,action:MessageAction::Transcribe{transcript}}).await
        })).await?.0;
        Ok(if let Mutation::Transcribed{transcript}=saved {Json(json!({"ok":true,"transcript":transcript})).into_response()}else{missing()})
    }

    async fn transcribe(&self,filename:&str,data:&[u8])->Option<String> {
        let filename=filename.rsplit('/').next()?;
        let mime=match filename.rsplit('.').next()? {"wav"=>"audio/wav","m4a"|"mp4"=>"audio/mp4","ogg"=>"audio/ogg","mp3"=>"audio/mpeg",_=>"audio/webm"};
        let mut random=[0;12]; getrandom::fill(&mut random).ok()?;
        let boundary=format!("----wiring{}",random.iter().map(|b|format!("{b:02x}")).collect::<String>());
        let mut body=format!("--{boundary}\r\nContent-Disposition: form-data; name=\"model\"\r\n\r\n{}\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"response_format\"\r\n\r\njson\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{filename}\"\r\nContent-Type: {mime}\r\n\r\n",self.model).into_bytes();
        body.extend_from_slice(data); body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
        let mut response=self.http.post("https://api.openai.com/v1/audio/transcriptions").bearer_auth(&self.key)
            .header("Content-Type",format!("multipart/form-data; boundary={boundary}")).body(body).send().await.ok()?;
        if !response.status().is_success() {return None;}
        let mut bytes=Vec::new();
        while let Some(chunk)=response.chunk().await.ok()? {
            if bytes.len()+chunk.len()>1024*1024 {return None;}
            bytes.extend_from_slice(&chunk);
        }
        let fields:Value=serde_json::from_slice(&bytes).ok()?;
        Some(take(fields["text"].as_str().unwrap_or("").trim(),2000))
    }
}
