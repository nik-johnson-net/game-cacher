//! Admin listener: REST API (docs/ARCHITECTURE.md "REST API"), metrics, health, UI.

pub mod auth;
mod managed;
mod platforms;
mod settings;
mod status;

use std::path::Path;
use std::sync::Arc;

use axum::Router;
use axum::extract::State;
use axum::http::header;
use axum::middleware;
use axum::response::IntoResponse;
use axum::routing::{delete, get, post, put};
use tower_http::services::{ServeDir, ServeFile};

use crate::state::AppState;

pub fn router(state: Arc<AppState>) -> Router {
    let protected = Router::new()
        .route("/status", get(status::status))
        .route("/platforms", get(platforms::list))
        .route(
            "/platforms/{id}",
            get(platforms::get_one).put(platforms::update),
        )
        .route("/platforms/{id}/cache", delete(platforms::purge))
        .route("/platforms/{id}/managed", get(managed::status))
        .route("/platforms/{id}/managed/login", post(managed::login))
        .route("/platforms/{id}/managed/logout", post(managed::logout))
        .route("/platforms/{id}/managed/games", get(managed::games))
        .route("/platforms/{id}/managed/selection", put(managed::selection))
        .route("/platforms/{id}/managed/run", post(managed::run))
        .route(
            "/settings",
            get(settings::get_settings).put(settings::put_settings),
        )
        .route("/auth/logout", post(auth::logout))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_auth,
        ));

    let api = Router::new()
        .route("/auth/login", post(auth::login))
        .route("/auth/status", get(auth::status))
        .merge(protected);

    Router::new()
        .nest("/api", api)
        .route("/healthz", get(|| async { "ok" }))
        .route("/metrics", get(metrics))
        .fallback_service(ui(&state.config.ui_path))
        .with_state(state)
}

async fn metrics(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    (
        [(
            header::CONTENT_TYPE,
            "application/openmetrics-text; version=1.0.0; charset=utf-8",
        )],
        state.metrics.encode(),
    )
}

/// Serves the built SPA; unknown paths fall back to index.html for client routing.
fn ui(path: &Path) -> Router {
    let index = path.join("index.html");
    if !index.exists() {
        log::warn!("UI not found at {} — serving API only", path.display());
        return Router::new().fallback(|| async {
            (
                axum::http::StatusCode::NOT_FOUND,
                "game-cacher UI is not installed",
            )
        });
    }
    Router::new().fallback_service(ServeDir::new(path).fallback(ServeFile::new(index)))
}
