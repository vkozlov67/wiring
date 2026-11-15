use std::{collections::{HashMap, HashSet}, env, error::Error, fs, path::Path, sync::Arc};

use serde::Deserialize;
use serde_json::{Value, json};
use tokio_postgres::Transaction;
use unicode_normalization::{UnicodeNormalization, char::is_combining_mark};

use crate::{Catalog, chat_read, chat_write::TransactionResult, inbox_http::notice_body};

#[derive(Deserialize)]
struct Rules {
    cities: HashSet<String>,
    city_keys: HashSet<String>,
    genders: HashSet<String>,
    intents: HashSet<String>,
    neuro: HashSet<String>,
    vibe: HashSet<String>,
    retired_vibe: HashSet<String>,
}

pub(crate) struct ProfileRules {
    rules: Rules,
    catalog: Arc<Catalog>,
    origin: String,
    jev_configured: bool,
}

impl ProfileRules {
    pub(crate) fn tags(&self, mut neuro: Vec<String>, raw_vibe: Vec<String>) -> (Vec<String>,Vec<String>) {
        neuro.retain(|tag| self.rules.neuro.contains(tag));
        let mut vibe = Vec::new();
        for tag in raw_vibe {
            if self.rules.retired_vibe.contains(&tag) { continue; }
            if self.rules.vibe.contains(&tag) {
                if !vibe.contains(&tag) { vibe.push(tag); }
            } else if self.rules.neuro.contains(&tag) && !neuro.contains(&tag) { neuro.push(tag); }
        }
        if neuro.iter().any(|tag|tag=="audhd") { neuro.retain(|tag|tag!="asd" && tag!="adhd"); }
        (neuro,vibe)
    }

    pub(crate) fn filter_keys(&self, kind: &str) -> &HashSet<String> {
        match kind { "neuro" => &self.rules.neuro, "vibe" => &self.rules.vibe, _ => &self.rules.intents }
    }

    pub(crate) fn gender_ok(&self, value:&str) -> bool { self.rules.genders.contains(value) }

    pub(crate) fn load(directory: &Path, catalog: Arc<Catalog>) -> Result<Self, Box<dyn Error>> {
        Ok(Self {
            rules: serde_json::from_slice(&fs::read(directory.join("profile-rules.json"))?)?,
            catalog,
            origin: env::var("SITE_URL").unwrap_or_else(|_| "https://wiring.date".into()).trim_end_matches('/').to_owned(),
            jev_configured: env::var("WIRING_JEV_CONFIGURED").as_deref() == Ok("1"),
        })
    }

    pub(crate) fn city_ok(&self, city: &str) -> bool {
        let city = city.trim();
        if city.is_empty() { return false; }
        if self.rules.cities.contains(city) { return true; }
        let normalized: String = city.chars().flat_map(char::to_lowercase).filter_map(|c| match c {
            '\'' | '`' | 'ʼ' | '’' => None,
            'ё' => Some('е'),
            '-' | '–' | '—' | '_' => Some(' '),
            _ => Some(c),
        }).collect();
        let legacy = normalized.split_whitespace().collect::<Vec<_>>().join(" ");
        if self.rules.city_keys.contains(&legacy) || self.rules.city_keys.contains(&legacy.replace(' ', "")) { return true; }
        if let Some((name, country)) = city.rsplit_once(" · ") {
            if (2..=200).contains(&name.chars().count()) && name.chars().any(char::is_alphabetic)
                && !name.chars().any(|c| c <= '\u{1f}' || matches!(c, '<' | '>' | '\u{7f}'))
                && self.catalog.countries.iter().any(|entry| entry.0.1 == country)
            { return true; }
        }
        let raw = city.to_lowercase().replace('ё', "е").replace('ß', "ss").replace('ς', "σ");
        let cleaned: String = raw.nfkd().filter(|c| !is_combining_mark(*c))
            .map(|c| if c.is_alphanumeric() { c } else { ' ' }).collect();
        let key = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
        if key.chars().count() < 2 { return false; }
        let key = format!("|{key}|");
        let finder = memchr::memmem::Finder::new(key.as_bytes());
        self.catalog.world.iter().any(|(_, _, aliases)| finder.find(aliases.as_bytes()).is_some())
    }

    pub(crate) async fn read(&self, tx: &Transaction<'_>, uid: i64, now: i64, lang: &str) -> TransactionResult<(Value, bool)> {
        self.serialize(tx, uid, now, lang, ProfileView::Current).await
    }

    pub(crate) async fn person(&self, tx: &Transaction<'_>, uid: i64, now: i64, lang: &str, own: bool) -> TransactionResult<Value> {
        Ok(self.serialize(tx, uid, now, lang, if own { ProfileView::Own } else { ProfileView::Detail }).await?.0)
    }

    pub(crate) async fn card(&self, tx: &Transaction<'_>, uid: i64, now: i64, lang: &str) -> TransactionResult<Value> {
        Ok(self.serialize(tx, uid, now, lang, ProfileView::Card).await?.0)
    }

    async fn serialize(&self, tx: &Transaction<'_>, uid: i64, now: i64, lang: &str, view: ProfileView) -> TransactionResult<(Value, bool)> {
        let row = tx.query_opt(
            "SELECT jsonb_build_object(
                'name', name, 'age', age, 'city', city, 'gender', gender, 'looking_for', looking_for,
                'bio', bio, 'photo', photo, 'job', job, 'intent', intent, 'height', height,
                'communication', communication, 'last_seen', last_seen, 'is_seed', is_seed,
                'deleted_at', deleted_at, 'name_changed_at', name_changed_at, 'theme', theme,
                'language', language, 'email', email, 'profile_setup_required', profile_setup_required,
                'special_data_consent_at', special_data_consent_at, 'premium_until', premium_until,
                'incognito', incognito, 'paused', paused, 'notify_enabled', notify_enabled,
                'notify_push', notify_push, 'referral_code', referral_code, 'seek_min_age', seek_min_age,
                'seek_max_age', seek_max_age, 'seek_place', seek_place, 'hide_tags', hide_tags,
                'report_banned_at', report_banned_at, 'jev_feed_enabled', jev_feed_enabled,
                'jev_recommendations_unlocked', jev_recommendations_unlocked)::text
             FROM users WHERE id = $1::bigint", &[&uid],
        ).await?;
        let Some(row) = row else { return Ok((Value::Null, true)); };
        let fields: Value = serde_json::from_str(row.get(0))?;
        let integer = |field: &str| fields[field].as_i64().unwrap_or(0);
        let text = |field: &str| fields[field].as_str().unwrap_or("");
        if integer("deleted_at") != 0 { return Ok((Value::Null, false)); }
        let email = text("email");
        let guest = email.ends_with("@wiring.guest") || email == "demo@wiring.app";
        let seed = integer("is_seed") != 0;
        let ref_code = if !guest && !seed {
            if view == ProfileView::Current { referral(tx, uid, text("referral_code")).await? }
            else if view == ProfileView::Own { text("referral_code").to_owned() } else { String::new() }
        } else { String::new() };

        let mut neuro = Vec::new();
        let mut old_vibes = Vec::new();
        for row in tx.query("SELECT kind, tag FROM user_tags WHERE user_id = $1::bigint ORDER BY tag", &[&uid]).await? {
            let tag: String = row.get(1);
            match row.get::<_, &str>(0) {
                "neuro" if self.rules.neuro.contains(&tag) && !neuro.contains(&tag) => neuro.push(tag),
                "vibe" => old_vibes.push(tag),
                _ => {},
            }
        }
        let mut vibes = Vec::new();
        for tag in old_vibes {
            if self.rules.retired_vibe.contains(&tag) { continue; }
            if self.rules.vibe.contains(&tag) {
                if !vibes.contains(&tag) { vibes.push(tag); }
            } else if self.rules.neuro.contains(&tag) && !neuro.contains(&tag) { neuro.push(tag); }
        }
        if neuro.iter().any(|tag| tag == "audhd") { neuro.retain(|tag| tag != "asd" && tag != "adhd"); }
        let mut hidden = Vec::new();
        for tag in text("hide_tags").split(',') {
            if self.rules.vibe.contains(tag) && !self.rules.retired_vibe.contains(tag) && !hidden.contains(&tag) {
                hidden.push(tag);
            }
        }
        let raw_intents = text("intent").trim();
        let json_intents = if raw_intents.starts_with('[') { serde_json::from_str::<Value>(raw_intents).ok() } else { None };
        let input: Vec<&str> = match &json_intents {
            Some(Value::Array(values)) => values.iter().filter_map(Value::as_str).collect(),
            Some(_) => vec![raw_intents],
            None => raw_intents.split([',', ';', '|']).collect(),
        };
        let mut intents = Vec::new();
        for raw in input {
            let value = raw.trim();
            if self.rules.intents.contains(value) && !intents.contains(&value) { intents.push(value); }
        }
        if intents.is_empty() { intents.push("dating"); }
        let prompts: Vec<Value> = tx.query(
            "SELECT prompt_id, answer FROM user_prompts WHERE user_id = $1::bigint ORDER BY sort_order, prompt_id",
            &[&uid],
        ).await?.into_iter().map(|row| json!({"id":row.get::<_, &str>(0), "answer":row.get::<_, &str>(1)})).collect();
        let photos = profile_photos(tx, uid).await?;
        let primary = photos.iter().find(|photo| photo["is_primary"] == true).or_else(|| photos.first())
            .and_then(|photo| photo["url"].as_str()).map(str::to_owned).unwrap_or_else(|| media_url(text("photo")));
        let albums = if view == ProfileView::Card { Vec::new() } else { profile_albums(tx, uid, &photos).await? };
        let display_name = text("name").trim();
        let unnamed = matches!(display_name.to_lowercase().as_str(), "участник" | "participant");
        let setup = integer("profile_setup_required") != 0 || unnamed || display_name.is_empty();
        let has_photo = !photos.is_empty() || !text("photo").trim().is_empty();
        let age = integer("age");
        let needs_profile = !(seed || guest || !setup && (2..=32).contains(&display_name.chars().count())
            && (18..=99).contains(&age) && self.rules.genders.contains(text("gender")) && has_photo
            && (neuro.is_empty() || integer("special_data_consent_at") != 0));
        let last_seen = integer("last_seen");
        let premium = integer("premium_until");
        let jev = !guest && !seed && integer("jev_recommendations_unlocked") != 0;
        let banned = integer("report_banned_at") != 0;
        let name_changed = integer("name_changed_at");
        let mut payload = json!({
            "id": uid, "name": if unnamed { "" } else { text("name") },
            "age": if unnamed || age == 0 { None } else { Some(age) }, "city": fields["city"], "gender": fields["gender"],
            "looking_for": fields["looking_for"], "bio": fields["bio"], "job": fields["job"], "intent": intents[0],
            "intents": intents, "height": fields["height"], "communication": fields["communication"],
            "photo": primary, "photos": photos, "neuro": neuro, "vibe": vibes, "prompts": prompts,
            "online": last_seen != 0 && now - last_seen < 600, "city_ok": self.city_ok(text("city")), "has_photo": has_photo,
            "seek_min_age": nonzero(integer("seek_min_age"), 18), "seek_max_age": nonzero(integer("seek_max_age"), 99),
            "seek_place": text("seek_place"), "hide_tags": hidden, "albums": albums,
        });
        // The projection is selected by the handler after checking that the target is visible.
        // Peer profiles never receive contact details, settings, or inbox data.
        if matches!(view, ProfileView::Card | ProfileView::Detail) {
            if view == ProfileView::Card {
                payload.as_object_mut().expect("profile is an object").remove("albums");
                payload["photos"] = Value::Array(photos.iter().map(|photo| photo["url"].clone()).collect());
            }
            return Ok((payload, false));
        }
        let account = json!({
            "name_change_available_at": if name_changed == 0 { 0 } else { name_changed + 14*24*60*60 },
            "theme": fields["theme"], "language": fields["language"], "email": email, "guest": guest,
            "needs_profile": needs_profile, "needs_onboard": needs_profile, "needs_profile_setup": setup, "needs_city": false,
            "needs_special_consent": integer("special_data_consent_at") == 0, "needs_photo_consent": false,
            "plus": premium > now, "plus_until": premium, "incognito": integer("incognito") != 0, "paused": integer("paused") != 0,
            "report_banned": banned, "notify_enabled": integer("notify_enabled") != 0, "notify_push": integer("notify_push") != 0,
            "jev_feed_unlocked": jev, "jev_feed_beta": jev, "jev_feed_available": jev && self.jev_configured,
            "jev_feed_enabled": jev && integer("jev_feed_enabled") != 0, "feed_qa": email.trim().eq_ignore_ascii_case("wiringremlin@gmail.com"),
        });
        if let Value::Object(account) = account {
            payload.as_object_mut().expect("profile is an object").extend(account);
        }
        if banned {
            payload["report_ban_message"] = json!(if lang == "en" {
                "Your account was hidden after reports from two different members. If you think this is a mistake, contact support for a manual review."
            } else { "Аккаунт скрыт после жалоб от двух разных участников. Если считаешь, что это ошибка, напиши в поддержку — разберём вручную." });
        }
        if view == ProfileView::Current {
            let counts = chat_read::inbox_counts(tx, uid).await?;
            let mut notices = chat_read::unread_notices(tx, uid, 8).await?;
            for notice in &mut notices { notice.body = notice_body(&notice.kind, &notice.body, lang == "en"); }
            let likes = if guest {
                tx.query_one("SELECT count(*) FROM swipes WHERE from_id = $1::bigint AND direction = 'like'", &[&uid]).await?.get::<_, i64>(0)
            } else { 0 };
            payload["likes_in"] = json!(counts.likes_in);
            payload["unread"] = json!(counts.unread);
            payload["unread_chats"] = json!(counts.unread_chats);
            payload["guest_nudge"] = json!(guest && likes >= 3);
            payload["notices"] = json!(notices);
        }
        if !ref_code.is_empty() {
            let count: i64 = tx.query_one("SELECT count(*) FROM referrals WHERE referrer_id = $1::bigint", &[&uid]).await?.get(0);
            payload["ref_url"] = json!(format!("{}/r/{ref_code}", self.origin));
            payload["ref"] = json!(ref_code);
            payload["ref_count"] = json!(count);
            payload["ref_days"] = json!(30);
        }
        Ok((payload, false))
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ProfileView { Current, Own, Detail, Card }

pub(crate) async fn profile_photos(tx: &Transaction<'_>, uid: i64) -> TransactionResult<Vec<Value>> {
    let rows = tx.query(
        "SELECT p.id::bigint, p.path, p.is_primary != 0, p.album_id::bigint, a.title FROM photos p
         LEFT JOIN albums a ON a.id = p.album_id WHERE p.user_id = $1::bigint
         ORDER BY p.is_primary DESC, p.sort_order, p.id", &[&uid],
    ).await?;
    Ok(rows.iter().map(|row| json!({
        "id": row.get::<_, i64>(0), "url": media_url(row.get(1)), "is_primary": row.get::<_, bool>(2),
        "album_id": row.get::<_, Option<i64>>(3), "album": row.get::<_, Option<&str>>(4).filter(|s| !s.is_empty()).unwrap_or("я"),
    })).collect())
}

pub(crate) async fn profile_albums(tx: &Transaction<'_>, uid: i64, photos: &[Value]) -> TransactionResult<Vec<Value>> {
    let mut albums: Vec<Value> = tx.query(
        "SELECT id::bigint, title FROM albums WHERE user_id = $1::bigint ORDER BY sort_order, id", &[&uid],
    ).await?.into_iter().map(|row| json!({"id": row.get::<_,i64>(0), "title": row.get::<_,&str>(1), "photos": []})).collect();
    let mut album_index: HashMap<i64, usize> = albums.iter().enumerate()
        .map(|(idx, album)| (album["id"].as_i64().unwrap(), idx)).collect();
    for photo in photos {
        let id = photo["album_id"].as_i64().unwrap_or(0);
        let idx = *album_index.entry(id).or_insert_with(|| {
            let idx = albums.len();
            albums.push(json!({"id": id, "title": photo["album"], "photos": []}));
            idx
        });
        albums[idx]["photos"].as_array_mut().unwrap().push(photo.clone());
    }
    Ok(albums)
}

pub(crate) fn media_url(path: &str) -> String {
    if path.is_empty() { return String::new(); }
    let base = if path.starts_with("portraits/") || path.starts_with("people/") { "/public/" } else { "/media/" };
    format!("{base}{path}")
}

fn nonzero(n: i64, default: i64) -> i64 {
    if n == 0 { default } else { n }
}

pub(crate) async fn referral(tx: &Transaction<'_>, uid: i64, existing: &str) -> TransactionResult<String> {
    if !existing.is_empty() { return Ok(existing.to_owned()); }
    for _ in 0..8 {
        let mut bytes = [0u8; 4];
        if getrandom::fill(&mut bytes).is_err() { return Err("Referral entropy unavailable".into()); }
        let code: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        tx.batch_execute("SAVEPOINT make_referral").await?;
        let update = tx.execute(
            "UPDATE users SET referral_code = $2 WHERE id = $1::bigint AND COALESCE(deleted_at, 0) = 0
             AND (referral_code IS NULL OR referral_code = '')", &[&uid, &code],
        ).await;
        if let Err(error) = update {
            tx.batch_execute("ROLLBACK TO SAVEPOINT make_referral; RELEASE SAVEPOINT make_referral").await?;
            if error.code() == Some(&tokio_postgres::error::SqlState::UNIQUE_VIOLATION) { continue; }
            return Err(error.into());
        }
        tx.batch_execute("RELEASE SAVEPOINT make_referral").await?;
        return Ok(tx.query_one("SELECT referral_code FROM users WHERE id = $1::bigint", &[&uid]).await?
            .get::<_, Option<String>>(0).unwrap_or_default());
    }
    Ok(String::new())
}
