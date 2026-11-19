use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::{Value, json};

use super::platforms::find;
use crate::error::{ApiError, ApiResult};
use crate::platforms::{DynPlatform, Game, LoginRequest, LoginResponse, ManagedUpdates};
use crate::settings::ManagedSettings;
use crate::state::AppState;
use crate::updates::{AlreadyRunning, ManagedStatus};

fn managed(state: &AppState, id: &str) -> ApiResult<DynPlatform> {
    let p = find(state, id)?;
    if p.managed().is_none() {
        return Err(ApiError::not_found(format!("{id} has no managed updates")));
    }
    Ok(p)
}

fn m(p: &DynPlatform) -> &dyn ManagedUpdates {
    p.managed().expect("checked by managed()")
}

pub async fn status(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> ApiResult<Json<ManagedStatus>> {
    let p = managed(&state, &id)?;
    Ok(Json(state.updates.status(&p).await?))
}

pub async fn login(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(req): Json<LoginRequest>,
) -> ApiResult<Json<LoginResponse>> {
    let p = managed(&state, &id)?;
    let resp = m(&p)
        .login(&state.updates.ctx(&id), req)
        .await
        .map_err(|e| {
            log::warn!("{id} login error: {e:#}");
            ApiError::bad_gateway(format!("{e:#}"))
        })?;
    Ok(Json(resp))
}

pub async fn logout(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    let p = managed(&state, &id)?;
    m(&p).logout(&state.updates.ctx(&id)).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn games(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> ApiResult<Json<Vec<Game>>> {
    let p = managed(&state, &id)?;
    let mut games = m(&p)
        .list_games(&state.updates.ctx(&id))
        .await
        .map_err(|e| ApiError::bad_gateway(format!("{e:#}")))?;
    let selected = state.settings.get().await.selected(&id);
    for g in &mut games {
        g.selected = selected.contains(&g.id);
    }
    Ok(Json(games))
}

#[derive(Deserialize)]
pub struct SelectionBody {
    selected: Vec<String>,
}

pub async fn selection(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(body): Json<SelectionBody>,
) -> ApiResult<Json<ManagedStatus>> {
    let p = managed(&state, &id)?;
    let mut selected: Vec<String> = body
        .selected
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let mut seen = std::collections::HashSet::new();
    selected.retain(|s| seen.insert(s.clone()));
    state
        .settings
        .update(|s| {
            s.managed.insert(id.clone(), ManagedSettings { selected });
        })
        .await?;
    Ok(Json(state.updates.status(&p).await?))
}

pub async fn run(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let p = managed(&state, &id)?;
    if !state.settings.get().await.platform_enabled(&id) {
        return Err(ApiError::conflict(format!("{id} is disabled")));
    }
    match state.updates.run_now(p) {
        Ok(()) => Ok((StatusCode::ACCEPTED, Json(json!({ "started": true })))),
        Err(e) if e.downcast_ref::<AlreadyRunning>().is_some() => {
            Err(ApiError::conflict(e.to_string()))
        }
        Err(e) => Err(e.into()),
    }
}
