use std::{env,error::Error,fs,io::ErrorKind,path::{Component,Path,PathBuf},time::{Duration,SystemTime,UNIX_EPOCH}};

use base64::{Engine,engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::json;
use tokio_postgres::{Client,NoTls,Transaction};

const DAY:i64=86400;
const CLEANUP:&[&str]=&[
    "DELETE FROM sendpulse_contacts WHERE user_id=$1::bigint",
    "UPDATE registration_events SET client_id=NULL,consent_at=NULL,delivery_state=CASE WHEN delivery_state IN ('pending','needs_attribution','sending') THEN 'erased' ELSE delivery_state END WHERE user_id=$1::bigint",
    "DELETE FROM message_reactions WHERE user_id=$1::bigint",
    "DELETE FROM messages WHERE from_id=$1::bigint OR to_id=$1::bigint",
    "DELETE FROM swipes WHERE from_id=$1::bigint OR to_id=$1::bigint",
    "DELETE FROM feed_history WHERE user_id=$1::bigint OR other_id=$1::bigint",
    "DELETE FROM user_tags WHERE user_id=$1::bigint",
    "DELETE FROM user_prompts WHERE user_id=$1::bigint",
    "DELETE FROM photos WHERE user_id=$1::bigint",
    "DELETE FROM albums WHERE user_id=$1::bigint",
    "DELETE FROM blocks WHERE from_id=$1::bigint OR to_id=$1::bigint",
    "DELETE FROM support_replies WHERE user_id=$1::bigint OR (source_kind='report' AND source_id IN (SELECT id FROM reports WHERE from_id=$1::bigint OR to_id=$1::bigint)) OR (source_kind='ticket' AND source_id IN (SELECT id FROM support_tickets WHERE user_id=$1::bigint))",
    "DELETE FROM reports WHERE from_id=$1::bigint OR to_id=$1::bigint",
    "DELETE FROM reads WHERE user_id=$1::bigint OR other_id=$1::bigint",
    "DELETE FROM notifications WHERE user_id=$1::bigint OR from_id=$1::bigint",
    "DELETE FROM notes WHERE user_id=$1::bigint OR other_id=$1::bigint",
    "DELETE FROM snoozes WHERE user_id=$1::bigint OR other_id=$1::bigint",
    "DELETE FROM promo_redemptions WHERE user_id=$1::bigint",
    "DELETE FROM referrals WHERE referrer_id=$1::bigint OR referred_id=$1::bigint",
    "DELETE FROM password_resets WHERE user_id=$1::bigint",
    "DELETE FROM email_verifications WHERE user_id=$1::bigint",
    "DELETE FROM oauth_identities WHERE user_id=$1::bigint",
    "DELETE FROM oauth_native_codes WHERE user_id=$1::bigint",
    "DELETE FROM support_tickets WHERE user_id=$1::bigint",
    "DELETE FROM device_visits WHERE user_id=$1::bigint",
    "DELETE FROM filter_events WHERE user_id=$1::bigint",
    "DELETE FROM session_handoffs WHERE user_id=$1::bigint",
    "DELETE FROM push_subscriptions WHERE user_id=$1::bigint",
    "DELETE FROM mobile_devices WHERE user_id=$1::bigint",
    "DELETE FROM mobile_refresh_tokens WHERE user_id=$1::bigint",
    "DELETE FROM chat_openers WHERE user_id=$1::bigint OR other_id=$1::bigint",
];

const SCRUB:&str="UPDATE users SET email=$2,password_hash=$3,name='',age=0,city='',gender='hidden',looking_for='',bio='',photo='',job='',intent='',
    height=NULL,communication='',language='ru',last_seen=0,premium_until=0,referral_code=NULL,seek_place='',hide_tags='',email_verified_at=NULL
    WHERE id=$1::bigint";

pub(crate) async fn run(plan:bool)->Result<(),Box<dyn Error>> {
    let url=env::var("WIRING_PURGE_DATABASE_URL")?;
    let mut config:tokio_postgres::Config=url.parse().map_err(|_|"Invalid purge database config")?;
    config.connect_timeout(Duration::from_secs(4));
    let (mut db,connection)=config.connect(NoTls).await?;
    tokio::spawn(async move {let _=connection.await;});
    let now=SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() as i64;
    let (deleted,guests)=purge(&mut db,now,plan).await.map_err(|error| {
        if let Some(db)=error.downcast_ref::<tokio_postgres::Error>() {
            format!("Account cleanup database error (SQLSTATE {})",db.code().map_or("unknown",|code|code.code()))
        } else {"Account cleanup could not complete".to_owned()}
    })?;
    println!("{}",json!({"deleted_accounts":deleted,"expired_guests":guests,"plan_only":plan}));
    Ok(())
}

async fn purge(db:&mut Client,now:i64,plan:bool)->Result<(u64,u64),Box<dyn Error+Send+Sync>> {
    db.batch_execute("SET statement_timeout='12s'; SET lock_timeout='4s'").await?;
    if !db.query_one("SELECT pg_try_advisory_lock(739201,11)",&[]).await?.get::<_,bool>(0) {return Ok((0,0));}
    let deleted=db.query("SELECT id::bigint FROM users WHERE deleted_at>0 AND deleted_at<$1::bigint
        AND email<>('deleted-'||id||'@deleted.wiring') ORDER BY id", &[&(now-7*DAY)]).await?;
    let guests=if env::var("WIRING_PURGE_EXPIRED_GUESTS").as_deref()==Ok("1") {
        db.query("SELECT id::bigint FROM users WHERE email LIKE 'guest-%@wiring.guest' AND created_at<$1::bigint ORDER BY id", &[&(now-3*DAY)]).await?
    } else {Vec::new()};
    if plan {return Ok((deleted.len() as u64,guests.len() as u64));}
    let root=PathBuf::from(env::var("UPLOAD_DIR")?);
    if !root.is_absolute() || !root.is_dir() || root.symlink_metadata()?.file_type().is_symlink() {return Err("Invalid upload directory".into());}
    let root=root.canonicalize()?;
    let cache=env::var_os("WIRING_MEDIA_CACHE_DIR").map(PathBuf::from).map(|dir|dir.canonicalize()).transpose()?;
    let mut counts=(0,0);
    for (rows,guest) in [(guests,true),(deleted,false)] {
        for row in rows {
            let uid:i64=row.get(0);
            let tx=db.transaction().await?;
            let condition=if guest {"email LIKE 'guest-%@wiring.guest' AND created_at<$2::bigint"}
                else {"deleted_at>0 AND deleted_at<$2::bigint AND email<>('deleted-'||id||'@deleted.wiring')"};
            let cutoff=now-if guest {3*DAY} else {7*DAY};
            if tx.query_opt(&format!("SELECT id FROM users WHERE id=$1::bigint AND {condition} FOR UPDATE"),&[&uid,&cutoff]).await?.is_none() {continue;}
            let paths=cleanup(&tx,uid).await?;
            if guest {
                if !tx.query_one("SELECT public.wiring_remove_expired_guest($1::bigint)", &[&uid]).await?.get::<_,bool>(0) {continue;}
            }
            else {
                let mut random=[0;24]; getrandom::fill(&mut random).map_err(|_|"Could not generate scrubbed password")?;
                let pass=crate::password::hash_password(URL_SAFE_NO_PAD.encode(random)).await.map_err(|_|"Could not hash scrubbed password")?;
                let email=format!("deleted-{uid}@deleted.wiring");
                tx.execute(SCRUB,&[&uid,&email,&pass]).await?;
            }
            // Erase files before commit so a filesystem error leaves a retriable database record.
            // A deleted user cannot access these files through authenticated routes.
            for path in paths {
                remove_upload(&root,&path)?;
                if let Some(cache)=cache.as_deref() {
                    remove_upload(cache,&format!("chat/{path}"))?;
                    if path.ends_with(".listen.m4a") {remove_upload(cache,&format!("chat-audio/{path}"))?;}
                }
            }
            for dir in [root.join(uid.to_string()),root.join("chat").join(uid.to_string())] {
                if let Ok(meta)=dir.symlink_metadata() && meta.file_type().is_dir() && dir.canonicalize()?.starts_with(&root) {
                    fs::remove_dir_all(dir)?;
                }
            }
            if let Some(cache)=cache.as_deref() {
                for dir in [cache.join("media").join(uid.to_string()),cache.join("chat/chat").join(uid.to_string()),
                    cache.join("chat-audio/chat").join(uid.to_string())] {
                    if let Ok(meta)=dir.symlink_metadata() && meta.file_type().is_dir() && dir.canonicalize()?.starts_with(cache) {
                        fs::remove_dir_all(dir)?;
                    }
                }
            }
            tx.commit().await?;
            if guest {counts.1+=1;} else {counts.0+=1;}
        }
    }
    Ok(counts)
}

async fn cleanup(tx:&Transaction<'_>,uid:i64)->Result<Vec<String>,Box<dyn Error+Send+Sync>> {
    let mut paths=Vec::new();
    let rows=tx.query("SELECT COALESCE(photo,''),COALESCE(extra_photos,'[]'),COALESCE(audio,'') FROM messages
        WHERE from_id=$1::bigint OR to_id=$1::bigint", &[&uid]).await?;
    for row in rows {
        let photo:String=row.get(0); let audio:String=row.get(2);
        if !photo.is_empty(){paths.push(photo);}
        if !audio.is_empty(){paths.push(format!("{audio}.listen.m4a"));paths.push(audio);}
        let extra:&str=row.get(1);
        if !extra.is_empty(){paths.extend(serde_json::from_str::<Vec<String>>(extra)?);}
    }
    for sql in CLEANUP {tx.execute(*sql,&[&uid]).await?;}
    Ok(paths)
}

fn remove_upload(root:&Path,relative:&str)->std::io::Result<()> {
    let path=Path::new(relative);
    if relative.is_empty() || relative.contains('\\') || relative.contains("..") || !path.components().all(|part|matches!(part,Component::Normal(_))) {return Ok(());}
    let target=root.join(path);
    let canonical=match target.canonicalize(){Ok(target)=>target,Err(err) if err.kind()==ErrorKind::NotFound=>return Ok(()),Err(err)=>return Err(err)};
    if !canonical.starts_with(root) || !target.symlink_metadata()?.file_type().is_file() {return Ok(());}
    fs::remove_file(target)
}
