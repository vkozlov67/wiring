use std::{collections::HashSet, env, error::Error, fmt, time::{Duration, SystemTime, UNIX_EPOCH}};

use pbkdf2::sha2::{Digest, Sha256};
use serde_json::{Value, json};
use tokio::time::{Instant, sleep_until};
use tokio_postgres::{Client, NoTls};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum Failure { Transport, Http(u16), Contract }

impl fmt::Display for Failure {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport => write!(out,"SendPulse unavailable or invalid JSON response"),
            Self::Http(status) => write!(out,"SendPulse HTTP {status}"),
            Self::Contract => write!(out,"SendPulse returned an unexpected response"),
        }
    }
}
impl Error for Failure {}

struct SendPulse { http: reqwest::Client, key: String, book: i64, next: Instant }

impl SendPulse {
    async fn request(&mut self, path: &str, data: Option<Value>, missing: bool) -> Result<Option<Value>, Failure> {
        sleep_until(self.next).await;
        self.next = Instant::now() + Duration::from_millis(150);
        let url = format!("https://api.sendpulse.com/addressbooks/{}{path}",self.book);
        let request = if let Some(data) = data { self.http.post(url).json(&data) } else { self.http.get(url) };
        let mut response = request.bearer_auth(&self.key).header("Accept","application/json").send().await.map_err(|_|Failure::Transport)?;
        let status = response.status().as_u16();
        if status == 404 && missing { return Ok(None); }
        if !(200..300).contains(&status) && !(status == 400 && missing) { return Err(Failure::Http(status)); }
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_|Failure::Transport)? {
            if body.len() + chunk.len() > 64 * 1024 { return Err(Failure::Transport); }
            body.extend_from_slice(&chunk);
        }
        let parsed: Value = serde_json::from_slice(&body).map_err(|_| if status == 400 { Failure::Http(status) } else { Failure::Transport })?;
        if missing && ["17","303","502"].iter().any(|code| field_is(&parsed["error_code"],code)) { return Ok(None); }
        if status == 400 { return Err(Failure::Http(status)); }
        if parsed["result"] == false || parsed.get("error").is_some() || parsed.get("error_code").is_some() { return Err(Failure::Contract); }
        Ok(Some(parsed))
    }

    async fn validate(&mut self) -> Result<(), Failure> {
        let body = self.request("",None,false).await?.ok_or(Failure::Contract)?;
        let record = if let Some(items) = body.as_array().filter(|items| items.len()==1) { &items[0] } else { &body };
        if field_is(&record["id"],&self.book.to_string()) && field_is(&record["status"],"0") { Ok(()) } else { Err(Failure::Contract) }
    }

    async fn sync(&mut self, email: &str, name: &str) -> Result<bool, Failure> {
        let segment = encode_path_segment(email);
        if let Some(contact) = self.request(&format!("/emails/{segment}"),None,true).await? {
            // Subscribed, unsubscribed, blocked and deleted contacts are all left untouched.
            if contact["email"].as_str().is_some_and(|existing| existing.eq_ignore_ascii_case(email)) { return Ok(true); }
            return Err(Failure::Contract);
        }
        if self.request("/emails",Some(json!({"emails":[{"email":email,"variables":{"name":name}}]})),false).await?
            .is_some_and(|body| body["result"]==true) { Ok(false) } else { Err(Failure::Contract) }
    }
}

fn field_is(value: &Value, expected: &str) -> bool {
    value.as_str().is_some_and(|s|s==expected) || value.as_i64().is_some_and(|n|n.to_string()==expected)
}

fn encode_path_segment(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte,b'-'|b'_'|b'.'|b'~') { out.push(byte as char); }
        else { use std::fmt::Write; let _ = write!(out,"%{byte:02X}"); }
    }
    out
}

fn retry_delay(attempts: i32) -> i64 { (300 * 2_i64.pow(attempts.saturating_sub(1).clamp(0,4) as u32)).min(3600) }

pub(crate) async fn run() -> Result<(), Box<dyn Error>> {
    let key = env::var("SENDPULSE_API_KEY").unwrap_or_default().trim().to_owned();
    let book = env::var("SENDPULSE_ADDRESSBOOK_ID").unwrap_or_default();
    let book = book.trim();
    if key.is_empty() && book.is_empty() {
        println!("{{\"configured\":false}}");
        return Ok(());
    }
    if key.is_empty() || key.chars().any(char::is_whitespace) || book.is_empty() || !book.bytes().all(|b|b.is_ascii_digit()) {
        return Err("SendPulse credentials are incomplete".into());
    }
    let book: i64 = book.parse()?;
    if book <= 0 { return Err("SendPulse address book must be positive".into()); }
    let http = reqwest::Client::builder().https_only(true).no_proxy().redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(10)).build()?;
    let config = env::var("WIRING_SENDPULSE_DATABASE_URL")?;
    let mut config: tokio_postgres::Config = config.parse().map_err(|_| "Invalid SendPulse database config")?;
    config.connect_timeout(Duration::from_secs(4));
    let (db, connection) = config.connect(NoTls).await?;
    tokio::spawn(async move { let _ = connection.await; });
    db.batch_execute("SET statement_timeout='8s'; SET lock_timeout='4s'").await?;
    let locked: bool = db.query_one("SELECT pg_try_advisory_lock(739201,1)",&[]).await?.get(0);
    if !locked { println!("{{\"busy\":true}}"); return Ok(()); }
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() as i64;
    let mut sender = SendPulse { http,key,book,next:Instant::now() };
    let (added,existing,failed) = batch(&db,&mut sender,now).await?;
    println!("{}",json!({"added":added,"existing":existing,"failed":failed,"busy":false}));
    if failed != 0 { return Err("Some contacts were deferred for retry".into()); }
    Ok(())
}

async fn batch(db: &Client, sender: &mut SendPulse, now: i64) -> Result<(u64,u64,u64),Box<dyn Error>> {
    let book = sender.book;
    let rows = db.query("SELECT u.id::bigint,LOWER(TRIM(u.email)),COALESCE(s.attempts,0)
        FROM users u LEFT JOIN sendpulse_contacts s ON s.addressbook_id=$1::bigint
        AND s.email_key=encode(sha256(convert_to(LOWER(TRIM(u.email)),'UTF8')),'hex')
        WHERE u.deleted_at IS NULL AND COALESCE(u.is_seed,0)=0 AND u.marketing_consent_at>0 AND u.email_verified_at>0
        AND u.email LIKE '%@%' AND LOWER(u.email) NOT LIKE '%@wiring.guest' AND LOWER(u.email) NOT LIKE '%@wiring.demo'
        AND LOWER(u.email) NOT LIKE '%@wiring.test' AND LOWER(u.email) NOT LIKE '%@deleted.wiring'
        AND LOWER(u.email)<>'demo@wiring.app' AND s.synced_at IS NULL AND COALESCE(s.retry_at,0)<=$2::bigint
        ORDER BY u.id LIMIT 25", &[&book,&now]).await?;
    if !rows.is_empty() {
        sender.validate().await.map_err(|error| format!("SendPulse address book validation failed: {error}"))?;
    }
    let (mut added,mut existing,mut failed) = (0,0,0);
    let mut reported_failures = HashSet::new();
    for row in rows {
        let uid: i64 = row.get(0);
        let email: &str = row.get(1);
        let attempts = row.get::<_,i32>(2).saturating_add(1);
        let Some(eligible) = db.query_opt("SELECT name FROM users WHERE id=$1::bigint AND deleted_at IS NULL
            AND marketing_consent_at>0 AND email_verified_at>0 AND LOWER(TRIM(email))=$2 AND COALESCE(is_seed,0)=0", &[&uid,&email]).await? else { continue; };
        let name: &str = eligible.get(0);
        let email_key: String = Sha256::digest(email.as_bytes()).iter().map(|b|format!("{b:02x}")).collect();
        match sender.sync(email,name).await {
            Err(error @ Failure::Http(401|403)) => return Err(error.into()),
            Err(error) => {
                db.execute("INSERT INTO sendpulse_contacts(addressbook_id,email_key,user_id,attempts,retry_at)
                    VALUES ($1,$2,$3,$4,$5) ON CONFLICT (addressbook_id,email_key) DO UPDATE
                    SET attempts=excluded.attempts,retry_at=excluded.retry_at", &[&book,&email_key,&uid,&attempts,&(now+retry_delay(attempts))]).await?;
                if reported_failures.insert(error) {
                    eprintln!("SendPulse contact import deferred: {error}");
                }
                failed+=1;
            }
            Ok(was_existing) => {
                db.execute("INSERT INTO sendpulse_contacts(addressbook_id,email_key,user_id,synced_at)
                    VALUES ($1,$2,$3,$4) ON CONFLICT (addressbook_id,email_key) DO UPDATE
                    SET synced_at=excluded.synced_at,attempts=0,retry_at=0", &[&book,&email_key,&uid,&now]).await?;
                if was_existing { existing+=1; } else { added+=1; }
            }
        }
    }
    Ok((added,existing,failed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contact_url_is_one_encoded_path_segment() {
        assert_eq!(encode_path_segment("a/b+é@example.org"),"a%2Fb%2B%C3%A9%40example.org");
        assert_eq!((1..=6).map(retry_delay).collect::<Vec<_>>(),[300,600,1200,2400,3600,3600]);
    }
}
