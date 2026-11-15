use std::{env,error::Error,path::{Path,PathBuf},process::{Command,Stdio},sync::Arc,time::{Duration,Instant}};

use axum::{Json,extract::Multipart,http::StatusCode,response::{IntoResponse,Response}};
use base64::{Engine,engine::general_purpose::STANDARD};
use serde_json::{Value,json};

use crate::{chat_write::{self,Writer},login::LoginLimit,media_http::Media,message_send::{Delivery,take},
    profile_read::{ProfileRules,profile_albums,profile_photos}};

const MAX:usize=8*1024*1024;
pub(crate) struct PhotoUpload { http:reqwest::Client,key:String,profile_limit:LoginLimit,chat_limit:LoginLimit,slots:Arc<tokio::sync::Semaphore> }
enum Code { Missing,Banned,Guest,NoMatch,Blocked,Full,AlbumMissing,Ready(String),Done(Value) }
enum ImageError { Type,Small,TooLarge,Invalid }

impl PhotoUpload {
    pub(crate) fn new()->Result<Self,Box<dyn Error>> {
        Ok(Self {http:reqwest::Client::builder().https_only(true).no_proxy().timeout(Duration::from_secs(8))
            .redirect(reqwest::redirect::Policy::none()).build()?,key:env::var("OPENAI_API_KEY").unwrap_or_default().trim().to_owned(),
            profile_limit:LoginLimit::configured(Duration::from_secs(3600),40),chat_limit:LoginLimit::configured(Duration::from_secs(3600),40),
            slots:Arc::new(tokio::sync::Semaphore::new(2))})
    }

    pub(crate) async fn serve(&self,db:&Writer,media:&Media,profile:Arc<ProfileRules>,delivery:&Delivery,uid:i64,chat:bool,
        ip:&str,now:i64,lang:&'static str,form:Option<Multipart>) -> Result<Response,StatusCode>
    {
        let error=|status,en,ru|(status,Json(json!({"ok":false,"error":if lang=="en"{en}else{ru}}))).into_response();
        let bad=StatusCode::BAD_REQUEST;
        let respond=|code| match code {
            Code::Missing=>crate::account_settings::signed_out(lang),Code::Banned=>crate::account_settings::report_ban_response(lang),
            Code::Guest=>error(StatusCode::FORBIDDEN,"Complete your profile first","сначала собери свой профиль"),
            Code::Blocked=>error(StatusCode::FORBIDDEN,"This person is hidden","этот человек скрыт"),
            Code::NoMatch=>error(StatusCode::FORBIDDEN,"You can message after a mutual like","написать можно после взаимного лайка"),
            Code::Full=>error(bad,"No more than 12 photos","не больше 12 фото"),
            Code::AlbumMissing=>error(StatusCode::NOT_FOUND,"Album not found","альбом не найден"),
            Code::Done(value)=>Json(value).into_response(),Code::Ready(_)=>unreachable!(),
        };
        let account=db.transaction(move |tx|Box::pin(async move {
            let row=tx.query_opt("SELECT name,email,COALESCE(report_banned_at,0)!=0 FROM users WHERE id=$1::bigint AND COALESCE(deleted_at,0)=0",&[&uid]).await?;
            let Some(row)=row else {return Ok(Code::Missing);};
            if row.get::<_,bool>(2){return Ok(Code::Banned);}
            tx.execute("UPDATE users SET last_seen=$2::bigint WHERE id=$1::bigint",&[&uid,&now]).await?;
            let email:&str=row.get(1);
            if email.ends_with("@wiring.guest")||email=="demo@wiring.app" {return Ok(Code::Guest);}
            Ok(Code::Ready(row.get(0)))
        })).await?.0;
        let Code::Ready(name)=account else {return Ok(respond(account));};
        let limit=if chat{&self.chat_limit}else{&self.profile_limit};
        if !limit.allow(ip,Instant::now()) {return Ok(error(StatusCode::TOO_MANY_REQUESTS,
            if chat{"Too many photos. Please wait."}else{"Too many uploads. Please wait."},
            if chat{"слишком много фото, подожди"}else{"слишком много загрузок, подожди"}));}
        let mut fields=std::collections::HashMap::new();
        let mut images=Vec::new();
        let mut single=Vec::new();
        if let Some(mut form)=form {
            loop {
                let field=match form.next_field().await {Ok(Some(field))=>field,Ok(None)=>break,Err(_)=>return Ok(crate::mobile_http::too_large())};
                let key=field.name().unwrap_or_default().to_owned();
                if let Some(filename)=field.file_name().map(str::to_owned) {
                    let list=match key.as_str(){"files" if chat=>&mut images,"file"=>&mut single,_=>continue};
                    if list.len()>=11 {return Ok(error(bad,"Choose between 1 and 10 photos","выбери от 1 до 10 фото"));}
                    list.push((filename,field.bytes().await.map_err(|_|StatusCode::PAYLOAD_TOO_LARGE)?));
                } else if matches!(key.as_str(),"to_id"|"reply_to_id"|"body"|"album"|"album_id") {
                    fields.entry(key).or_insert(field.text().await.map_err(|_|bad)?);
                }
            }
        }
        let field=|key:&str|fields.get(key).map_or("",String::as_str);
        let to=if chat {
            let Some(other)=field("to_id").trim().parse::<i64>().ok().filter(|id|*id!=0) else {return Ok(error(bad,"No recipient","нет адресата"));};
            let check=db.transaction(move |tx|Box::pin(async move {
                let pair:[&(dyn tokio_postgres::types::ToSql+Sync);2]=[&uid,&other];
                let (blocked,matched)=chat_write::pair_access(tx,&pair).await?;
                Ok(if blocked{Some(Code::Blocked)}else if !matched{Some(Code::NoMatch)}else{None})
            })).await?.0;
            if let Some(code)=check{return Ok(respond(code));}
            Some(other)
        } else {None};
        let reply=if chat {match field("reply_to_id") {""|"0"=>None,s=>match s.trim().parse().ok(){Some(id)=>Some(id),None=>return Ok(error(bad,"Invalid response","некорректный ответ"))}}}else{None};
        let album=if chat {None}else{match field("album_id"){""=>None,s=>match s.trim().parse::<i64>().ok(){Some(id)=>Some(id),None=>return Ok(error(bad,"Invalid album","не тот альбом"))}}};
        let title=take(&field("album").replace('\0',""),32).trim().to_owned();
        let title=if title.is_empty(){"я".to_owned()}else{title};
        if images.is_empty(){images=single;}
        if !chat{images.truncate(1);}
        if images.is_empty()||images.len()>10 {
            return Ok(if chat{error(bad,"Choose between 1 and 10 photos","выбери от 1 до 10 фото")}else{error(bad,"Choose a file","выбери файл")});
        }
        let slot=Arc::clone(&self.slots).try_acquire_owned().map_err(|_|StatusCode::SERVICE_UNAVAILABLE)?;
        let processing=tokio::task::spawn_blocking(move || process(images,!chat,slot)).await.map_err(|_|StatusCode::SERVICE_UNAVAILABLE)?;
        let (mut photos,cover)=match processing {
            Ok(images)=>images,
            Err(ImageError::Type)=>return Ok(error(bad,"Choose a JPG, PNG or WebP image","нужен jpg, png или webp")),
            Err(ImageError::Small)=>return Ok(error(bad,"Photo is too small","фото слишком маленькое")),
            Err(ImageError::TooLarge)=>return Ok(error(bad,"File is larger than 8 MB. Resize it or choose another photo.","файл больше 8 МБ — сожми или выбери другое фото")),
            Err(ImageError::Invalid)=>return Ok(error(bad,"This does not look like a photo","это не похоже на фото")),
        };
        for (_,preview) in &photos {if self.blocked(preview).await {return Ok(error(bad,"This photo cannot be uploaded","это фото нельзя загрузить"));}}
        if let Some(cover)=cover {photos.insert(0,(cover,Vec::new()));}
        let mut paths=Vec::new();
        for (photo,_) in photos {
            match media.save_photo(uid,chat,&photo) {
                Ok(path)=>paths.push(path),
                Err(code)=>{media.erase_chat_uploads(paths);return Err(code);}
            }
        }
        if let Some(other)=to {
            return crate::voice_upload::store_attachment(db,media,delivery,uid,other,reply,now,lang,name,take(field("body").trim(),500).replace('\0',""),
                paths.clone(),chat_write::Media::Photo{paths}).await;
        }
        let path=paths.pop().expect("one profile photo");
        let stored=path.clone();
        let result=db.transaction(move |tx|Box::pin(async move {
            let row=tx.query_opt("SELECT COALESCE(report_banned_at,0)!=0 FROM users WHERE id=$1::bigint AND COALESCE(deleted_at,0)=0 FOR UPDATE",&[&uid]).await?;
            let Some(row)=row else {return Ok(Code::Missing);}; if row.get::<_,bool>(0){return Ok(Code::Banned);}
            let count=tx.query_one("SELECT count(*) FROM photos WHERE user_id=$1::bigint",&[&uid]).await?.get::<_,i64>(0);
            if count>=12{return Ok(Code::Full);}
            let album=if let Some(album)=album {
                if tx.query_opt("SELECT id FROM albums WHERE id=$1::bigint AND user_id=$2::bigint",&[&album,&uid]).await?.is_none(){return Ok(Code::AlbumMissing);}
                album
            }else if let Some(row)=tx.query_opt("SELECT id::bigint FROM albums WHERE user_id=$1::bigint AND title=$2 ORDER BY id LIMIT 1",&[&uid,&title]).await?{row.get(0)}
            else{tx.query_one("INSERT INTO albums(user_id,title,sort_order,created_at) SELECT $1::bigint,$2,count(*)::integer,$3::bigint FROM albums WHERE user_id=$1::bigint RETURNING id::bigint",&[&uid,&title,&now]).await?.get(0)};
            let id=tx.query_one("INSERT INTO photos(user_id,album_id,path,is_primary,sort_order,created_at) VALUES($1::bigint,$2::bigint,$3,$4,$5,$6::bigint) RETURNING id::bigint",&[&uid,&album,&stored,&i32::from(count==0),&(count as i32),&now]).await?.get::<_,i64>(0);
            tx.execute("UPDATE users SET photo=(SELECT path FROM photos WHERE user_id=$1::bigint ORDER BY is_primary DESC,sort_order,id LIMIT 1) WHERE id=$1::bigint",&[&uid]).await?;
            let user=profile.read(tx,uid,now,lang).await?.0;
            tx.execute("UPDATE users SET onboard_done=$2 WHERE id=$1::bigint",&[&uid,&i32::from(user["needs_profile"]==false)]).await?;
            let photos=profile_photos(tx,uid).await?;
            let item=photos.iter().find(|photo|photo["id"]==id);
            Ok(Code::Done(json!({"ok":true,"photo":item,"photos":photos,"albums":profile_albums(tx,uid,&photos).await?})))
        })).await?.0;
        if !matches!(result,Code::Done(_)){media.erase_profile_upload(uid,&path);}
        Ok(respond(result))
    }

    async fn blocked(&self,preview:&[u8])->bool {
        if self.key.is_empty(){return false;}
        let input=format!("data:image/jpeg;base64,{}",STANDARD.encode(preview));
        let request=self.http.post("https://api.openai.com/v1/moderations").bearer_auth(&self.key)
            .json(&json!({"model":"omni-moderation-latest","input":[{"type":"image_url","image_url":{"url":input}}]}));
        let Ok(mut response)=request.send().await else{return false;};
        if !response.status().is_success(){return false;}
        let mut body=Vec::new();
        while let Ok(Some(chunk))=response.chunk().await {if body.len()+chunk.len()>1024*1024{return false;}body.extend_from_slice(&chunk);}
        let Ok(fields)=serde_json::from_slice::<Value>(&body) else{return false;};
        let categories=&fields["results"][0]["categories"];
        categories["sexual"]==true || categories["sexual/minors"]==true
    }
}

struct Temp(PathBuf);
impl Drop for Temp{fn drop(&mut self){let _=std::fs::remove_dir_all(&self.0);}}
type Processed=Result<(Vec<(Vec<u8>,Vec<u8>)>,Option<Vec<u8>>),ImageError>;

fn process(images:Vec<(String,axum::body::Bytes)>,portrait:bool,_slot:tokio::sync::OwnedSemaphorePermit)->Processed {
    let mut nonce=[0;16];getrandom::fill(&mut nonce).map_err(|_|ImageError::Type)?;
    let temp=Temp(env::temp_dir().join(format!("wiring-photo-{}",nonce.iter().map(|b|format!("{b:02x}")).collect::<String>())));
    std::fs::create_dir(&temp.0).map_err(|_|ImageError::Type)?;
    let mut output=Vec::new();
    let mut paths=Vec::new();
    for (index,(name,data)) in images.into_iter().enumerate() {
        if data.len()>MAX{return Err(ImageError::TooLarge);}
        if data.len()<32{return Err(ImageError::Invalid);}
        if let Some((_,ext))=name.rsplit_once('.') && !matches!(ext.to_ascii_lowercase().as_str(),"jpg"|"jpeg"|"png"|"gif"|"webp") {return Err(ImageError::Type);}
        let format=if data.starts_with(b"\xff\xd8\xff"){"jpeg_pipe"}else if data.starts_with(b"\x89PNG\r\n\x1a\n"){"png_pipe"}
            else if data.starts_with(b"GIF87a")||data.starts_with(b"GIF89a"){"gif"}
            else if data.starts_with(b"RIFF")&&&data[8..12]==b"WEBP"{"webp_pipe"}else{return Err(ImageError::Type);};
        let raw=temp.0.join(format!("input-{index}"));
        std::fs::write(&raw,&data).map_err(|_|ImageError::Type)?;
        let dimensions=Command::new("/usr/bin/timeout").args(["8","/usr/bin/ffprobe","-v","error","-f",format,"-select_streams","v:0",
            "-show_entries","stream=width,height","-of","csv=s=x:p=0"]).arg(&raw).output().map_err(|_|ImageError::Type)?;
        if !dimensions.status.success(){return Err(ImageError::Type);}
        let dimensions=String::from_utf8_lossy(&dimensions.stdout);
        let (width,height)=dimensions.trim().split_once('x').and_then(|(w,h)|Some((w.parse::<u64>().ok()?,h.parse::<u64>().ok()?))).ok_or(ImageError::Type)?;
        if width.min(height)<160{return Err(ImageError::Small);}
        if width.checked_mul(height).is_none_or(|pixels|pixels>40_000_000){return Err(ImageError::Type);}
        let jpg=temp.0.join(format!("image-{index}.jpg"));
        let preview=temp.0.join(format!("preview-{index}.jpg"));
        let scale="scale='min(1600,iw)':'min(1600,ih)':force_original_aspect_ratio=decrease";
        let crop=format!("crop='min(iw,round(ih*3/4))':'min(ih,round(iw*4/3))':'(iw-ow)/2':'(ih-oh)*0.12':exact=1,{scale}");
        convert(&raw,Some(format),if portrait{&crop}else{scale},"3",&jpg)?;
        convert(&jpg,Some("jpeg_pipe"),"scale='min(512,iw)':'min(512,ih)':force_original_aspect_ratio=decrease","5",&preview)?;
        output.push((std::fs::read(&jpg).map_err(|_|ImageError::Type)?,std::fs::read(preview).map_err(|_|ImageError::Type)?));
        paths.push(jpg);
    }
    let cover=if paths.len()>1 {
        let sheet=temp.0.join("sheet.jpg");
        let mut command=ffmpeg();
        for path in &paths {command.arg("-i").arg(path);}
        let mut filter=(0..paths.len()).map(|i|format!("[{i}:v]scale=592:592:force_original_aspect_ratio=decrease,pad=600:600:(ow-iw)/2:(oh-ih)/2:color=0x171719,setsar=1[v{i}];")).collect::<String>();
        for i in 0..paths.len(){filter.push_str(&format!("[v{i}]"));}
        let layout=(0..paths.len()).map(|i|format!("{}_{}",i%2*600,i/2*600)).collect::<Vec<_>>().join("|");
        filter.push_str(&format!("xstack=inputs={}:layout={layout}:fill=0x171719,pad=1200:{}:0:0:color=0x171719",paths.len(),paths.len().div_ceil(2)*600));
        command.args(["-filter_complex",&filter]);
        finish(command,"3",&sheet)?;
        Some(std::fs::read(sheet).map_err(|_|ImageError::Type)?)
    }else{None};
    Ok((output,cover))
}

fn ffmpeg()->Command {
    let mut cmd=Command::new("/usr/bin/timeout");
    cmd.args(["10","/usr/bin/ffmpeg","-nostdin","-v","error","-y","-threads","1","-filter_threads","1","-filter_complex_threads","1"]);
    cmd
}

fn convert(input:&Path,format:Option<&str>,filter:&str,quality:&str,output:&Path)->Result<(),ImageError> {
    let mut cmd=ffmpeg(); if let Some(format)=format{cmd.args(["-f",format]);}
    cmd.arg("-i").arg(input).args(["-vf",filter]);
    finish(cmd,quality,output)
}

fn finish(mut cmd:Command,quality:&str,output:&Path)->Result<(),ImageError> {
    if cmd.args(["-map_metadata","-1","-frames:v","1","-threads","1","-q:v",quality]).arg(output)
        .stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|status|status.success()) {Ok(())}else{Err(ImageError::Type)}
}
