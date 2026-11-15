use std::{collections::HashMap, sync::Arc, time::{Duration, Instant}};

use axum::{Json, http::StatusCode, response::{IntoResponse, Response}};
use serde_json::{Value, json};
use unicode_normalization::{UnicodeNormalization, char::is_combining_mark};

use crate::{Catalog, Search, chat_write::Writer, login::LoginLimit};

pub(crate) struct PublicPlaces {
    catalog: Arc<Catalog>,
    legacy: HashMap<String, String>,
    countries: Arc<CountryIndex>,
    limit: LoginLimit,
    slots: Arc<tokio::sync::Semaphore>,
}

impl PublicPlaces {
    pub(crate) fn new(catalog: Arc<Catalog>) -> Result<Self, serde_json::Error> {
        let info: Value = serde_json::from_slice(&catalog.public)?;
        let mut legacy = HashMap::new();
        if let Some(keys) = info["city_search"].as_object() {
            for (city, aliases) in keys {
                if let Some(aliases) = aliases.as_array() {
                    for alias in aliases.iter().filter_map(Value::as_str) {
                        legacy.entry(alias.to_owned()).or_insert_with(|| city.to_owned());
                    }
                }
            }
        }
        let mut countries = CountryIndex { exact: HashMap::new(), names: catalog.countries.iter().map(|(p,_,_,_)|p.1.clone()).collect() };
        for (place,_) in &catalog.cities { countries.exact.insert(place.0.clone(),place.1.clone()); }
        Ok(Self { catalog, legacy, countries:Arc::new(countries), limit: LoginLimit::configured(Duration::from_secs(60), 180),
            slots: Arc::new(tokio::sync::Semaphore::new(6)) })
    }

    pub(crate) async fn search(&self, db: &Writer, uid: i64, query: &str, now: i64, lang: &str) -> Result<Response, StatusCode> {
        let result = db.transaction(move |tx| Box::pin(async move {
            let row = tx.query_opt("SELECT COALESCE(report_banned_at,0)!=0 FROM users
                WHERE id=$1::bigint AND COALESCE(deleted_at,0)=0", &[&uid]).await?;
            let Some(row) = row else { return Ok(None); };
            let banned: bool = row.get(0);
            if !banned {
                tx.execute("UPDATE users SET last_seen=$2::bigint WHERE id=$1::bigint", &[&uid,&now]).await?;
            }
            Ok(Some(banned))
        })).await?.0;
        match result {
            None => return Ok(crate::account_settings::signed_out(lang)),
            Some(true) => return Ok(crate::account_settings::report_ban_response(lang)),
            Some(false) => (),
        }
        if !self.limit.allow(&uid.to_string(), Instant::now()) {
            return Ok((StatusCode::TOO_MANY_REQUESTS, Json(json!({"ok":false,"error":if lang == "en" {
                "Wait a little before checking another city"
            } else { "Подожди немного перед следующей проверкой города" }}))).into_response());
        }
        let mut input = HashMap::new();
        for (key, value) in form_urlencoded::parse(query.as_bytes()) {
            if matches!(key.as_ref(), "q" | "country" | "countries") { input.entry(key).or_insert(value); }
        }
        let raw: String = input.get("q").map_or("", |s| s.as_ref()).trim().chars().take(120).collect();
        let country = input.get("country").map_or("", |s| s.as_ref()).trim().chars().take(100).collect();
        let normalized = legacy_key(&raw);
        let normalized_city = self.legacy.get(&normalized).or_else(|| self.legacy.get(&normalized.replace(' ', "")))
            .cloned().unwrap_or_default();
        let query = Search { needle: search_key(&raw), normalized_city, country,
            include_countries: input.get("countries").is_some_and(|s| s == "1"), english: lang == "en" };
        let slot = Arc::clone(&self.slots).try_acquire_owned().map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
        let catalog = Arc::clone(&self.catalog);
        let result = tokio::task::spawn_blocking(move || {
            let _slot = slot;
            Json(json!({"ok":true,"places":catalog.search(&query)})).into_response()
        }).await.map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
        Ok(result)
    }

    pub(crate) fn known(&self, raw: &str) -> Option<(String, String)> {
        let raw = raw.trim();
        if let Some((place,_)) = self.catalog.cities.iter().find(|(place,_)|place.0==raw) {
            return Some((place.0.clone(),place.1.clone()));
        }
        let legacy = legacy_key(raw);
        if let Some(name) = self.legacy.get(&legacy).or_else(||self.legacy.get(&legacy.replace(' ',""))) {
            if let Some((place,_)) = self.catalog.cities.iter().find(|(place,_)|&place.0==name) {
                return Some((place.0.clone(),place.1.clone()));
            }
        }
        if raw.contains(" · ") {
            return self.catalog.world.iter().find(|(place,_,_)|place.0==raw).map(|(place,_,_)|(place.0.clone(),place.1.clone()));
        }
        let needle = search_key(raw);
        if needle.chars().count()<2 { return None; }
        let pattern = format!("|{needle}|");
        let finder = memchr::memmem::Finder::new(pattern.as_bytes());
        self.catalog.world.iter().find(|(_,_,aliases)|finder.find(aliases.as_bytes()).is_some())
            .map(|(place,_,_)|(place.0.clone(),place.1.clone()))
    }

    pub(crate) fn country_known(&self, country:&str) -> bool {
        self.catalog.countries.iter().any(|(place,_,_,_)|place.1==country)
    }

    pub(crate) fn country_index(&self) -> Arc<CountryIndex> { Arc::clone(&self.countries) }

    pub(crate) fn qualified_country<'a>(&self, name:&'a str) -> Option<&'a str> {
        let (city,country) = name.rsplit_once(" · ")?;
        ((2..=200).contains(&city.chars().count()) && city.chars().any(char::is_alphabetic)
            && !city.chars().any(|c|c<='\u{1f}' || matches!(c,'<'|'>'|'\u{7f}')) && self.country_known(country)).then_some(country)
    }
}

pub(crate) struct CountryIndex { exact:HashMap<String,String>, names:std::collections::HashSet<String> }

impl CountryIndex {
    pub(crate) fn country<'a>(&'a self, name:&'a str) -> &'a str {
        if let Some(country)=self.exact.get(name) { return country; }
        name.rsplit_once(" · ").map(|(_,country)|country).filter(|country|self.names.contains(*country)).unwrap_or_default()
    }
}

fn legacy_key(raw: &str) -> String {
    raw.chars().flat_map(char::to_lowercase).filter_map(|c| match c {
        '\'' | '`' | 'ʼ' | '’' => None,
        'ё' => Some('е'),
        '-' | '–' | '—' | '_' => Some(' '),
        _ => Some(c),
    }).collect::<String>().split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(crate) fn search_key(raw: &str) -> String {
    raw.to_lowercase().replace('ё', "е").replace('ß', "ss").replace('ς', "σ")
        .nfkd().filter(|c| !is_combining_mark(*c)).map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>().split_whitespace().collect::<Vec<_>>().join(" ")
}
