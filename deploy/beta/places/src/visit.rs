use pbkdf2::sha2::{Digest, Sha256};
use tokio_postgres::Transaction;

use crate::chat_write::TransactionResult;

pub(crate) struct Visit {
    device: &'static str,
    os: &'static str,
    key: String,
}

impl Visit {
    pub(crate) fn new(ip: &str, user_agent: &str) -> Self {
        let ua: String = user_agent.chars().take(512).flat_map(char::to_lowercase).collect();
        let device = if ua.contains("ipad") || ua.contains("tablet") || (ua.contains("android") && !ua.contains("mobile")) {
            "tablet"
        } else if ["mobile", "iphone", "ipod", "android"].iter().any(|part| ua.contains(part)) {
            "mobile"
        } else { "desktop" };
        let os = if ["iphone", "ipad", "ipod"].iter().any(|part| ua.contains(part)) {
            "ios"
        } else if ua.contains("android") { "android" }
        else if ua.contains("windows") { "windows" }
        else if ua.contains("mac os") || ua.contains("macintosh") { "macos" }
        else if ua.contains("linux") { "linux" }
        else { "other" };
        let user_agent: String = user_agent.chars().take(160).collect();
        let key = Sha256::digest(format!("{ip}|{user_agent}").as_bytes())[..12]
            .iter().map(|b| format!("{b:02x}")).collect();
        Self { device, os, key }
    }
}

pub(crate) async fn track(tx: &Transaction<'_>, uid: Option<i64>, now: i64, visit: Visit) -> TransactionResult<bool> {
    if let Some(uid) = uid {
        if tx.query_opt("SELECT 1 FROM users WHERE id = $1::bigint", &[&uid]).await?.is_none() {
            return Ok(false);
        }
    }
    let key = if uid.is_some() { "" } else { &visit.key };
    let since = now - 1800;
    tx.execute(
        "INSERT INTO device_visits (user_id, visitor_key, device_class, os_name, created_at)
         SELECT $1::bigint, $2, $3, $4, $5::bigint WHERE NOT EXISTS (
             SELECT 1 FROM device_visits WHERE device_class = $3 AND created_at >= $6::bigint
             AND (($1::bigint IS NOT NULL AND user_id = $1) OR
                  ($1::bigint IS NULL AND user_id IS NULL AND visitor_key = $2))
         )",
        &[&uid, &key, &visit.device, &visit.os, &now, &since],
    ).await?;
    Ok(true)
}
