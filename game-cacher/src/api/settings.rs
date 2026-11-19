use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use serde::{Deserialize, Serialize};

use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

#[derive(Serialize, Deserialize)]
pub struct SettingsBody {
    update_interval_minutes: u32,
}

pub async fn get_settings(State(state): State<Arc<AppState>>) -> Json<SettingsBody> {
    Json(SettingsBody {
        update_interval_minutes: state.settings.get().await.update_interval_minutes,
    })
}

pub async fn put_settings(
    State(state): State<Arc<AppState>>,
    Json(body): Json<SettingsBody>,
) -> ApiResult<Json<SettingsBody>> {
    if !(5..=7 * 24 * 60).contains(&body.update_interval_minutes) {
        return Err(ApiError::bad_request(
            "update_interval_minutes must be between 5 and 10080",
        ));
    }
    let s = state
        .settings
        .update(|s| s.update_interval_minutes = body.update_interval_minutes)
        .await?;
    Ok(Json(SettingsBody {
        update_interval_minutes: s.update_interval_minutes,
    }))
}
