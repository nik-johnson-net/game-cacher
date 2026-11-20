//! Update check + plan for one app: PICS build id → selected depot manifests →
//! one object group of chunks per depot.

use std::collections::{BTreeMap, HashMap};

use anyhow::{Context, Result, bail};

use super::appinfo::{self, AppInfo, DepotPlan, PUBLIC_BRANCH};
use super::backend::{CdnServer, SteamSession};
use super::manifest::{self, Manifest};
use super::state::AppState;
use crate::platforms::Prefill;
use crate::platforms::plan::{GamePlan, Outcome};

pub const IDENTIFIER: &str = super::Steam::ID;
/// Concurrent chunk warm-ups.
const CHUNK_CONCURRENCY: usize = 16;

/// What `next_state` needs after the warm.
struct Payload {
    previous: AppState,
    buildid: String,
    depots: Vec<PlannedDepot>,
}

struct PlannedDepot {
    key: String,
    gid: String,
    /// The depot's object group; None when its manifest could not be planned.
    group: Option<u16>,
}

/// Plans `app_id` against `previous`: every selected depot's chunks, even when
/// the build id is unchanged (already-cached chunks are cheap and anything
/// evicted is re-downloaded). A depot whose manifest fails is recorded as the
/// plan's error and keeps its previous gid.
pub async fn plan_app(
    session: &dyn SteamSession,
    prefill: &dyn Prefill,
    app_id: u32,
    previous: &AppState,
) -> Result<GamePlan> {
    let app = fetch_app(session, app_id).await?;
    let buildid = app
        .public_buildid
        .clone()
        .with_context(|| format!("app {app_id} has no public branch build id"))?;
    let mut plan = GamePlan::new(IDENTIFIER, &app_id.to_string(), &app.name);
    plan.previous_version = previous.buildid.clone();
    plan.current_version = Some(buildid.clone());
    plan.version_changed = previous.buildid.as_deref() != Some(buildid.as_str());
    plan.concurrency = CHUNK_CONCURRENCY;
    let mut depots = Vec::new();
    for dp in plan_depots(session, &app).await? {
        let group = match plan_depot(session, prefill, app_id, &dp, &mut plan).await {
            Ok(g) => Some(g),
            Err(e) => {
                log::warn!("steam app {app_id} depot {}: {e:#}", dp.depot_id);
                plan.error
                    .get_or_insert_with(|| format!("depot {}: {e:#}", dp.depot_id));
                None
            }
        };
        depots.push(PlannedDepot {
            key: dp.depot_id.to_string(),
            gid: dp.manifest_gid.to_string(),
            group,
        });
    }
    plan.payload = Box::new(Payload {
        previous: previous.clone(),
        buildid,
        depots,
    });
    Ok(plan)
}

/// Adds one depot's chunks as a new object group; returns the group.
async fn plan_depot(
    session: &dyn SteamSession,
    prefill: &dyn Prefill,
    app_id: u32,
    dp: &DepotPlan,
    plan: &mut GamePlan,
) -> Result<u16> {
    if plan.host.is_empty() {
        plan.host = pick_cdn_host(&session.cdn_servers().await?, app_id)
            .context("no usable Steam CDN server")?;
    }
    let manifest = fetch_manifest(session, prefill, &plan.host, dp).await?;
    let group = plan.group(format!("depot {}", dp.depot_id), true);
    for c in manifest.chunks {
        let path = format!("/depot/{}/chunk/{}", dp.depot_id, c.sha);
        plan.add(path, Some(c.compressed_size.into()), group);
    }
    Ok(group)
}

/// State to persist after warming `plan`, or None when it is unchanged. Depots
/// whose chunks all warmed record their new gid, the rest keep the previous one;
/// the build id only advances once everything succeeded.
pub fn next_state(plan: &GamePlan, outcome: &Outcome) -> Option<AppState> {
    let p = plan.payload::<Payload>()?;
    let mut depots = BTreeMap::new();
    for d in &p.depots {
        match d.group {
            Some(g) if outcome.group_ok(g) => {
                depots.insert(d.key.clone(), d.gid.clone());
            }
            _ => {
                if let Some(old) = p.previous.depots.get(&d.key) {
                    depots.insert(d.key.clone(), old.clone());
                }
            }
        }
    }
    let buildid = if outcome.succeeded(plan) {
        Some(p.buildid.clone())
    } else {
        p.previous.buildid.clone()
    };
    let next = AppState { buildid, depots };
    (next != p.previous).then_some(next)
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

async fn fetch_manifest(
    session: &dyn SteamSession,
    prefill: &dyn Prefill,
    host: &str,
    plan: &DepotPlan,
) -> Result<Manifest> {
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
    manifest::parse(&body).context("parsing manifest")
}
