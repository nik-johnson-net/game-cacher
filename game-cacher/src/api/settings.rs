use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::error::{ApiError, ApiResult};
use crate::schedule::Schedule;
use crate::state::AppState;

#[derive(Serialize, Deserialize)]
pub struct SettingsBody {
    update_schedule: ScheduleBody,
}

/// Raw strings so validation errors become 400s with a readable message.
#[derive(Serialize, Deserialize)]
struct ScheduleBody {
    timezone: String,
    times: Vec<String>,
}

impl From<&Schedule> for SettingsBody {
    fn from(s: &Schedule) -> Self {
        Self {
            update_schedule: ScheduleBody {
                timezone: s.timezone.name().to_string(),
                times: s.times.iter().map(|t| t.to_string()).collect(),
            },
        }
    }
}

pub async fn get_settings(State(state): State<Arc<AppState>>) -> Json<SettingsBody> {
    Json((&state.settings.get().await.update_schedule).into())
}

pub async fn put_settings(
    State(state): State<Arc<AppState>>,
    Json(body): Json<SettingsBody>,
) -> ApiResult<Json<SettingsBody>> {
    let ScheduleBody { timezone, times } = body.update_schedule;
    let next =
        Schedule::parse(&timezone, &times).map_err(|e| ApiError::bad_request(format!("{e:#}")))?;
    let s = state
        .settings
        .update(|s| s.update_schedule.set(next, Utc::now()))
        .await?;
    Ok(Json((&s.update_schedule).into()))
}
