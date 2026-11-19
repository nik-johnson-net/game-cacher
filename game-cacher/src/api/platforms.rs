use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use serde::{Deserialize, Serialize};

use crate::error::{ApiError, ApiResult};
use crate::metrics::PlatformCounters;
use crate::platforms::DynPlatform;
use crate::settings::PlatformSettings;
use crate::state::AppState;

#[derive(Serialize)]
pub struct PlatformStats {
    #[serde(flatten)]
    counters: PlatformCounters,
    storage_bytes: u64,
    objects: u64,
}

#[derive(Serialize)]
pub struct PlatformSummary {
    id: &'static str,
    name: &'static str,
    description: &'static str,
    enabled: bool,
    domains: &'static [&'static str],
    managed: bool,
    stats: PlatformStats,
}

pub(super) fn find(state: &AppState, id: &str) -> ApiResult<DynPlatform> {
    state
        .platforms
        .get(id)
        .cloned()
        .ok_or_else(|| ApiError::not_found(format!("unknown platform: {id}")))
}

async fn summary(state: &AppState, p: &DynPlatform) -> PlatformSummary {
    let usage = state.index.usage();
    let u = usage
        .per_identifier
        .get(p.id())
        .cloned()
        .unwrap_or_default();
    PlatformSummary {
        id: p.id(),
        name: p.name(),
        description: p.description(),
        enabled: state.settings.get().await.platform_enabled(p.id()),
        domains: p.domains(),
        managed: p.managed().is_some(),
        stats: PlatformStats {
            counters: state.metrics.platform_counters(p.id()),
            storage_bytes: u.bytes,
            objects: u.objects,
        },
    }
}

pub async fn list(State(state): State<Arc<AppState>>) -> Json<Vec<PlatformSummary>> {
    let mut out = Vec::new();
    for p in state.platforms.all() {
        out.push(summary(&state, p).await);
    }
    Json(out)
}

pub async fn get_one(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> ApiResult<Json<PlatformSummary>> {
    let p = find(&state, &id)?;
    Ok(Json(summary(&state, &p).await))
}

#[derive(Deserialize)]
pub struct UpdateBody {
    enabled: bool,
}

pub async fn update(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(body): Json<UpdateBody>,
) -> ApiResult<Json<PlatformSummary>> {
    let p = find(&state, &id)?;
    state
        .settings
        .update(|s| {
            s.platforms.insert(
                id.clone(),
                PlatformSettings {
                    enabled: body.enabled,
                },
            );
        })
        .await?;
    log::info!(
        "platform {id} {}",
        if body.enabled { "enabled" } else { "disabled" }
    );
    Ok(Json(summary(&state, &p).await))
}

#[derive(Serialize)]
pub struct PurgeResult {
    deleted_objects: u64,
    freed_bytes: u64,
}

pub async fn purge(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> ApiResult<Json<PurgeResult>> {
    let p = find(&state, &id)?;
    let before = state
        .index
        .usage()
        .per_identifier
        .get(p.id())
        .cloned()
        .unwrap_or_default();
    let (_, freed_bytes) = state
        .storage
        .delete_prefix(&format!("cache/{}/", p.id()))
        .await?;
    state.index.remove_identifier(p.id());
    log::info!(
        "purged {id} cache: {} objects, {freed_bytes} bytes",
        before.objects
    );
    Ok(Json(PurgeResult {
        deleted_objects: before.objects,
        freed_bytes,
    }))
}
