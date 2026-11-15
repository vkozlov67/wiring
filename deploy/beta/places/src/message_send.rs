use std::{env,error::Error,time::Duration};

use axum::{Json,http::StatusCode,response::{IntoResponse,Response}};
use base64::{Engine,engine::general_purpose::{STANDARD,URL_SAFE,URL_SAFE_NO_PAD}};
use ring::{aead,agreement,hkdf,rand::{SecureRandom,SystemRandom},signature::{ECDSA_P256_SHA256_FIXED_SIGNING,EcdsaKeyPair,KeyPair}};
use serde_json::{Value,json};

use crate::{chat_write::{self,SendMessage,Writer},mobile_http::string_field};

pub(crate) struct Delivery {
    http:reqwest::Client, enabled:bool, subject:String, slots:tokio::sync::Semaphore,
}
struct Recipient { uid:i64, email:String, language:String, notify:bool, push:bool }
enum Outcome { Missing,Banned,InvalidTo,InvalidBody,InvalidReply,NoMatch,Blocked,PeerMissing,ReplyMissing,Duplicate(i64),Sent(i64,Option<(Recipient,String)>) }

impl Delivery {
    pub(crate) fn new()->Result<Self,Box<dyn Error>> {
        let http=reqwest::Client::builder().https_only(true).no_proxy().redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(5)).build()?;
        let subject=env::var("WEB_PUSH_VAPID_SUBJECT").unwrap_or_else(|_|"mailto:noreply@wiring.date".into());
        let enabled=env::var("WIRING_MESSAGE_TEST_NO_PUSH").as_deref()!=Ok("1");
        Ok(Self{http,enabled,subject,slots:tokio::sync::Semaphore::new(8)})
    }

    pub(crate) async fn send(&self,db:&Writer,uid:i64,fields:Value,now:i64,lang:&str)->Result<Response,StatusCode> {
        let account=db.transaction(move |tx|Box::pin(async move {
            let row=tx.query_opt("SELECT name,COALESCE(report_banned_at,0)!=0 FROM users WHERE id=$1::bigint AND COALESCE(deleted_at,0)=0", &[&uid]).await?;
            let Some(row)=row else {return Ok(None);};
            let banned:bool=row.get(1);
            if !banned {tx.execute("UPDATE users SET last_seen=$2::bigint WHERE id=$1::bigint", &[&uid,&now]).await?;}
            Ok(Some((row.get::<_,String>(0),banned)))
        })).await?.0;
        let result=match account {
            None=>Outcome::Missing, Some((_,true))=>Outcome::Banned,
            Some((name,false))=>{
                let target=integer(&fields["to_id"]);
                let body=string_field(&fields,"body").trim().to_owned();
                let reply=&fields["reply_to_id"];
                let reply=if reply.is_null() || reply=="" || reply=="0" || reply.as_f64()==Some(0.0) || reply==false {Some(None)}
                    else {integer(reply).map(Some)};
                let client:String=string_field(&fields,"client_id").trim().chars().take(64).collect();
                match (target,reply) {
                    (None,_)=>Outcome::InvalidTo,
                    _ if !(1..=1000).contains(&body.chars().count()) || body.contains('\0')=>Outcome::InvalidBody,
                    (_,None)=>Outcome::InvalidReply,
                    (Some(other),Some(reply_to_id))=>{
                        db.transaction(move |tx|Box::pin(async move {
                            let pair:[&(dyn tokio_postgres::types::ToSql+Sync);2]=[&uid,&other];
                            chat_write::lock_pair(tx,&pair).await?;
                            let (blocked,matched)=chat_write::pair_access(tx,&pair).await?;
                            if blocked {return Ok(Outcome::Blocked);}
                            if !matched {return Ok(Outcome::NoMatch);}
                            let row=tx.query_opt("SELECT email,language,COALESCE(is_seed,0)!=0,COALESCE(notify_enabled,0)!=0,COALESCE(notify_push,0)!=0
                                FROM users WHERE id=$1::bigint AND COALESCE(deleted_at,0)=0", &[&other]).await?;
                            let Some(row)=row else {return Ok(Outcome::PeerMissing);};
                            let recipient=Recipient{uid:other,email:row.get(0),language:row.get(1),notify:row.get::<_,bool>(3)&&!row.get::<_,bool>(2),push:row.get(4)};
                            let snippet=body.split_whitespace().collect::<Vec<_>>().join(" ");
                            let snippet=if snippet.chars().count()>80 {format!("{}…",take(&snippet,79))} else {snippet};
                            let notice=if snippet.is_empty() {
                                if recipient.language=="en" {format!("{name} sent a message")} else {format!("{name} написал(а)")}
                            } else {format!("{name}: {snippet}")};
                            let notify=recipient.notify && !demo(&recipient.email);
                            let input=SendMessage{uid,other_id:other,body,reply_to_id,client_id:(!client.is_empty()&&!client.contains('\0')).then_some(client),
                                created_at:now,notice_body:notify.then(||take(&notice,280)),media:None};
                            Ok(match chat_write::write_message(tx,&input).await? {
                                chat_write::Outcome::Blocked=>Outcome::Blocked,
                                chat_write::Outcome::NoMatch=>Outcome::NoMatch,
                                chat_write::Outcome::ReplyMissing=>Outcome::ReplyMissing,
                                chat_write::Outcome::Duplicate{id}=>Outcome::Duplicate(id),
                                chat_write::Outcome::Sent{id,..}=>Outcome::Sent(id,(notify && recipient.push).then_some((recipient,notice))),
                            })
                        })).await?.0
                    }
                }
            }
        };
        let error=|status,en,ru|(status,Json(json!({"ok":false,"error":if lang=="en" {en} else {ru}}))).into_response();
        Ok(match result {
            Outcome::Missing=>crate::account_settings::signed_out(lang),
            Outcome::Banned=>crate::account_settings::report_ban_response(lang),
            Outcome::InvalidTo=>error(StatusCode::BAD_REQUEST,"No recipient","нет адресата"),
            Outcome::InvalidBody=>error(StatusCode::BAD_REQUEST,"Message must contain between 1 and 1000 characters","сообщение: 1–1000 символов"),
            Outcome::InvalidReply=>error(StatusCode::BAD_REQUEST,"Invalid response","некорректный ответ"),
            Outcome::ReplyMissing=>error(StatusCode::BAD_REQUEST,"The message you are replying to was not found","сообщение для ответа не найдено"),
            Outcome::NoMatch=>error(StatusCode::FORBIDDEN,"You can message after a mutual like","написать можно после взаимного лайка"),
            Outcome::Blocked=>error(StatusCode::FORBIDDEN,"This person is hidden","этот человек скрыт"),
            Outcome::PeerMissing=>error(StatusCode::NOT_FOUND,"Person not found","человек не найден"),
            Outcome::Duplicate(id)=>Json(json!({"ok":true,"duplicate":true,"id":id})).into_response(),
            Outcome::Sent(id,recipient)=>{
                if let Some((recipient,notice))=recipient {self.push(db,recipient.uid,uid,"message",&notice,now).await;}
                Json(json!({"ok":true,"id":id})).into_response()
            }
        })
    }

    pub(crate) async fn push(&self,db:&Writer,to:i64,from:i64,kind:&str,body:&str,now:i64) {
        if !self.enabled { return; }
        let Ok(_slot)=self.slots.try_acquire() else { eprintln!("Push capacity exceeded");return; };
        let Ok(Json((tokens,web,key)))=db.transaction(move |tx|Box::pin(async move {
            if tx.query_opt("SELECT 1 FROM users WHERE id=$1::bigint AND COALESCE(deleted_at,0)=0 AND notify_enabled<>0 AND notify_push<>0", &[&to]).await?.is_none() {
                return Ok((Vec::new(),Vec::new(),None));
            }
            let tokens:Vec<String>=tx.query("SELECT token FROM mobile_devices WHERE user_id=$1::bigint", &[&to]).await?.into_iter().map(|r|r.get(0)).collect();
            let web:Vec<(String,String,String)>=tx.query("SELECT endpoint,p256dh,auth FROM push_subscriptions WHERE user_id=$1::bigint", &[&to]).await?
                .into_iter().map(|r|(r.get(0),r.get(1),r.get(2))).collect();
            let key=if web.is_empty() {None} else {tx.query_opt("SELECT private_pem FROM push_vapid WHERE id=1", &[]).await?.map(|r|r.get::<_,String>(0))};
            Ok((tokens,web,key))
        })).await else {return;};
        let url=if kind=="like" {"/likes".to_owned()} else {format!("/chats/{from}")};
        let tag=format!("{kind}-{from}"); let text=take(body,180);
        let mut gone_tokens=Vec::new(); let mut gone_web=Vec::new();
        for tokens in tokens.chunks(100) {
            let messages:Vec<_>=tokens.iter().map(|token|json!({"to":token,"title":"WIRING","body":text,"sound":"default","priority":"high",
                "ttl":43200,"data":{"url":url,"tag":tag},"channelId":"default"})).collect();
            if let Ok(response)=self.http.post("https://exp.host/--/api/v2/push/send").json(&messages).send().await
                && let Ok(response)=response.error_for_status() && let Ok(value)=response.json::<Value>().await
                && let Some(data)=value["data"].as_array() {
                for (token,ticket) in tokens.iter().zip(data) {
                    if ticket["status"]=="error" {
                        match ticket["details"]["error"].as_str() {
                            Some("DeviceNotRegistered") => gone_tokens.push(token.clone()),
                            Some("InvalidCredentials") => eprintln!("Expo push rejected: InvalidCredentials (check APNs/FCM in EAS)"),
                            _ => eprintln!("Expo push rejected by provider"),
                        }
                    }
                }
            } else {
                eprintln!("Expo push request failed or returned an invalid response");
            }
        }
        if let Some(key)=key.and_then(|key|signing_key(&key)) {
            let payload=json!({"title":"WIRING","body":text,"url":url,"tag":tag}).to_string();
            for (endpoint,public,auth) in web {
                let Ok(destination)=reqwest::Url::parse(&endpoint) else {continue;};
                if !allowed(&destination) { continue; }
                let origin=destination.origin().ascii_serialization();
                let Some(vapid)=vapid(&key,&origin,&self.subject,now) else {continue;};
                let Some(record)=seal(&public,&auth,payload.as_bytes()) else {continue;};
                if let Ok(response)=self.http.post(destination).header("Authorization",vapid).header("TTL","43200")
                    .header("Content-Encoding","aes128gcm").header("Content-Type","application/octet-stream").body(record).send().await
                    && matches!(response.status().as_u16(),404|410) {gone_web.push(endpoint);}
            }
        }
        if !gone_tokens.is_empty() || !gone_web.is_empty() {
            let _=db.transaction(move |tx|Box::pin(async move {
                tx.execute("DELETE FROM mobile_devices WHERE user_id=$1::bigint AND token=ANY($2::text[])", &[&to,&gone_tokens]).await?;
                tx.execute("DELETE FROM push_subscriptions WHERE user_id=$1::bigint AND endpoint=ANY($2::text[])", &[&to,&gone_web]).await?;
                Ok(())
            })).await;
        }
    }
}

fn allowed(url:&reqwest::Url)->bool {
    if url.scheme()!="https" || url.port().is_some_and(|p|p!=443) || !url.username().is_empty() || url.password().is_some() {return false;}
    let host=url.host_str().unwrap_or("");
    matches!(host,"fcm.googleapis.com"|"updates.push.services.mozilla.com"|"web.push.apple.com"|"wns.windows.com")
        || host.ends_with(".notify.windows.com") || host.ends_with(".push.apple.com") || host.ends_with(".push.services.mozilla.com")
}

fn signing_key(pem:&str)->Option<EcdsaKeyPair> {
    let data:String=pem.lines().filter(|line|!line.starts_with("-----")).map(str::trim).collect();
    EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING,&STANDARD.decode(data).ok()?,&SystemRandom::new()).ok()
}
fn vapid(key:&EcdsaKeyPair,origin:&str,subject:&str,now:i64)->Option<String> {
    let header=URL_SAFE_NO_PAD.encode(br#"{"typ":"JWT","alg":"ES256"}"#);
    let claims=URL_SAFE_NO_PAD.encode(json!({"aud":origin,"sub":subject,"exp":now+43200}).to_string());
    let input=format!("{header}.{claims}");
    let signature=key.sign(&SystemRandom::new(),input.as_bytes()).ok()?;
    Some(format!("vapid t={input}.{}, k={}",URL_SAFE_NO_PAD.encode(signature.as_ref()),URL_SAFE_NO_PAD.encode(key.public_key().as_ref())))
}
fn decode(value:&str)->Option<Vec<u8>> {URL_SAFE_NO_PAD.decode(value).or_else(|_|URL_SAFE.decode(value)).ok()}
struct Length(usize);
impl hkdf::KeyType for Length {fn len(&self)->usize {self.0}}

fn expand<const N:usize>(key:&hkdf::Prk,info:&[u8])->Option<[u8;N]> {
    let inputs=[info]; let mut data=[0;N]; key.expand(&inputs,Length(N)).ok()?.fill(&mut data).ok()?; Some(data)
}

fn seal(public:&str,auth:&str,payload:&[u8])->Option<Vec<u8>> {
    let browser=decode(public)?; let auth=decode(auth)?;
    if browser.len()!=65 || auth.len()!=16 || payload.len()>4096-17 {return None;}
    let rng=SystemRandom::new(); let secret=agreement::EphemeralPrivateKey::generate(&agreement::ECDH_P256,&rng).ok()?;
    let server=secret.compute_public_key().ok()?;
    let peer=agreement::UnparsedPublicKey::new(&agreement::ECDH_P256,&browser);
    let shared=agreement::agree_ephemeral(secret,&peer,|bytes|bytes.to_vec()).ok()?;
    let mut context=b"WebPush: info\0".to_vec(); context.extend_from_slice(&browser); context.extend_from_slice(server.as_ref());
    let ikm=expand::<32>(&hkdf::Salt::new(hkdf::HKDF_SHA256,&auth).extract(&shared),&context)?;
    let mut salt=[0;16]; rng.fill(&mut salt).ok()?;
    let prk=hkdf::Salt::new(hkdf::HKDF_SHA256,&salt).extract(&ikm);
    let cek=expand::<16>(&prk,b"Content-Encoding: aes128gcm\0")?;
    let nonce=expand::<12>(&prk,b"Content-Encoding: nonce\0")?;
    let key=aead::LessSafeKey::new(aead::UnboundKey::new(&aead::AES_128_GCM,&cek).ok()?);
    let mut content=payload.to_vec(); content.push(2);
    key.seal_in_place_append_tag(aead::Nonce::assume_unique_for_key(nonce),aead::Aad::empty(),&mut content).ok()?;
    let mut output=salt.to_vec(); output.extend_from_slice(&4096_u32.to_be_bytes()); output.push(65);
    output.extend_from_slice(server.as_ref()); output.extend_from_slice(&content); Some(output)
}

pub(crate) fn integer(value:&Value)->Option<i64> {
    match value {Value::String(s)=>s.trim().parse().ok(),Value::Bool(v)=>Some(i64::from(*v)),
        Value::Number(n)=>n.as_i64().or_else(||n.as_f64().filter(|n|(-9_223_372_036_854_775_808.0..9_223_372_036_854_775_808.0).contains(n)).map(|n|n as i64)),_=>None}
}
pub(crate) fn demo(email:&str)->bool {let email=email.trim().to_lowercase();["@wiring.guest","@wiring.demo","@wiring.app","@example.com"].iter().any(|s|email.ends_with(s))}
pub(crate) fn take(value:&str,n:usize)->String {value.chars().take(n).collect()}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_ciphertext_decrypts_for_subscriber() {
        let rng=SystemRandom::new();
        let secret=agreement::EphemeralPrivateKey::generate(&agreement::ECDH_P256,&rng).unwrap();
        let public=secret.compute_public_key().unwrap();
        let auth=[7;16];
        let payload=b"fixture message";
        let wire=seal(&URL_SAFE_NO_PAD.encode(public.as_ref()),&URL_SAFE_NO_PAD.encode(auth),payload).unwrap();
        assert_eq!(&wire[16..21],&[0,0,16,0,65]);
        let server=&wire[21..86];
        let peer=agreement::UnparsedPublicKey::new(&agreement::ECDH_P256,server);
        let shared=agreement::agree_ephemeral(secret,&peer,|bytes|bytes.to_vec()).unwrap();
        let mut context=b"WebPush: info\0".to_vec(); context.extend_from_slice(public.as_ref()); context.extend_from_slice(server);
        let ikm=expand::<32>(&hkdf::Salt::new(hkdf::HKDF_SHA256,&auth).extract(&shared),&context).unwrap();
        let prk=hkdf::Salt::new(hkdf::HKDF_SHA256,&wire[..16]).extract(&ikm);
        let cek=expand::<16>(&prk,b"Content-Encoding: aes128gcm\0").unwrap();
        let nonce=expand::<12>(&prk,b"Content-Encoding: nonce\0").unwrap();
        let key=aead::LessSafeKey::new(aead::UnboundKey::new(&aead::AES_128_GCM,&cek).unwrap());
        let mut ciphertext=wire[86..].to_vec();
        let decoded=key.open_in_place(aead::Nonce::assume_unique_for_key(nonce),aead::Aad::empty(),&mut ciphertext).unwrap();
        assert_eq!(&decoded[..decoded.len()-1],payload);
        assert_eq!(decoded.last(),Some(&2));
    }

    #[test]
    fn push_destination_requires_recognized_https_origin() {
        for url in ["http://fcm.googleapis.com/push","https://fcm.googleapis.com.attacker.invalid/x","https://127.0.0.1/x","https://user@fcm.googleapis.com/x"] {
            assert!(!allowed(&reqwest::Url::parse(url).unwrap()));
        }
        assert!(allowed(&reqwest::Url::parse("https://fcm.googleapis.com/fcm/send/fixture").unwrap()));
    }
}
