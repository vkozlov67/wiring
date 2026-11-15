use std::{env, time::Duration};

use axum::{
    Json,
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::{Value, json};
use tokio_postgres::Transaction;

use crate::{
    chat_write::TransactionResult,
    login::Context,
    mobile_http::string_field,
    password::hash_password,
    public_http::header_text,
};

const DAY: i64 = 86_400;

pub(crate) struct Registration {
    mail: Option<Mail>,
    signup_plus: bool,
    managed_events: bool,
    origin: String,
}

struct Mail { client: reqwest::Client, key: String, from: String }

pub(crate) struct NewUser {
    pub email: String,
    pub name: String,
    pub hash: String,
    pub locale: &'static str,
    pub now: i64,
    pub verified: bool,
    pub marketing: bool,
    pub channel: String,
    pub managed: bool,
    pub consent: bool,
    pub client_id: Option<String>,
    pub referral: String,
}

impl Registration {
    pub(crate) fn managed(&self) -> bool { self.managed_events }

    pub(crate) fn load() -> Result<Self, Box<dyn std::error::Error>> {
        let key = env::var("RESEND_API_KEY").unwrap_or_default().trim().to_owned();
        let mail = if key.is_empty() {
            if env::var("WIRING_REGISTRATION_TEST_NO_MAIL").as_deref() != Ok("1") {
                return Err("Rust registration requires a configured mail transport".into());
            }
            None
        } else {
            let client = reqwest::Client::builder().https_only(true).no_proxy()
                .timeout(Duration::from_secs(12)).redirect(reqwest::redirect::Policy::none()).build()?;
            Some(Mail { client, key, from: env::var("MAIL_FROM").unwrap_or_else(|_| "WIRING <noreply@wiring.date>".into()) })
        };
        Ok(Self {
            mail,
            signup_plus: !matches!(env::var("WIRING_SIGNUP_PLUS").unwrap_or_default().trim().to_ascii_lowercase().as_str(),
                                  "0" | "off" | "false" | "no"),
            managed_events: env::var("WIRING_REGISTRATION_MANAGED").as_deref() == Ok("1"),
            origin: env::var("SITE_URL").unwrap_or_else(|_| "https://wiring.date".into()).trim_end_matches('/').to_owned(),
        })
    }

    pub(crate) async fn create(&self, tx: &Transaction<'_>, input: NewUser) -> TransactionResult<Option<i64>> {
        let uid = tx.query_opt(
            "INSERT INTO users (email,password_hash,name,age,city,gender,looking_for,bio,photo,job,intent,height,
              communication,privacy_accepted_at,special_data_consent_at,photo_rights_consent_at,marketing_consent_at,
              email_verified_at,onboard_done,profile_setup_required,is_seed,created_at,last_seen,language,premium_until)
             VALUES ($1,$2,$3,0,'','other','everyone','','','','dating',NULL,'',$4::bigint,NULL,NULL,
              $5::bigint,$6::bigint,0,1,0,$4::bigint,$4::bigint,$7,$8::bigint)
             ON CONFLICT(email) DO NOTHING RETURNING id::bigint",
            &[&input.email,&input.hash,&input.name,&input.now,&input.marketing.then_some(input.now),
              &input.verified.then_some(input.now),&input.locale,&if self.signup_plus {input.now+90*DAY} else {0}],
        ).await?.map(|row| row.get::<_, i64>(0));
        let Some(uid) = uid else { return Ok(None); };
        let native = input.channel.starts_with("native");
        let managed = input.managed && self.managed_events;
        let consent = input.consent && !native;
        let client = if managed && consent { input.client_id } else { None };
        let mode = if managed { "server" } else if native { "native" } else { "legacy" };
        let state = if managed {
            if client.is_some() {"pending"} else if consent {"needs_attribution"} else {"no_consent"}
        } else if native {"no_consent"} else {"legacy_browser"};
        tx.execute(
            "INSERT INTO registration_events (user_id,created_at,channel,mode,consent_at,client_id,delivery_state)
             VALUES($1::bigint,$2::bigint,$3,$4,$5::bigint,$6,$7) ON CONFLICT(user_id) DO NOTHING",
            &[&uid,&input.now,&input.channel,&mode,&consent.then_some(input.now),&client,&state],
        ).await?;
        crate::profile_read::referral(tx, uid, "").await?;
        let code: String = input.referral.to_lowercase().chars().filter(|c| c.is_alphanumeric()).take(16).collect();
        if !code.is_empty() {
            let referrer = tx.query_opt(
                "SELECT id::bigint FROM users WHERE referral_code=$1 AND id != $2::bigint
                   AND COALESCE(is_seed,0)=0 AND email NOT LIKE '%@wiring.guest' AND email != 'demo@wiring.app'",
                &[&code,&uid],
            ).await?.map(|row| row.get::<_, i64>(0));
            if let Some(owner) = referrer {
                let inserted = tx.execute(
                    "INSERT INTO referrals(referrer_id,referred_id,created_at) VALUES($1::bigint,$2::bigint,$3::bigint)
                     ON CONFLICT(referred_id) DO NOTHING", &[&owner,&uid,&input.now],
                ).await?;
                if inserted == 1 {
                    tx.execute("UPDATE users SET premium_until=GREATEST(COALESCE(premium_until,0),$3::bigint)+$4::bigint
                        WHERE id IN ($1::bigint,$2::bigint)", &[&owner,&uid,&input.now,&(30*DAY)]).await?;
                    let body = if input.locale == "en" {"Someone signed up with your link — another 30 days of WIRING+"}
                        else {"по твоей ссылке зарегистрировались — ещё 30 дней WIRING+"};
                    tx.execute("INSERT INTO notifications(user_id,kind,from_id,body,created_at,\"read\")
                        VALUES($1::bigint,'referral',0,$2,$3::bigint,0)", &[&owner,&body,&input.now]).await?;
                }
            }
        }
        Ok(Some(uid))
    }

    pub(crate) async fn register(self: std::sync::Arc<Self>, ctx: Context<'_>, headers: &HeaderMap, fields: Value)
        -> Result<(Response, bool), StatusCode>
    {
        let lang = ctx.locale;
        let error = |code, en, ru| (code, Json(json!({"ok": false, "error": if lang == "en" { en } else { ru }}))).into_response();
        let bad = StatusCode::BAD_REQUEST;
        if !truthy(fields.get("age_confirm")) {
            return Ok((error(bad,"Confirm that you are 18 or older","нужно подтвердить, что тебе есть 18"),false));
        }
        if fields.get("privacy_confirm") != Some(&Value::Bool(true)) {
            return Ok((error(bad,"Consent to personal data processing and the privacy policy is required","нужно согласие на обработку персональных данных и политику конфиденциальности"),false));
        }
        let name = string_field(&fields, "name").trim().to_owned();
        let email = string_field(&fields, "email").trim().to_lowercase();
        let password = string_field(&fields, "password");
        if !(2..=32).contains(&name.chars().count()) || name.contains('\0') {
            return Ok((error(bad,"Name must contain between 2 and 32 characters","имя: 2–32 символа"),false));
        }
        if !valid_email(&email) { return Ok((error(bad,"Enter a valid email address","нужна нормальная почта"),false)); }
        if password.chars().count() < 6 {
            return Ok((error(bad,"Password must have at least 6 characters","пароль минимум 6 символов"),false));
        }
        let hash = hash_password(password).await?;
        let native = matches!(header_text(headers, "x-app-platform"), Some("ios" | "android"));
        let managed = !native && self.managed_events && header_text(headers,"x-wiring-registration-tracking")==Some("server-v1");
        let consent = cookie(headers, "wiring-analytics-consent") == Some("granted") && !native;
        let client_id = cookie(headers, "_ym_uid").filter(|id| (1..=20).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_digit())).map(str::to_owned);
        let mut referral = string_field(&fields, "ref");
        if referral.is_empty() { referral = string_field(&fields, "referral"); }
        let input = NewUser {
            email: email.clone(), name, hash, locale: lang, now: ctx.stamp, verified: false,
            marketing: fields.get("marketing_consent") == Some(&Value::Bool(true)),
            channel: if native {"native_email"} else {"web_email"}.into(), managed, consent, client_id, referral,
        };
        let token = random_link()?;
        let stored_token = token.clone();
        let stamp = ctx.stamp;
        let registration = self.clone();
        let uid = ctx.db.transaction(move |tx| Box::pin(async move {
            let uid = registration.create(tx, input).await?;
            if let Some(uid) = uid {
                tx.execute("INSERT INTO email_verifications(token,user_id,created_at) VALUES($1,$2::bigint,$3::bigint)",
                           &[&stored_token,&uid,&stamp]).await?;
            }
            Ok(uid)
        })).await?.0;
        let Some(uid) = uid else {
            return Ok((error(StatusCode::CONFLICT,"This email is already registered","такая почта уже есть"),false));
        };
        self.deliver(&email, &token, lang, false).await;
        let mut result = json!({"ok":true,"needs_email_verify":true,"email":email});
        let mut response = if managed {
            result["registration_tracking"] = "server".into();
            Json(result).into_response()
        } else { Json(result).into_response() };
        if managed {
            let receipt = ctx.keys.sign_registration(uid, ctx.stamp as u64);
            let secure = if header_text(headers,"x-forwarded-proto") == Some("https") { "; Secure" } else { "" };
            let expires = httpdate::fmt_http_date(std::time::UNIX_EPOCH + Duration::from_secs(ctx.stamp as u64 + 43_200));
            let value = format!("wiring-registration-receipt={receipt}; Expires={expires}; Max-Age=43200{secure}; HttpOnly; Path=/api/analytics; SameSite=Lax");
            if let Ok(value) = HeaderValue::from_str(&value) { response.headers_mut().append(header::SET_COOKIE,value); }
        }
        Ok((response,true))
    }

    pub(crate) async fn send_link(&self, ctx: Context<'_>, fields: Value, reset: bool) -> Result<Response, StatusCode> {
        let email = string_field(&fields,"email").trim().to_lowercase();
        if valid_email(&email) {
            let token = random_link()?;
            let stored = token.clone();
            let email_key = email.clone();
            let now = ctx.stamp;
            let lang = ctx.db.transaction(move |tx| Box::pin(async move {
                let row = tx.query_opt("SELECT id::bigint,language,COALESCE(email_verified_at,0)!=0
                    FROM users WHERE email=$1 AND COALESCE(is_seed,0)=0 FOR UPDATE", &[&email_key]).await?;
                let Some(row) = row else { return Ok(None); };
                if !reset && (row.get::<_,bool>(2) || email_key == "demo@wiring.app") { return Ok(None); }
                let uid: i64 = row.get(0);
                if reset {
                    tx.execute("DELETE FROM password_resets WHERE user_id=$1::bigint",&[&uid]).await?;
                    tx.execute("INSERT INTO password_resets(token,user_id,created_at) VALUES($1,$2::bigint,$3::bigint)",&[&stored,&uid,&now]).await?;
                } else {
                    tx.execute("DELETE FROM email_verifications WHERE user_id=$1::bigint",&[&uid]).await?;
                    tx.execute("INSERT INTO email_verifications(token,user_id,created_at) VALUES($1,$2::bigint,$3::bigint)",&[&stored,&uid,&now]).await?;
                }
                Ok(Some(row.get::<_,String>(1)))
            })).await?.0;
            if let Some(lang) = lang { self.deliver(&email,&token,&lang,reset).await; }
        }
        Ok(Json(json!({"ok":true})).into_response())
    }

    pub(crate) async fn attribution(ctx: Context<'_>, headers: &HeaderMap, fields: Value) -> Result<Response, StatusCode> {
        let result = |ok| (if ok {StatusCode::OK} else {StatusCode::BAD_REQUEST},Json(json!({"ok":ok}))).into_response();
        let Some(uid) = cookie(headers,"wiring-registration-receipt")
            .and_then(|token|ctx.keys.read_registration(token,ctx.stamp as u64)).filter(|_|fields.is_object())
        else { return Ok(result(false)); };
        let consent = fields.get("consent") == Some(&Value::Bool(true));
        let client_id = fields.get("client_id").and_then(Value::as_str)
            .filter(|id|(1..=20).contains(&id.len()) && id.bytes().all(|b|b.is_ascii_digit())).map(str::to_owned);
        let now = ctx.stamp;
        let accepted=ctx.db.transaction(move |tx|Box::pin(async move {
            let row=tx.query_opt("SELECT delivery_state FROM registration_events
                WHERE user_id=$1::bigint AND mode='server' AND created_at >= $2::bigint FOR UPDATE",
                &[&uid,&(now-43_200)]).await?;
            let Some(row)=row else {return Ok(false);};
            if matches!(row.get::<_,&str>(0),"pending"|"needs_attribution"|"no_consent") {
                if !consent {
                    tx.execute("UPDATE registration_events SET client_id=NULL,consent_at=NULL,delivery_state='no_consent'
                        WHERE user_id=$1::bigint",&[&uid]).await?;
                } else if let Some(client_id)=client_id {
                    tx.execute("UPDATE registration_events SET client_id=$2,consent_at=$3::bigint,delivery_state='pending'
                        WHERE user_id=$1::bigint",&[&uid,&client_id,&now]).await?;
                }
            }
            Ok(true)
        })).await?.0;
        Ok(result(accepted))
    }

    async fn deliver(&self, email: &str, token: &str, lang: &str, reset: bool) {
        let Some(mail) = &self.mail else { return; };
        let lang = if lang == "en" {"en"} else {"ru"};
        let kind = if reset {"reset"} else {"verify"};
        let link = format!("{}/?{kind}={token}&lang={lang}",self.origin);
        let (subject,text) = match (lang,reset) {
            ("en", true) => ("WIRING — reset your password",format!("This link is valid for 2 hours:\n\n{link}\n\nIf you did not request this, ignore this email.")),
            ("en", false) => ("WIRING — verify your email",format!("To sign in to WIRING, verify your email using this link (valid for 48 hours):\n\n{link}\n\nIf you did not request this, ignore this email.")),
            (_,true) => ("WIRING — сброс пароля",format!("Ссылка действует 2 часа:\n\n{link}\n\nЕсли это не ты — просто проигнорируй письмо.")),
            (_,false) => ("WIRING — подтверди почту",format!("Чтобы войти в WIRING, подтверди почту по ссылке (действует 48 часов):\n\n{link}\n\nЕсли это не ты — просто проигнорируй письмо.")),
        };
        let result = mail.client.post("https://api.resend.com/emails").bearer_auth(&mail.key)
            .json(&json!({"from":mail.from,"to":email,"subject":subject,"text":text})).send().await;
        if !result.is_ok_and(|res| res.status().is_success()) {
            eprintln!("Account email delivery was not confirmed");
        }
    }
}

pub(crate) fn valid_email(email: &str) -> bool {
    let Some((local, domain)) = email.split_once('@') else { return false; };
    !local.is_empty() && !domain.contains('@') && domain.rsplit_once('.').is_some_and(|(left,right)| !left.is_empty() && !right.is_empty())
        && !email.chars().any(|c| c.is_whitespace() || c.is_control())
        && !email.contains("@wiring.guest") && !email.ends_with("@wiring.demo")
}

pub(crate) fn cookie<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get_all(header::COOKIE).iter().filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';')).find_map(|value| {
            let (key, value) = value.trim().split_once('=')?;
            (key == name).then_some(value)
        })
}

fn random_link() -> Result<String, StatusCode> {
    let mut seed = [0u8;32];
    getrandom::fill(&mut seed).map_err(|_|StatusCode::SERVICE_UNAVAILABLE)?;
    Ok(URL_SAFE_NO_PAD.encode(seed))
}

fn truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null | Value::Bool(false)) => false,
        Some(Value::String(value)) => !value.is_empty(),
        Some(Value::Number(value)) => value.as_f64() != Some(0.0),
        Some(Value::Array(value)) => !value.is_empty(),
        Some(Value::Object(value)) => !value.is_empty(),
        Some(Value::Bool(true)) => true,
    }
}
