use std::{collections::HashMap,env,error::Error,path::{Component,Path,PathBuf},sync::Arc,time::Duration};

use axum::{http::{HeaderValue,StatusCode,header},response::{IntoResponse,Response}};

use crate::chat_write::Writer;

pub(crate) struct Media {
    uploads:PathBuf,
    thumbs:PathBuf,
    public:PathBuf,
    slots:Arc<tokio::sync::Semaphore>,
}
enum Access { SignedOut,Banned,Forbidden,Missing,Allowed{path:String,audio:bool} }

impl Media {
    pub(crate) fn new()->Result<Self,Box<dyn Error>> {
        let uploads=PathBuf::from(env::var("UPLOAD_DIR")?).canonicalize()?;
        let thumbs=PathBuf::from(env::var("THUMB_DIR")?).canonicalize()?;
        let public=PathBuf::from(env::var("WIRING_PUBLIC_STATIC_DIR")?).canonicalize()?;
        if !uploads.is_dir()||!thumbs.is_dir()||!public.is_dir(){return Err("Media directories are required".into());}
        Ok(Self{uploads,thumbs,public,slots:Arc::new(tokio::sync::Semaphore::new(2))})
    }

    pub(crate) async fn chat(&self,db:&Writer,uid:i64,id:&str,query:&str,now:i64,lang:&str)->Result<Response,StatusCode> {
        if id.is_empty() || !id.bytes().all(|b|b.is_ascii_digit()) {return Ok(missing());}
        let id=id.parse::<i64>().ok();
        let params=params(query);
        let index=params.get("index").and_then(|value|value.parse::<i64>().ok()).unwrap_or_default();
        let access=db.transaction(move |tx|Box::pin(async move {
            let viewer=tx.query_opt("SELECT COALESCE(report_banned_at,0)!=0 FROM users WHERE id=$1::bigint AND COALESCE(deleted_at,0)=0", &[&uid]).await?;
            let Some(viewer)=viewer else {return Ok(Access::SignedOut);};
            if viewer.get::<_,bool>(0) {return Ok(Access::Banned);}
            tx.execute("UPDATE users SET last_seen=$2::bigint WHERE id=$1::bigint", &[&uid,&now]).await?;
            let Some(id)=id else {return Ok(Access::Missing);};
            let row=tx.query_opt("SELECT from_id::bigint,to_id::bigint,COALESCE(photo,''),COALESCE(extra_photos,'[]'),COALESCE(audio,'')
                FROM messages WHERE id=$1::bigint AND COALESCE(deleted_at,0)=0", &[&id]).await?;
            let Some(row)=row else {return Ok(Access::Missing);};
            let (from,to):(i64,i64)=(row.get(0),row.get(1));
            let other=if from==uid {to} else if to==uid {from} else {return Ok(Access::Forbidden);};
            if tx.query_opt("SELECT 1 FROM blocks WHERE (from_id=$1::bigint AND to_id=$2::bigint) OR (from_id=$2::bigint AND to_id=$1::bigint)",
                &[&uid,&other]).await?.is_some(){return Ok(Access::Forbidden);}
            let first:String=row.get(2); let extras:&str=row.get(3);
            let mut photos=if first.is_empty(){Vec::new()}else{vec![first]};
            if !extras.is_empty(){photos.extend(serde_json::from_str::<Vec<String>>(extras)?);}
            if index<0 || !photos.is_empty() && index as usize>=photos.len(){return Ok(Access::Missing);}
            let audio:String=row.get(4);
            let (path,audio)=if audio.is_empty(){(photos.get(index as usize).cloned().unwrap_or_default(),false)}else{(audio,true)};
            Ok(if path.is_empty(){Access::Missing}else{Access::Allowed{path,audio}})
        })).await?.0;
        Ok(match access {
            Access::SignedOut=>crate::account_settings::signed_out(lang),Access::Banned=>crate::account_settings::report_ban_response(lang),
            Access::Forbidden=>(StatusCode::FORBIDDEN,[(header::CONTENT_TYPE,"text/html; charset=utf-8")],
                "<!doctype html>\n<html lang=en>\n<title>403 Forbidden</title>\n<h1>Forbidden</h1>\n<p>You don't have the permission to access the requested resource. It is either read-protected or not readable by the server.</p>\n").into_response(),
            Access::Missing=>missing(),
            Access::Allowed{path,audio}=>{
                let Some(source)=self.chat_upload(&path) else{return Ok(missing());};
                if !audio && params.get("s").is_some_and(|v|v=="sm") {
                    if let Some(cache)=self.cached(&source,&format!("chat/{path}"),false).await {return Ok(internal("private_thumbs",&cache,"image/jpeg"));}
                }
                let (file,mime)=if audio {
                    let converted=format!("{path}.listen.m4a");
                    if safe(&self.uploads,&converted).is_some_and(|cached|fresh(&source,&cached,true)) {
                        (converted,"audio/mp4")
                    } else if let Some(cache)=self.cached(&source,&format!("chat-audio/{converted}"),true).await {
                        return Ok(internal("private_thumbs",&cache,"audio/mp4"));
                    } else {(path.clone(),content_type(&path,true))}
                }else{(path.clone(),content_type(&path,false))};
                internal("private_uploads",&file,mime)
            }
        })
    }

    pub(crate) async fn profile(&self,path:&str,query:&str)->Response {
        // Only public profile-photo paths belong under /media; chat files require the API ACL.
        if !path.split_once('/').is_some_and(|(owner,name)| !owner.is_empty() && owner.bytes().all(|b|b.is_ascii_digit())
            && !name.contains('/') && name.to_ascii_lowercase().ends_with(".jpg")) {return missing();}
        let Some(source)=safe(&self.uploads,path) else {return missing();};
        if params(query).get("s").is_some_and(|v|v=="sm") {
            if let Some(cache)=self.cached(&source,&format!("media/{path}"),false).await {
                return internal("thumbs",&cache,"image/jpeg");
            }
        }
        internal("uploads",path,content_type(path,false))
    }

    pub(crate) async fn static_thumbnail(&self,path:&str)->Response {
        let Some(source)=safe(&self.public,path) else {return missing();};
        if let Some(cache)=self.cached(&source,&format!("public/{path}"),false).await {
            internal("thumbs",&cache,"image/jpeg")
        } else {
            internal("static_files",path,content_type(path,false))
        }
    }

    pub(crate) fn erase_chat_uploads(&self, uploads: Vec<String>) {
        for path in uploads {
            if !path.starts_with("chat/") { continue; }
            let audio = format!("{path}.listen.m4a");
            for (root, file) in [(&self.uploads,path.clone()),(&self.uploads,audio.clone()),
                (&self.thumbs,format!("chat/{path}")),(&self.thumbs,format!("chat-audio/{audio}"))] {
                if let Some(file) = safe(root,&file) && std::fs::remove_file(file).is_err() {
                    eprintln!("Could not remove a deleted message attachment");
                }
            }
        }
    }

    pub(crate) fn chat_upload(&self,path:&str)->Option<PathBuf> {
        if !path.starts_with("chat/") {return None;}
        safe(&self.uploads,path)
    }

    pub(crate) fn erase_profile_upload(&self,uid:i64,path:&str) {
        if !path.split_once('/').is_some_and(|(owner,name)|owner==uid.to_string() && !name.contains('/') && name.ends_with(".jpg")) {return;}
        let thumb=format!("media/{path}");
        for (root,relative) in [(&self.uploads,path),(&self.thumbs,thumb.as_str())] {
            if let Some(file)=safe(root,relative) && std::fs::remove_file(file).is_err() {eprintln!("Could not remove deleted profile photo");}
        }
    }

    pub(crate) fn save_chat_voice(&self,uid:i64,extension:&str,bytes:&[u8])->Result<String,StatusCode> {
        if !matches!(extension,"webm"|"wav"|"ogg"|"mp3"|"m4a"|"aac") {return Err(StatusCode::BAD_REQUEST);}
        self.save_upload(uid,true,extension,bytes)
    }

    pub(crate) fn save_photo(&self,uid:i64,chat:bool,bytes:&[u8])->Result<String,StatusCode> {
        self.save_upload(uid,chat,"jpg",bytes)
    }

    fn save_upload(&self,uid:i64,chat:bool,extension:&str,bytes:&[u8])->Result<String,StatusCode> {
        use std::io::Write;
        let mut random=[0;16];
        getrandom::fill(&mut random).map_err(|_|StatusCode::SERVICE_UNAVAILABLE)?;
        let name=random.iter().map(|b|format!("{b:02x}")).collect::<String>();
        let relative=format!("{}{uid}/{name}.{extension}",if chat {"chat/"} else {""});
        let path=self.uploads.join(&relative);
        let parent=path.parent().ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
        std::fs::create_dir_all(parent).map_err(|_|StatusCode::SERVICE_UNAVAILABLE)?;
        if !parent.canonicalize().is_ok_and(|canonical|canonical==parent) {return Err(StatusCode::SERVICE_UNAVAILABLE);}
        let mut output=std::fs::OpenOptions::new().create_new(true).write(true).open(&path).map_err(|_|StatusCode::SERVICE_UNAVAILABLE)?;
        if output.write_all(bytes).is_err() {
            let _=std::fs::remove_file(path);
            return Err(StatusCode::SERVICE_UNAVAILABLE);
        }
        Ok(relative)
    }

    async fn cached(&self,source:&Path,relative:&str,audio:bool)->Option<String> {
        let cache=self.thumbs.join(relative);
        if let Some(existing)=safe(&self.thumbs,relative) {
            if fresh(source,&existing,audio) {return Some(relative.to_owned());}
        }
        let slot=Arc::clone(&self.slots).try_acquire_owned().ok()?;
        let source=source.to_owned(); let root=self.thumbs.clone(); let relative=relative.to_owned();
        tokio::task::spawn_blocking(move || {
            let _slot=slot;
            let parent=cache.parent()?;
            std::fs::create_dir_all(parent).ok()?;
            if !parent.canonicalize().ok()?.starts_with(root){return None;}
            let mut random=[0;8]; getrandom::fill(&mut random).ok()?;
            let suffix:String=random.iter().map(|b|format!("{b:02x}")).collect();
            let temporary=cache.with_extension(format!("{suffix}.{}",if audio {"m4a"} else {"jpg"}));
            let mut command=std::process::Command::new("/usr/bin/timeout");
            command.arg(Duration::from_secs(if audio {30}else{8}).as_secs().to_string())
                .arg("/usr/bin/ffmpeg").args(["-nostdin","-v","error","-y","-threads","1","-i"]).arg(source);
            if audio {
                command.args(["-vn","-ac","1","-ar","44100","-c:a","aac","-b:a","64k","-movflags","+faststart"]);
            } else {
                command.args(["-vf","scale='min(192,iw)':'min(192,ih)':force_original_aspect_ratio=decrease","-frames:v","1","-q:v","5","-threads","1"]);
            }
            let status=command.arg(&temporary).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status();
            if !status.is_ok_and(|s|s.success()) || std::fs::rename(&temporary,&cache).is_err(){let _=std::fs::remove_file(&temporary);return None;}
            Some(relative)
        }).await.ok().flatten()
    }
}

fn fresh(source:&Path,cached:&Path,audio:bool)->bool {
    cached.metadata().ok().zip(source.metadata().ok()).is_some_and(|(cached,original)|
        (!audio || cached.len()>32) && cached.modified().ok().zip(original.modified().ok()).is_some_and(|(a,b)|a>=b))
}

fn params(query:&str)->HashMap<std::borrow::Cow<'_,str>,std::borrow::Cow<'_,str>> {
    let mut fields=HashMap::new(); for (k,v) in form_urlencoded::parse(query.as_bytes()){fields.entry(k).or_insert(v);} fields
}

fn safe(root:&Path,relative:&str)->Option<PathBuf> {
    let path=Path::new(relative);
    if relative.is_empty()||relative.contains("..")||!relative.bytes().all(|b|b.is_ascii_alphanumeric()||matches!(b,b'/'|b'-'|b'_'|b'.'))
        || !path.components().all(|part|matches!(part,Component::Normal(_))){return None;}
    let file=root.join(path);
    if !file.symlink_metadata().ok()?.file_type().is_file(){return None;}
    let canonical=file.canonicalize().ok()?;
    canonical.starts_with(root).then_some(canonical)
}

fn internal(kind:&str,path:&str,mime:&'static str)->Response {
    let Ok(location)=HeaderValue::from_str(&format!("/_wiring_{kind}/{path}")) else {return missing();};
    let mut response=StatusCode::OK.into_response();
    response.headers_mut().insert("x-accel-redirect",location);
    response.headers_mut().insert(header::CONTENT_TYPE,HeaderValue::from_static(mime));
    response
}

fn content_type(file:&str,audio:bool)->&'static str {
    match file.rsplit_once('.').map(|(_,extension)|extension) {
        Some("jpg"|"jpeg")=>"image/jpeg",Some("png")=>"image/png",Some("gif")=>"image/gif",Some("webp")=>"image/webp",
        Some("webm") if audio=>"audio/webm",Some("m4a"|"mp4") if audio=>"audio/mp4",Some("ogg") if audio=>"audio/ogg",
        Some("mp3") if audio=>"audio/mpeg",Some("wav") if audio=>"audio/wav",Some("aac") if audio=>"audio/aac",
        Some("svg")=>"image/svg+xml",Some("css")=>"text/css",Some("js")=>"text/javascript",
        Some("woff")=>"font/woff",Some("woff2")=>"font/woff2",Some("ico")=>"image/vnd.microsoft.icon",
        _=>"application/octet-stream",
    }
}

fn missing()->Response {
    (StatusCode::NOT_FOUND,[(header::CONTENT_TYPE,"text/html; charset=utf-8")],
        "<!doctype html>\n<html lang=en>\n<title>404 Not Found</title>\n<h1>Not Found</h1>\n<p>The requested URL was not found on the server. If you entered the URL manually please check your spelling and try again.</p>\n").into_response()
}
