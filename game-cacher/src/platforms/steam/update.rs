//! Update check + prefill for one app: PICS build id → changed depot manifests
//! → manifest download → chunk warm-up through the cache.

use std::collections::{BTreeMap, HashMap};

use anyhow::{Context, Result, anyhow, bail};
use futures::StreamExt;

use super::appinfo::{self, AppInfo, DepotPlan, PUBLIC_BRANCH};
use super::backend::{CdnServer, SteamSession};
use super::manifest;
use super::state::AppState;
use crate::platforms::{Prefill, UpdateReport};

pub const IDENTIFIER: &str = "steam";
/// Concurrent chunk warm-ups per depot.
const CHUNK_CONCURRENCY: usize = 16;

/// Checks `app_id` against `previous` and warms every selected depot through the
/// cache, even when the build id is unchanged: already-cached chunks are cheap
/// and anything evicted is re-downloaded (verification/repair). Returns the
/// report and the state to persist. Depot failures are recorded in the report
/// (first error) and leave that depot's previous gid in place so it is retried;
/// the new build id is only recorded once every depot succeeded.
pub async fn update_app(
    session: &dyn SteamSession,
    prefill: &dyn Prefill,
    app_id: u32,
    previous: &AppState,
) -> Result<(UpdateReport, AppState)> {
    let app = fetch_app(session, app_id).await?;
    let buildid = app
        .public_buildid
        .clone()
        .with_context(|| format!("app {app_id} has no public branch build id"))?;
    let mut report = UpdateReport {
        game_id: app_id.to_string(),
        name: app.name.clone(),
        previous_version: previous.buildid.clone(),
        current_version: Some(buildid.clone()),
        ..Default::default()
    };
    let build_changed = previous.buildid.as_deref() != Some(buildid.as_str());

    let plans = plan_depots(session, &app).await?;
    let (depots, error) =
        prefill_plans(session, prefill, app_id, &plans, previous, &mut report).await;
    // A build change, or any bytes fetched while verifying (cache evicted or
    // purged), counts as an update.
    report.updated = build_changed || report.bytes_downloaded > 0;
    // Until every depot succeeds, keep the old build id so the next run retries.
    let buildid = if error.is_none() {
        Some(buildid)
    } else {
        previous.buildid.clone()
    };
    report.error = error;
    Ok((report, AppState { buildid, depots }))
}

/// Prefetches every plan (changed or not, to verify the cache), accumulating
/// into `report`. Returns the depot → gid map to persist and the first error.
async fn prefill_plans(
    session: &dyn SteamSession,
    prefill: &dyn Prefill,
    app_id: u32,
    plans: &[DepotPlan],
    previous: &AppState,
    report: &mut UpdateReport,
) -> (BTreeMap<String, String>, Option<String>) {
    let mut depots = BTreeMap::new();
    let mut cdn: Option<String> = None;
    let mut first_error = None;
    for plan in plans {
        let key = plan.depot_id.to_string();
        let gid = plan.manifest_gid.to_string();
        let result = async {
            let host = match &cdn {
                Some(h) => h.clone(),
                None => {
                    let h = pick_cdn_host(&session.cdn_servers().await?, app_id)
                        .context("no usable Steam CDN server")?;
                    cdn.insert(h).clone()
                }
            };
            prefill_depot(session, prefill, &host, plan).await
        }
        .await;
        match result {
            Ok(depot) => {
                report.files += depot.chunks;
                report.bytes_downloaded += depot.bytes_downloaded;
                report.bytes_already_cached += depot.bytes_already_cached;
                depots.insert(key, gid);
            }
            Err(e) => {
                log::warn!("steam app {app_id} depot {}: {e:#}", plan.depot_id);
                first_error.get_or_insert(format!("depot {}: {e:#}", plan.depot_id));
                if let Some(old) = previous.depots.get(&key) {
                    depots.insert(key, old.clone());
                }
            }
        }
    }
    (depots, first_error)
}

async fn fetch_app(session: &dyn SteamSession, app_id: u32) -> Result<AppInfo> {
    session
        .app_info(&[app_id])
        .await?
        .into_iter()
        .find(|a| a.app_id == app_id)
        .with_context(|| format!("no PICS app info for app {app_id}"))
}

/// Depot selection for `app`, resolving shared-install depots via their source app.
pub async fn plan_depots(session: &dyn SteamSession, app: &AppInfo) -> Result<Vec<DepotPlan>> {
    let owned = session.ownership().await?;
    let depots = appinfo::filter_depots(app, &owned);
    let proxy_ids = appinfo::proxy_apps(app, &depots);
    let proxies: HashMap<u32, AppInfo> = if proxy_ids.is_empty() {
        HashMap::new()
    } else {
        session
            .app_info(&proxy_ids)
            .await?
            .into_iter()
            .map(|a| (a.app_id, a))
            .collect()
    };
    let (plans, notes) = appinfo::resolve_plans(app, &depots, &proxies);
    for note in notes {
        log::info!("steam app {}: {note}", app.app_id);
    }
    Ok(plans)
}

/// Best content server for `app_id`: CDN/SteamCache type, allowed for the app,
/// not a proxy; lowest weighted load wins. Returns its vhost. `https_support`
/// is ignored: steamcontent.com servers also answer plain HTTP (lancache relies
/// on this), and the cache engine fetches upstream over HTTP.
pub fn pick_cdn_host(servers: &[CdnServer], app_id: u32) -> Option<String> {
    servers
        .iter()
        .filter(|s| s.kind == "CDN" || s.kind == "SteamCache")
        .filter(|s| s.allowed_app_ids.is_empty() || s.allowed_app_ids.contains(&app_id))
        .filter(|s| !s.use_as_proxy)
        .min_by(|a, b| a.weighted_load.total_cmp(&b.weighted_load))
        .map(|s| {
            if s.vhost.is_empty() {
                s.host.clone()
            } else {
                s.vhost.clone()
            }
        })
}

#[derive(Debug, Default)]
struct DepotResult {
    chunks: u64,
    bytes_downloaded: u64,
    bytes_already_cached: u64,
}

async fn prefill_depot(
    session: &dyn SteamSession,
    prefill: &dyn Prefill,
    host: &str,
    plan: &DepotPlan,
) -> Result<DepotResult> {
    let code = session
        .manifest_request_code(
            plan.containing_app,
            plan.depot_id,
            plan.manifest_gid,
            PUBLIC_BRANCH,
        )
        .await
        .context("manifest request code")?;
    if code == 0 {
        bail!("Steam returned no manifest request code (no access?)");
    }
    let path = format!(
        "/depot/{}/manifest/{}/5/{code}",
        plan.depot_id, plan.manifest_gid
    );
    let body = prefill
        .get(IDENTIFIER, host, &path)
        .await
        .context("downloading manifest")?;
    let manifest = manifest::parse(&body).context("parsing manifest")?;

    let paths: Vec<String> = manifest
        .chunks
        .iter()
        .map(|sha| format!("/depot/{}/chunk/{sha}", plan.depot_id))
        .collect();
    let mut results = futures::stream::iter(paths)
        .map(|path| async move {
            prefill
                .warm(IDENTIFIER, host, &path)
                .await
                .with_context(|| format!("GET {path}"))
        })
        .buffer_unordered(CHUNK_CONCURRENCY);
    let mut out = DepotResult::default();
    let mut failures = 0usize;
    let mut first_error = None;
    while let Some(r) = results.next().await {
        match r {
            Ok(w) => {
                out.chunks += 1;
                out.bytes_downloaded += w.bytes_downloaded;
                out.bytes_already_cached += w.bytes_already_cached;
            }
            Err(e) => {
                failures += 1;
                first_error.get_or_insert(e);
            }
        }
    }
    match first_error {
        None => Ok(out),
        Some(e) => Err(anyhow!(
            "{failures} of {} chunks failed: {e:#}",
            manifest.chunks.len()
        )),
    }
}
