//! Managed-update scheduler and run tracking. Each managed platform's selected
//! games are updated every `update_interval_minutes`; run history is kept in
//! `state/<platform>.runs.json`.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::metrics::Metrics;
use crate::platforms::{
    AuthStatus, DynPlatform, LoginMethod, PlatformCtx, Platforms, Prefill, UpdateReport,
};
use crate::secrets::DynSecrets;
use crate::settings::SettingsStore;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateRun {
    pub started: DateTime<Utc>,
    pub finished: Option<DateTime<Utc>>,
    pub games: Vec<UpdateReport>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ManagedStatus {
    pub auth: AuthStatus,
    pub login_methods: Vec<LoginMethod>,
    pub selected: Vec<String>,
    pub running: bool,
    pub last_run: Option<UpdateRun>,
    pub next_run: Option<DateTime<Utc>>,
}

#[derive(Debug, thiserror::Error)]
#[error("an update run is already in progress")]
pub struct AlreadyRunning;

pub struct UpdateManager {
    platforms: Arc<Platforms>,
    settings: Arc<SettingsStore>,
    secrets: DynSecrets,
    http: reqwest::Client,
    prefill: Arc<dyn Prefill>,
    metrics: Arc<Metrics>,
    running: Mutex<HashSet<String>>,
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
            settings,
            secrets,
            http,
            prefill,
            metrics,
            running: Mutex::new(HashSet::new()),
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

    pub fn is_running(&self, platform_id: &str) -> bool {
        self.running
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(platform_id)
    }

    pub async fn last_run(&self, platform_id: &str) -> Result<Option<UpdateRun>> {
        let v = self.runs_store(platform_id).load().await?;
        Ok(serde_json::from_value::<Option<UpdateRun>>(
            v.get("last_run").cloned().unwrap_or_default(),
        )
        .unwrap_or_default())
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
            Some(next_due(
                last_run.as_ref(),
                settings.update_interval_minutes,
            ))
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
        if !self
            .running
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id.clone())
        {
            return Err(AlreadyRunning.into());
        }
        let this = self.clone();
        tokio::spawn(async move {
            // Clears the running flag even if the run panics.
            let _guard = RunningGuard(this.clone(), id.clone());
            if let Err(e) = this.run(&platform).await {
                log::error!("managed update run for {id} failed: {e:#}");
            }
        });
        Ok(())
    }

    async fn run(&self, platform: &DynPlatform) -> Result<()> {
        let Some(managed) = platform.managed() else {
            return Ok(());
        };
        let id = platform.id();
        let ctx = self.ctx(id);
        let selected = self.settings.get().await.selected(id);
        log::info!("managed update run for {id}: {} game(s)", selected.len());
        let mut run = UpdateRun {
            started: Utc::now(),
            finished: None,
            games: vec![],
        };
        self.save_run(id, &run).await?;
        let mut ok = true;
        for game_id in selected {
            let report = match managed.update_game(&ctx, &game_id).await {
                Ok(r) => r,
                Err(e) => UpdateReport {
                    game_id: game_id.clone(),
                    name: game_id.clone(),
                    error: Some(format!("{e:#}")),
                    ..Default::default()
                },
            };
            ok &= report.error.is_none();
            self.metrics.update_bytes(id, report.bytes_downloaded);
            log::info!(
                "{id}/{}: updated={} downloaded={} cached={} error={:?}",
                report.game_id,
                report.updated,
                report.bytes_downloaded,
                report.bytes_already_cached,
                report.error
            );
            run.games.push(report);
            self.save_run(id, &run).await?;
        }
        run.finished = Some(Utc::now());
        self.save_run(id, &run).await?;
        self.metrics.update_run(id, ok);
        Ok(())
    }

    fn runs_store(&self, platform_id: &str) -> Arc<dyn crate::platforms::StateStore> {
        self.settings.state_store(&format!("{platform_id}.runs"))
    }

    async fn save_run(&self, platform_id: &str, run: &UpdateRun) -> Result<()> {
        self.runs_store(platform_id)
            .save(&serde_json::json!({ "last_run": run }))
            .await
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
            if next_due(last.as_ref(), settings.update_interval_minutes) > Utc::now() {
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
        self.0
            .running
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.1);
    }
}

/// Next scheduled run: `interval` after the last run started; now if never run.
fn next_due(last: Option<&UpdateRun>, interval_minutes: u32) -> DateTime<Utc> {
    match last {
        Some(r) => r.started + chrono::Duration::minutes(interval_minutes as i64),
        None => Utc::now(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn due_calculation() {
        let now = Utc::now();
        assert!(next_due(None, 60) <= Utc::now());
        let run = UpdateRun {
            started: now,
            finished: None,
            games: vec![],
        };
        assert_eq!(
            next_due(Some(&run), 60),
            now + chrono::Duration::minutes(60)
        );
    }
}
