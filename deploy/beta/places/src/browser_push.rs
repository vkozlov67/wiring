use std::env;

use axum::{Json, http::StatusCode, response::{IntoResponse, Response}};
use base64::{Engine, engine::general_purpose::{STANDARD,URL_SAFE_NO_PAD}};
use ring::{rand::SystemRandom, signature::{ECDSA_P256_SHA256_FIXED_SIGNING,EcdsaKeyPair,KeyPair}};
use serde_json::{Value, json};

use crate::{chat_write::Writer, mobile_http::string_field};

pub(crate) struct BrowserPush { key: Option<String> }

enum Outcome { SignedOut, Banned, Disabled, Invalid, KeyInvalid, Key(String), Done }

impl BrowserPush {
    pub(crate) fn new() -> Self {
        Self { key: env::var("WIRING_VAPID_PUBLIC_KEY").ok().map(|key|key.trim().to_owned()).filter(|key|!key.is_empty()) }
    }

    pub(crate) async fn serve(&self, db: &Writer, uid: i64, path: &str, fields: Value, now: i64, lang: &str)
        -> Result<Response, StatusCode>
    {
        let read = path == "/api/push/public-key";
        let subscribe = path == "/api/push/subscribe";
        let endpoint = string_field(&fields,"endpoint").trim().to_owned();
        let p256dh = string_field(&fields["keys"],"p256dh").trim().to_owned();
        let auth = string_field(&fields["keys"],"auth").trim().to_owned();
        let key = self.key.clone();
        let result = db.transaction(move |tx| Box::pin(async move {
            let row = tx.query_opt("SELECT COALESCE(report_banned_at,0)!=0,COALESCE(notify_enabled,0)!=0,COALESCE(notify_push,0)!=0
                FROM users WHERE id=$1::bigint AND COALESCE(deleted_at,0)=0 FOR UPDATE", &[&uid]).await?;
            let Some(row) = row else { return Ok(Outcome::SignedOut); };
            if row.get::<_,bool>(0) { return Ok(Outcome::Banned); }
            tx.execute("UPDATE users SET last_seen=$2::bigint WHERE id=$1::bigint", &[&uid,&now]).await?;
            if read {
                if let Some(key) = key {
                    return Ok(if URL_SAFE_NO_PAD.decode(&key).is_ok_and(|bytes|bytes.len()==65 && bytes[0]==4) {
                        Outcome::Key(key)
                    } else { Outcome::KeyInvalid });
                }
                if let Some(row) = tx.query_opt("SELECT public_key FROM push_vapid WHERE id=1", &[]).await? {
                    return Ok(Outcome::Key(row.get(0)));
                }
                let Some((private,public)) = new_keypair() else { return Ok(Outcome::KeyInvalid); };
                tx.execute("INSERT INTO push_vapid(id,private_pem,public_key) VALUES(1,$1,$2) ON CONFLICT(id) DO NOTHING", &[&private,&public]).await?;
                return Ok(Outcome::Key(tx.query_one("SELECT public_key FROM push_vapid WHERE id=1", &[]).await?.get(0)));
            }
            if subscribe {
                if !row.get::<_,bool>(1) || !row.get::<_,bool>(2) { return Ok(Outcome::Disabled); }
                if endpoint.is_empty() || endpoint.chars().count()>2000 || p256dh.is_empty() || auth.is_empty()
                    || [&endpoint,&p256dh,&auth].iter().any(|s|s.contains('\0')) { return Ok(Outcome::Invalid); }
                tx.execute("INSERT INTO push_subscriptions(endpoint,user_id,p256dh,auth,created_at)
                    VALUES($1,$2::bigint,$3,$4,$5::bigint) ON CONFLICT(endpoint) DO UPDATE
                    SET user_id=excluded.user_id,p256dh=excluded.p256dh,auth=excluded.auth,created_at=excluded.created_at",
                    &[&endpoint,&uid,&p256dh,&auth,&now]).await?;
            } else if !endpoint.is_empty() && !endpoint.contains('\0') {
                tx.execute("DELETE FROM push_subscriptions WHERE endpoint=$1 AND user_id=$2::bigint", &[&endpoint,&uid]).await?;
            }
            Ok(Outcome::Done)
        })).await?.0;
        let error = |status, error| (status, Json(json!({"ok":false,"error":error}))).into_response();
        Ok(match result {
            Outcome::SignedOut => crate::account_settings::signed_out(lang),
            Outcome::Banned => crate::account_settings::report_ban_response(lang),
            Outcome::Disabled => error(StatusCode::FORBIDDEN, if lang=="en" {"Enable push notifications first"} else {"сначала включи пуш в уведомлениях"}),
            Outcome::Invalid => error(StatusCode::BAD_REQUEST, "неполная подписка"),
            Outcome::KeyInvalid => error(StatusCode::SERVICE_UNAVAILABLE,"Web push key unavailable"),
            Outcome::Key(key) => Json(json!({"ok":true,"publicKey":key})).into_response(),
            Outcome::Done => Json(json!({"ok":true})).into_response(),
        })
    }
}

fn new_keypair() -> Option<(String,String)> {
    let rng = SystemRandom::new();
    let der = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING,&rng).ok()?;
    let key = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING,der.as_ref(),&rng).ok()?;
    let public = URL_SAFE_NO_PAD.encode(key.public_key().as_ref());
    let data = STANDARD.encode(der.as_ref());
    let mut private = String::from("-----BEGIN PRIVATE KEY-----\n");
    for line in data.as_bytes().chunks(64) { private.push_str(std::str::from_utf8(line).ok()?); private.push('\n'); }
    private.push_str("-----END PRIVATE KEY-----\n");
    Some((private,public))
}
