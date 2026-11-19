use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use serde_json::{Value, json};

use crate::state::AppState;

pub async fn status(State(state): State<Arc<AppState>>) -> Json<Value> {
    let usage = state.index.usage();
    let c = &state.config;
    Json(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "uptime_secs": state.started.elapsed().as_secs(),
        "storage": {
            "backend": state.storage.name(),
            "used_bytes": usage.total_bytes,
            "max_bytes": state.index.max_bytes(),
            "objects": usage.objects,
        },
        "secrets_backend": state.secrets.name(),
        "dns": {
            "enabled": !c.disable_dns,
            "cache_ips": c.cache_ip,
            "forwarders": c.dns_forward.iter().map(|f| f.0.to_string()).collect::<Vec<_>>(),
        },
        "sni_enabled": !c.disable_sni,
    }))
}
