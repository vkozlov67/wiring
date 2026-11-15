use std::sync::Arc;

use axum::{Json, extract::State, http::{HeaderMap, StatusCode}};
use serde::{Deserialize, Serialize};
use tokio_postgres::Transaction;

use crate::chat_write::{TransactionResult, Writer};

#[derive(Deserialize)]
pub(crate) struct SettingsRequest {
    uid: i64,
    #[serde(flatten)]
    change: Setting,
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
enum Setting {
    Language { value: Language },
    Theme { value: Theme },
    Consents { special_at: Option<i64>, update_photo: bool, photo_at: Option<i64> },
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Language { Ru, En }

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Theme { Pastel, Mist, Dusk, Night, Slate }

#[derive(Serialize)]
pub(crate) struct Updated {
    updated: bool,
}

pub(crate) async fn settings(
    State(state): State<Arc<Writer>>,
    headers: HeaderMap,
    Json(input): Json<SettingsRequest>,
) -> Result<Json<Updated>, StatusCode> {
    state.authorize(&headers)?;
    state.transaction(move |tx| Box::pin(async move { save_settings(tx, input).await })).await
}

async fn save_settings(tx: &Transaction<'_>, input: SettingsRequest) -> TransactionResult<Updated> {
    let rows = match input.change {
        Setting::Language { value } => {
            let value = match value { Language::Ru => "ru", Language::En => "en" };
            tx.execute("UPDATE users SET language = $2 WHERE id = $1::bigint", &[&input.uid, &value]).await?
        }
        Setting::Theme { value } => {
            let value = match value {
                Theme::Pastel => "pastel", Theme::Mist => "mist", Theme::Dusk => "dusk",
                Theme::Night => "night", Theme::Slate => "slate",
            };
            tx.execute("UPDATE users SET theme = $2 WHERE id = $1::bigint", &[&input.uid, &value]).await?
        }
        Setting::Consents { special_at, update_photo, photo_at } => tx.execute(
            "UPDATE users SET special_data_consent_at = $2::bigint,
             photo_rights_consent_at = CASE WHEN $3 THEN $4::bigint ELSE photo_rights_consent_at END
             WHERE id = $1::bigint",
            &[&input.uid, &special_at, &update_photo, &photo_at],
        ).await?,
    };
    Ok(Updated { updated: rows == 1 })
}
