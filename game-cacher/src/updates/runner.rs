//! One update run: list the selected games, check each (plan + download
//! estimate), then download them one by one. The record is saved at every
//! phase/game transition and by a periodic snapshot task.

use std::sync::Arc;
use std::time::{Duration, Instant};

use super::UpdateManager;
use super::model::{GameRun, GameStatus, Phase};
use super::progress::ActiveRun;
use crate::platforms::plan::{self, Estimate, GamePlan};
use crate::platforms::{DynPlatform, ManagedUpdates, PlatformCtx, UpdateReport};

/// How often an active run's record is saved while a game downloads.
pub const SNAPSHOT_EVERY: Duration = Duration::from_secs(10);

struct Run<'a> {
    mgr: &'a UpdateManager,
    managed: &'a dyn ManagedUpdates,
    ctx: PlatformCtx,
    active: &'a ActiveRun,
    id: &'a str,
}

/// Runs every phase and saves the final record.
pub async fn run(mgr: &UpdateManager, platform: &DynPlatform, active: &ActiveRun) {
    let Some(managed) = platform.managed() else {
        return;
    };
    let id = platform.id();
    let r = Run {
        mgr,
        managed,
        ctx: mgr.ctx(id),
        active,
        id,
    };
    r.list().await;
    let plans = r.check().await;
    r.download(plans).await;
    let cancel_requested = active.cancel.is_cancelled();
    let status = active.update(|run| {
        run.finish(cancel_requested);
        run.status
    });
    mgr.metrics.update_run(id, status.metric_result());
    r.save().await;
    log::info!("managed update run for {id} finished: {status:?}");
}

impl Run<'_> {
    async fn save(&self) {
        self.mgr.save_snapshot(self.active).await;
    }

    /// Phase 1: the games to check, recorded as pending.
    async fn list(&self) {
        let selected = self.mgr.settings.get().await.selected(self.id);
        log::info!(
            "managed update run for {}: {} game(s)",
            self.id,
            selected.len()
        );
        let mut games = Vec::with_capacity(selected.len());
        for g in &selected {
            games.push(GameRun::pending(
                g,
                self.managed.game_name(&self.ctx, g).await,
            ));
        }
        self.active.update(|run| {
            run.games = games;
            run.phase = Some(Phase::Checking);
        });
        self.save().await;
    }

    /// Phase 2: plans every game and estimates its download.
    async fn check(&self) -> Vec<Option<(GamePlan, Estimate)>> {
        let ids: Vec<String> = self
            .active
            .update(|run| run.games.iter().map(|g| g.report.game_id.clone()).collect());
        let mut plans = Vec::with_capacity(ids.len());
        for (i, game_id) in ids.iter().enumerate() {
            if self.active.cancel.is_cancelled() {
                break;
            }
            self.set_game(i, |g| g.status = GameStatus::Checking);
            plans.push(self.check_game(i, game_id).await);
            self.save().await;
        }
        plans
    }

    async fn check_game(&self, i: usize, game_id: &str) -> Option<(GamePlan, Estimate)> {
        let plan = match self.managed.plan_game(&self.ctx, game_id).await {
            Ok(p) => p,
            Err(e) => {
                log::warn!("{}/{game_id}: check failed: {e:#}", self.id);
                self.set_game(i, |g| {
                    let name = std::mem::take(&mut g.report.name);
                    g.report = UpdateReport::failed(game_id, name, &e);
                    g.status = GameStatus::Failed;
                });
                return None;
            }
        };
        let est = plan::estimate(self.ctx.prefill.as_ref(), &plan, &self.active.cancel).await;
        self.set_game(i, |g| {
            g.report.name = plan.name.clone();
            g.report.previous_version = plan.previous_version.clone();
            g.report.current_version = plan.current_version.clone();
            g.report.bytes_downloaded = est.bytes_fetched;
            g.bytes_to_download = Some(est.bytes_to_download);
            g.objects = plan.objects.len() as u64;
            g.status = GameStatus::Queued;
        });
        Some((plan, est))
    }

    /// Phase 3: warms the checked games one by one and commits each.
    async fn download(&self, plans: Vec<Option<(GamePlan, Estimate)>>) {
        self.active
            .update(|run| run.phase = Some(Phase::Downloading));
        self.active.start_downloads(Instant::now());
        self.save().await;
        for (i, entry) in plans.into_iter().enumerate() {
            if self.active.cancel.is_cancelled() {
                break;
            }
            let Some((plan, est)) = entry else { continue };
            self.active.start_game(i, Instant::now());
            let mut outcome = plan::execute(
                self.ctx.prefill.as_ref(),
                &plan,
                self.active,
                &self.active.cancel,
            )
            .await;
            outcome.add_fetched(est.bytes_fetched);
            let report = self.managed.finish_game(&self.ctx, &plan, &outcome).await;
            self.mgr
                .metrics
                .update_bytes(self.id, report.bytes_downloaded);
            log::info!(
                "{}/{}: updated={} downloaded={} cached={} error={:?}",
                self.id,
                report.game_id,
                report.updated,
                report.bytes_downloaded,
                report.bytes_already_cached,
                report.error
            );
            self.active.end_game();
            self.set_game(i, |g| {
                g.status = match (&report.error, outcome.cancelled) {
                    (_, true) => GameStatus::Cancelled,
                    (Some(_), false) => GameStatus::Failed,
                    (None, false) => GameStatus::Done,
                };
                g.report = report;
            });
            self.save().await;
        }
    }

    fn set_game(&self, i: usize, f: impl FnOnce(&mut GameRun)) {
        self.active.update(|run| {
            if let Some(g) = run.games.get_mut(i) {
                f(g);
            }
        });
    }
}

/// Saves `active` every `period` until the run is no longer registered.
pub fn spawn_snapshots(mgr: Arc<UpdateManager>, active: Arc<ActiveRun>, period: Duration) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
        loop {
            interval.tick().await;
            if !mgr.is_active(&active) {
                return;
            }
            mgr.save_snapshot(&active).await;
        }
    });
}
