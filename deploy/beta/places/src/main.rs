use std::{
    env,
    error::Error,
    fs::File,
    io::{BufRead, BufReader},
    path::Path,
    sync::Arc,
};

use axum::{
    Json, Router,
    body::Bytes,
    extract::State,
    http::{StatusCode, header},
    routing::{get, post},
};
use memchr::memmem::Finder;
use serde::{Deserialize, Serialize};

mod social;
mod chat_read;
mod chat_pair;
mod chat_write;
mod feed_write;
mod feed_delivery_http;
mod password;
mod mobile_session;
mod profile;
mod public_http;
mod signed_session;
mod mobile_http;
mod inbox_http;
mod message_edits;
mod spa_http;
mod visit;
mod service_http;
mod landing_http;
mod profile_read;
mod people_http;
mod places_http;
mod place_validation;
mod login;
mod account_tokens;
mod registration;
mod oauth_start;
mod oauth_identity;
mod oauth_callback;
mod account_settings;
mod albums_http;
mod premium_http;
mod notification_settings;
mod chat_history_http;
mod archive_http;
mod registration_worker;
mod sendpulse_worker;
mod purge_worker;
mod media_http;
mod demo_http;
mod support_http;
mod transcribe_http;
mod voice_upload;
mod photo_upload;
mod schema;
mod admin_actions;
mod task_triage;
mod admin_stats;
mod admin_page;
mod admin_reply;
mod browser_push;
mod photo_metadata;
mod discovery_http;
mod profile_edit;
mod message_send;
mod swipe_http;
mod recommendations_http;

const WORLD_LIMIT: usize = 40;
const RESPONSE_LIMIT: usize = 60;

type Country = (Place, String, String, String);
type City = (Place, String);
type WorldCity = (Place, String, String);

#[derive(Deserialize)]
struct Place(String, String, String, String);

#[derive(Serialize)]
struct Suggestion<'a> {
    value: &'a str,
    country: &'a str,
    label: &'a str,
}

struct Catalog {
    public: Bytes,
    countries: Vec<Country>,
    cities: Vec<City>,
    world: Vec<WorldCity>,
}

#[derive(Deserialize)]
struct Search {
    needle: String,
    normalized_city: String,
    country: String,
    include_countries: bool,
    english: bool,
}

impl Catalog {
    fn load(directory: &Path) -> Result<Self, Box<dyn Error>> {
        let (countries, cities) =
            serde_json::from_reader(BufReader::new(File::open(directory.join("curated.json"))?))?;
        let world = BufReader::new(File::open(directory.join("world.jsonl"))?)
            .lines()
            .map(|line| Ok(serde_json::from_str(&line?)?))
            .collect::<Result<_, Box<dyn Error>>>()?;
        Ok(Self {
            public: std::fs::read(directory.join("catalog.json"))?.into(),
            countries,
            cities,
            world,
        })
    }

    fn search(&self, query: &Search) -> Vec<Suggestion<'_>> {
        let needle = query.needle.as_str();
        let country = query.country.as_str();
        let mut results = Vec::new();
        if query.include_countries {
            results.extend(
                self.countries
                    .iter()
                    .filter(|(_, aliases, russian, english)| {
                        let label = if query.english { english } else { russian };
                        aliases.contains(needle) || label.contains(needle)
                    })
                    .map(|(place, _, _, _)| place),
            );
        }
        results.extend(
            self.cities
                .iter()
                .filter(|(place, key)| {
                    (country.is_empty() || place.1 == country)
                        && (key.contains(needle) || place.0 == query.normalized_city)
                })
                .map(|(place, _)| place),
        );
        if needle.chars().count() >= 2 || !country.is_empty() {
            results.extend(self.search_world(needle, country));
        }
        let mut seen = std::collections::HashSet::new();
        results
            .into_iter()
            .filter(|place| seen.insert(place.0.as_str()))
            .take(RESPONSE_LIMIT)
            .map(|place| Suggestion {
                value: &place.0,
                country: &place.1,
                label: if query.english { &place.3 } else { &place.2 },
            })
            .collect()
    }

    fn search_world(&self, needle: &str, country: &str) -> impl Iterator<Item = &Place> {
        let contains = Finder::new(needle);
        let exact_pattern = format!("|{needle}|");
        let exact_alias = Finder::new(&exact_pattern);
        let mut exact = Vec::new();
        let mut prefix = Vec::new();
        let mut rest = Vec::new();

        for (place, name, aliases) in &self.world {
            if (!country.is_empty() && place.1 != country)
                || contains.find(aliases.as_bytes()).is_none()
            {
                continue;
            }
            if name == needle || exact_alias.find(aliases.as_bytes()).is_some() {
                exact.push(place);
                if exact.len() == WORLD_LIMIT {
                    break;
                }
            } else if name.starts_with(needle) {
                if prefix.len() < WORLD_LIMIT {
                    prefix.push(place);
                }
            } else if rest.len() < WORLD_LIMIT {
                rest.push(place);
            }
            if needle.is_empty() && prefix.len() + rest.len() >= WORLD_LIMIT {
                break;
            }
        }
        exact
            .into_iter()
            .chain(prefix)
            .chain(rest)
            .take(WORLD_LIMIT)
    }
}

async fn public_catalog(State(catalog): State<Arc<Catalog>>) -> ([(header::HeaderName, &'static str); 1], Bytes) {
    ([(header::CONTENT_TYPE, "application/json")], catalog.public.clone())
}

async fn search(
    State(catalog): State<Arc<Catalog>>,
    Json(query): Json<Search>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    tokio::task::spawn_blocking(move || Json(serde_json::json!(catalog.search(&query))))
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    if env::args().nth(1).as_deref()==Some("init-schema") {
        return schema::install().await;
    }
    if env::args().nth(1).as_deref() == Some("sync-registrations") {
        return registration_worker::run().await;
    }
    if env::args().nth(1).as_deref() == Some("sync-sendpulse") {
        return sendpulse_worker::run().await;
    }
    if env::args().nth(1).as_deref() == Some("purge-accounts") {
        return purge_worker::run(env::args().nth(2).as_deref() != Some("--apply")).await;
    }
    let directory = env::var("WIRING_PLACES_CATALOG").unwrap_or_else(|_| "/catalog".into());
    let catalog = Arc::new(Catalog::load(Path::new(&directory))?);
    let writer = chat_write::Writer::from_env()?;
    let app = Router::new()
        .route("/search", post(search))
        .route("/catalog", get(public_catalog))
        .with_state(catalog.clone());
    let app = app
        .route("/feed/filter", post(social::feed))
        .route("/chat/thread", post(social::thread))
        .merge(chat_write::routes(writer.clone()));
    let address = env::var("WIRING_PLACES_BIND").unwrap_or_else(|_| "127.0.0.1:5081".into());
    let listener = tokio::net::TcpListener::bind(address).await?;
    if let Ok(address) = env::var("WIRING_PUBLIC_BIND") {
        let public_directory = env::var("WIRING_PUBLIC_DOCUMENTS").unwrap_or_else(|_| "/documents".into());
        let public = public_http::router(Path::new(&public_directory))?
            .merge(mobile_http::routes(writer.clone())?)
            .merge(inbox_http::routes(writer.clone())?)
            .merge(spa_http::routes(writer.clone(), Path::new(&public_directory))?)
            .merge(landing_http::routes(writer.clone(), Path::new(&public_directory))?)
            .merge(oauth_start::routes(writer.clone())?)
            .merge(admin_actions::routes(writer.clone())?)
            .merge(admin_page::routes(writer.clone(),Path::new(&public_directory))?)
            .merge(service_http::routes(writer, catalog, Path::new(&public_directory))?);
        let public_listener = tokio::net::TcpListener::bind(address).await?;
        tokio::try_join!(async { axum::serve(listener, app).await },
                         async { axum::serve(public_listener, public).await })?;
    } else {
        axum::serve(listener, app).await?;
    }
    Ok(())
}
