//! Managed-update scheduler and run tracking. Each managed platform's selected
//! games are updated at the times in `update_schedule` (see `schedule`). A run
//! lists the games, checks what each needs, then downloads them one by one
//! (`runner`); live progress is kept in memory (`progress`) and the last runs
//! in `state/<platform>.runs.json` (`history`).

mod history;
pub mod model;
mod progress;
mod runner;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::Serialize;

use self::history::History;
pub use self::model::{GameRun, GameStatus, Phase, RunStatus, UpdateRun};
use self::progress::ActiveRun;
use crate::metrics::Metrics;
use crate::platforms::{AuthStatus, DynPlatform, LoginMethod, PlatformCtx, Platforms, Prefill};
use crate::secrets::DynSecrets;
use crate::settings::SettingsStore;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ManagedStatus {
    pub auth: AuthStatus,
    pub login_methods: Vec<LoginMethod>,
    pub selected: Vec<String>,
    pub running: bool,
    /// The latest run (live while running).
    pub last_run: Option<UpdateRun>,
    pub next_run: Option<DateTime<Utc>>,
}

#[derive(Debug, thiserror::Error)]
#[error("an update run is already in progress")]
pub struct AlreadyRunning;

#[derive(Debug, thiserror::Error)]
#[error("no update run is in progress")]
pub struct NotRunning;

pub struct UpdateManager {
    platforms: Arc<Platforms>,
    settings: Arc<SettingsStore>,
    secrets: DynSecrets,
    http: reqwest::Client,
    prefill: Arc<dyn Prefill>,
    metrics: Arc<Metrics>,
    history: History,
    /// Active run per platform id.
    active: Mutex<HashMap<String, Arc<ActiveRun>>>,
    snapshot_every: Duration,
}

impl UpdateManager {
    pub fn new(
        platforms: Arc<Platforms>,
        settings: Arc<SettingsStore>,
        secrets: DynSecrets,
        http: reqwest::Client,
        prefill: Arc<dyn Prefill>,
        metrics: Arc<Metrics>,
    ) -> Arc<Self> {
        Arc::new(Self {
            platforms,
            history: History::new(settings.clone()),
            settings,
            secrets,
            http,
            prefill,
            metrics,
            active: Mutex::new(HashMap::new()),
            snapshot_every: runner::SNAPSHOT_EVERY,
        })
    }

    pub fn ctx(&self, platform_id: &str) -> PlatformCtx {
        PlatformCtx {
            secrets: self.secrets.clone(),
            state: self.settings.state_store(platform_id),
            settings: self.settings.clone(),
            http: self.http.clone(),
            prefill: self.prefill.clone(),
        }
    }

    fn active_map(&self) -> std::sync::MutexGuard<'_, HashMap<String, Arc<ActiveRun>>> {
        self.active.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn active_run(&self, platform_id: &str) -> Option<Arc<ActiveRun>> {
        self.active_map().get(platform_id).cloned()
    }

    fn is_active(&self, run: &Arc<ActiveRun>) -> bool {
        self.active_map().values().any(|a| Arc::ptr_eq(a, run))
    }

    pub fn is_running(&self, platform_id: &str) -> bool {
        self.active_map().contains_key(platform_id)
    }

    async fn save_snapshot(&self, active: &ActiveRun) {
        let run = active.view(Instant::now());
        if let Err(e) = self.history.save(&run).await {
            log::warn!("saving {} run record: {e:#}", run.platform);
        }
    }

    /// A platform's runs, newest first: the active one live, stored ones as saved,
    /// except a stored `running` run that isn't active (its process died), which
    /// is shown as incomplete.
    pub async fn runs(&self, platform_id: &str) -> Result<Vec<UpdateRun>> {
        let mut runs = self.history.load(platform_id).await?;
        for r in runs.iter_mut().filter(|r| r.status == RunStatus::Running) {
            r.mark_incomplete();
        }
        if let Some(active) = self.active_run(platform_id) {
            let live = active.view(Instant::now());
            runs.retain(|r| r.id != live.id);
            runs.insert(0, live);
        }
        Ok(runs)
    }

    /// Runs of every managed platform, newest first.
    pub async fn all_runs(&self) -> Result<Vec<UpdateRun>> {
        let mut all = Vec::new();
        for p in self
            .platforms
            .all()
            .iter()
            .filter(|p| p.managed().is_some())
        {
            all.extend(self.runs(p.id()).await?);
        }
        all.sort_by_key(|r| std::cmp::Reverse(r.started));
        Ok(all)
    }

    pub async fn last_run(&self, platform_id: &str) -> Result<Option<UpdateRun>> {
        Ok(self.runs(platform_id).await?.into_iter().next())
    }

    /// The schedule's reference point: when the last run started, or None
    /// (run as soon as the schedule allows) when it was left incomplete.
    fn schedule_anchor(last: Option<&UpdateRun>) -> Option<DateTime<Utc>> {
        last.filter(|r| r.status != RunStatus::Incomplete)
            .map(|r| r.started)
    }

    pub async fn status(&self, platform: &DynPlatform) -> Result<ManagedStatus> {
        let managed = platform
            .managed()
            .ok_or_else(|| anyhow::anyhow!("not managed"))?;
        let id = platform.id();
        let settings = self.settings.get().await;
        let last_run = self.last_run(id).await?;
        let running = self.is_running(id);
        let auth = managed.auth_status(&self.ctx(id)).await?;
        let next_run = if settings.selected(id).is_empty()
            || !settings.platform_enabled(id)
            || (auth.required && !auth.logged_in)
        {
            None
        } else {
            settings
                .update_schedule
                .next_run(Self::schedule_anchor(last_run.as_ref()), Utc::now())
        };
        Ok(ManagedStatus {
            auth,
            login_methods: managed.login_methods().to_vec(),
            selected: settings.selected(id),
            running,
            last_run,
            next_run,
        })
    }

    /// Starts a run in the background. Errors with `AlreadyRunning` if one is active.
    pub fn run_now(self: &Arc<Self>, platform: DynPlatform) -> Result<()> {
        let id = platform.id().to_string();
        let active = Arc::new(ActiveRun::new(UpdateRun::new(&id)));
        {
            let mut map = self.active_map();
            if map.contains_key(&id) {
                return Err(AlreadyRunning.into());
            }
            map.insert(id.clone(), active.clone());
        }
        runner::spawn_snapshots(self.clone(), active.clone(), self.snapshot_every);
        let this = self.clone();
        tokio::spawn(async move {
            // Unregisters the run even if it panics.
            let _guard = RunningGuard(this.clone(), id);
            runner::run(&this, &platform, &active).await;
        });
        Ok(())
    }

    /// Asks the platform's active run to stop. Errors with `NotRunning` if none is.
    pub fn cancel(&self, platform_id: &str) -> Result<()> {
        let active = self.active_run(platform_id).ok_or(NotRunning)?;
        log::info!("cancelling managed update run for {platform_id}");
        active.cancel.cancel();
        Ok(())
    }

    /// Starts runs for platforms that are due. Called every minute by `spawn_scheduler`.
    pub async fn tick(self: &Arc<Self>) {
        let settings = self.settings.get().await;
        for platform in self.platforms.all() {
            let id = platform.id();
            let Some(managed) = platform.managed() else {
                continue;
            };
            if !settings.platform_enabled(id)
                || settings.selected(id).is_empty()
                || self.is_running(id)
            {
                continue;
            }
            let last = match self.last_run(id).await {
                Ok(l) => l,
                Err(e) => {
                    log::warn!("reading run state for {id}: {e:#}");
                    continue;
                }
            };
            if !settings
                .update_schedule
                .is_due(Self::schedule_anchor(last.as_ref()), Utc::now())
            {
                continue;
            }
            match managed.auth_status(&self.ctx(id)).await {
                Ok(a) if a.required && !a.logged_in => continue,
                Err(e) => {
                    log::warn!("auth status for {id}: {e:#}");
                    continue;
                }
                _ => {}
            }
            let _ = self.run_now(platform.clone());
        }
    }

    pub fn spawn_scheduler(self: &Arc<Self>, period: Duration) -> tokio::task::JoinHandle<()> {
        let this = self.clone();
        tokio::spawn(async move {
            // First check one period after startup, not immediately.
            let mut interval =
                tokio::time::interval_at(tokio::time::Instant::now() + period, period);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                interval.tick().await;
                this.tick().await;
            }
        })
    }
}

struct RunningGuard(Arc<UpdateManager>, String);

impl Drop for RunningGuard {
    fn drop(&mut self) {
        self.0.active_map().remove(&self.1);
    }
}
