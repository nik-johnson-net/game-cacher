//! Per-platform run history in `state/<platform>.runs.json`:
//! `{"runs": [UpdateRun]}`, newest first, at most `KEEP` runs.

use std::sync::Arc;

use anyhow::Result;
use serde_json::{Value, json};

use super::model::{LegacyRun, RunStatus, UpdateRun};
use crate::platforms::StateStore;
use crate::settings::SettingsStore;

/// Runs kept per platform.
pub const KEEP: usize = 10;

pub struct History {
    settings: Arc<SettingsStore>,
    /// Serializes load-modify-save of the history documents.
    lock: tokio::sync::Mutex<()>,
}

impl History {
    pub fn new(settings: Arc<SettingsStore>) -> Self {
        Self {
            settings,
            lock: tokio::sync::Mutex::new(()),
        }
    }

    fn store(&self, platform: &str) -> Arc<dyn StateStore> {
        self.settings.state_store(&format!("{platform}.runs"))
    }

    /// Stored runs, newest first, as last saved (a `running` run may be stale).
    pub async fn load(&self, platform: &str) -> Result<Vec<UpdateRun>> {
        Ok(parse(self.store(platform).load().await?, platform))
    }

    /// Inserts or replaces `run` (by id) and trims to `KEEP`. Only one run per
    /// platform is active, so any other stored `running` run is dead and becomes
    /// incomplete. A `running` snapshot never overwrites a finished record (a
    /// late periodic snapshot racing the final save).
    pub async fn save(&self, run: &UpdateRun) -> Result<()> {
        let _guard = self.lock.lock().await;
        let mut runs = self.load(&run.platform).await?;
        match runs.iter_mut().find(|r| r.id == run.id) {
            Some(old) if old.status != RunStatus::Running && run.status == RunStatus::Running => {
                return Ok(());
            }
            Some(old) => *old = run.clone(),
            None => runs.push(run.clone()),
        }
        for r in &mut runs {
            if r.id != run.id && r.status == RunStatus::Running {
                r.mark_incomplete();
            }
        }
        runs.sort_by_key(|r| std::cmp::Reverse(r.started));
        runs.truncate(KEEP);
        self.store(&run.platform)
            .save(&json!({ "runs": runs }))
            .await
    }
}

/// Reads the history document, falling back to the pre-history `last_run` shape.
/// Unreadable entries are skipped with a warning.
fn parse(v: Value, platform: &str) -> Vec<UpdateRun> {
    if let Some(runs) = v.get("runs").and_then(Value::as_array) {
        return runs
            .iter()
            .filter_map(|r| match serde_json::from_value(r.clone()) {
                Ok(run) => Some(run),
                Err(e) => {
                    log::warn!("skipping unreadable {platform} run record: {e}");
                    None
                }
            })
            .collect();
    }
    match v.get("last_run").filter(|r| !r.is_null()) {
        Some(r) => match serde_json::from_value::<LegacyRun>(r.clone()) {
            Ok(legacy) => vec![legacy.into_run(platform)],
            Err(e) => {
                log::warn!("skipping unreadable {platform} last_run: {e}");
                Vec::new()
            }
        },
        None => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, Utc};

    use super::*;
    use crate::storage::MemoryStorage;

    async fn history() -> History {
        let settings = SettingsStore::load(Arc::new(MemoryStorage::new()))
            .await
            .unwrap();
        History::new(Arc::new(settings))
    }

    fn run(minutes_ago: i64) -> UpdateRun {
        let mut r = UpdateRun::new("steam");
        r.started = Utc::now() - Duration::minutes(minutes_ago);
        r
    }

    #[tokio::test]
    async fn keeps_newest_runs_and_replaces_by_id() {
        let h = history().await;
        assert!(h.load("steam").await.unwrap().is_empty());
        let mut runs: Vec<UpdateRun> = (0..12).map(|i| run(100 - i)).collect();
        for r in &mut runs {
            r.finish(false);
            h.save(r).await.unwrap();
        }
        let stored = h.load("steam").await.unwrap();
        assert_eq!(stored.len(), KEEP);
        assert_eq!(stored[0].id, runs[11].id);
        assert_eq!(stored[9].id, runs[2].id);

        runs[11].status = RunStatus::Error;
        h.save(&runs[11]).await.unwrap();
        let stored = h.load("steam").await.unwrap();
        assert_eq!(stored.len(), KEEP);
        assert_eq!(stored[0].status, RunStatus::Error);
        assert!(h.load("epic").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn stale_running_runs_become_incomplete() {
        let h = history().await;
        let dead = run(10);
        h.save(&dead).await.unwrap();
        let live = run(1);
        h.save(&live).await.unwrap();
        let stored = h.load("steam").await.unwrap();
        assert_eq!(stored[0].status, RunStatus::Running);
        assert_eq!(stored[1].status, RunStatus::Incomplete);
    }

    #[tokio::test]
    async fn running_snapshot_never_overwrites_a_finished_run() {
        let h = history().await;
        let snapshot = run(1);
        let mut done = snapshot.clone();
        done.finish(false);
        h.save(&done).await.unwrap();
        h.save(&snapshot).await.unwrap();
        assert_eq!(h.load("steam").await.unwrap()[0].status, RunStatus::Ok);
    }

    #[test]
    fn parses_legacy_and_skips_bad_entries() {
        let legacy =
            json!({"last_run": {"started": "2026-11-01T00:00:00Z", "finished": null, "games": []}});
        let runs = parse(legacy, "epic");
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].status, RunStatus::Incomplete);
        assert_eq!(runs[0].platform, "epic");

        assert!(parse(json!({"last_run": null}), "epic").is_empty());
        assert!(parse(json!({"last_run": {"bogus": 1}}), "epic").is_empty());
        assert!(parse(json!({}), "epic").is_empty());

        let good = serde_json::to_value(run(1)).unwrap();
        let runs = parse(json!({"runs": [good, {"id": 1}]}), "steam");
        assert_eq!(runs.len(), 1);
    }
}
