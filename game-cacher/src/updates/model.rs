//! Run records: what an update run did, or is doing, per game. Stored in
//! `state/<platform>.runs.json` and served by the API.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::platforms::UpdateReport;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Running,
    /// Finished and every game succeeded.
    Ok,
    /// Finished with at least one failed game.
    Error,
    Cancelled,
    /// Stopped without finishing (crash or restart).
    Incomplete,
}

impl RunStatus {
    /// `result` label of `managed_update_runs_total`.
    pub fn metric_result(self) -> &'static str {
        match self {
            RunStatus::Ok => "ok",
            RunStatus::Cancelled => "cancelled",
            _ => "error",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// Compiling the list of games to check.
    Listing,
    /// Planning each game and estimating what it needs to download.
    Checking,
    /// Warming games one by one.
    Downloading,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GameStatus {
    Pending,
    Checking,
    /// Checked, waiting to download.
    Queued,
    Downloading,
    Done,
    Failed,
    Cancelled,
}

impl GameStatus {
    /// Queued or downloading: still has bytes to fetch in this run.
    pub fn is_remaining(self) -> bool {
        matches!(self, GameStatus::Queued | GameStatus::Downloading)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameRun {
    /// Live while the game downloads, final once it is done.
    #[serde(flatten)]
    pub report: UpdateReport,
    pub status: GameStatus,
    /// Estimated bytes to download, known once the game is checked.
    #[serde(default)]
    pub bytes_to_download: Option<u64>,
    #[serde(default)]
    pub objects: u64,
    /// Objects finished: found cached when checked, or warmed (or failed) since.
    #[serde(default)]
    pub objects_done: u64,
    /// Live only (never read back from storage).
    #[serde(default, skip_deserializing)]
    pub rate_bps: Option<u64>,
    #[serde(default, skip_deserializing)]
    pub eta_secs: Option<u64>,
}

impl GameRun {
    pub fn pending(game_id: &str, name: String) -> Self {
        Self {
            report: UpdateReport {
                game_id: game_id.to_string(),
                name,
                ..Default::default()
            },
            status: GameStatus::Pending,
            bytes_to_download: None,
            objects: 0,
            objects_done: 0,
            rate_bps: None,
            eta_secs: None,
        }
    }

    /// Estimated bytes this game still has to download in this run.
    pub fn bytes_remaining(&self) -> u64 {
        if !self.status.is_remaining() {
            return 0;
        }
        self.bytes_to_download
            .unwrap_or(0)
            .saturating_sub(self.report.bytes_downloaded)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateRun {
    pub id: String,
    pub platform: String,
    pub started: DateTime<Utc>,
    pub finished: Option<DateTime<Utc>>,
    pub status: RunStatus,
    #[serde(default)]
    pub phase: Option<Phase>,
    /// Sum of the games' estimates.
    #[serde(default)]
    pub bytes_to_download: u64,
    /// Sum of the games' downloaded bytes.
    #[serde(default)]
    pub bytes_downloaded: u64,
    #[serde(default, skip_deserializing)]
    pub rate_bps: Option<u64>,
    #[serde(default, skip_deserializing)]
    pub eta_secs: Option<u64>,
    pub games: Vec<GameRun>,
}

impl UpdateRun {
    pub fn new(platform: &str) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            platform: platform.to_string(),
            started: Utc::now(),
            finished: None,
            status: RunStatus::Running,
            phase: Some(Phase::Listing),
            bytes_to_download: 0,
            bytes_downloaded: 0,
            rate_bps: None,
            eta_secs: None,
            games: Vec::new(),
        }
    }

    /// Recomputes the run totals from its games.
    pub fn sum_games(&mut self) {
        self.bytes_to_download = self.games.iter().filter_map(|g| g.bytes_to_download).sum();
        self.bytes_downloaded = self.games.iter().map(|g| g.report.bytes_downloaded).sum();
    }

    /// Marks the run finished: games that didn't finish become cancelled, and the
    /// status follows the games' errors. A cancel requested after every game
    /// finished doesn't count.
    pub fn finish(&mut self, cancel_requested: bool) {
        let cancelled = cancel_requested
            && self
                .games
                .iter()
                .any(|g| !matches!(g.status, GameStatus::Done | GameStatus::Failed));
        self.stop();
        self.sum_games();
        self.status = if cancelled {
            RunStatus::Cancelled
        } else if self.games.iter().any(|g| g.status == GameStatus::Failed) {
            RunStatus::Error
        } else {
            RunStatus::Ok
        };
        self.finished = Some(Utc::now());
    }

    /// Ends the live state: games that didn't finish become cancelled (stopped),
    /// and phase, rates and ETAs are cleared.
    fn stop(&mut self) {
        for g in &mut self.games {
            if !matches!(g.status, GameStatus::Done | GameStatus::Failed) {
                g.status = GameStatus::Cancelled;
            }
            g.rate_bps = None;
            g.eta_secs = None;
        }
        self.phase = None;
        self.rate_bps = None;
        self.eta_secs = None;
    }

    /// A stored run left `running` by a process that is gone.
    pub fn mark_incomplete(&mut self) {
        self.stop();
        self.status = RunStatus::Incomplete;
    }
}

/// The pre-history `{"last_run": ...}` document.
#[derive(Deserialize)]
pub struct LegacyRun {
    started: DateTime<Utc>,
    finished: Option<DateTime<Utc>>,
    games: Vec<UpdateReport>,
}

impl LegacyRun {
    pub fn into_run(self, platform: &str) -> UpdateRun {
        let games: Vec<GameRun> = self
            .games
            .into_iter()
            .map(|report| {
                let status = if report.error.is_some() {
                    GameStatus::Failed
                } else {
                    GameStatus::Done
                };
                let files = report.files;
                GameRun {
                    report,
                    status,
                    bytes_to_download: None,
                    objects: files,
                    objects_done: files,
                    rate_bps: None,
                    eta_secs: None,
                }
            })
            .collect();
        let failed = games.iter().any(|g| g.status == GameStatus::Failed);
        let mut run = UpdateRun {
            id: format!("legacy-{}", self.started.timestamp()),
            started: self.started,
            finished: self.finished,
            status: match (self.finished, failed) {
                (None, _) => RunStatus::Incomplete,
                (Some(_), true) => RunStatus::Error,
                (Some(_), false) => RunStatus::Ok,
            },
            phase: None,
            games,
            ..UpdateRun::new(platform)
        };
        run.sum_games();
        run
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn game(status: GameStatus, error: Option<&str>) -> GameRun {
        let mut g = GameRun::pending("g", "G".into());
        g.status = status;
        g.report.error = error.map(String::from);
        g
    }

    #[test]
    fn finish_derives_status_and_cancels_unrun_games() {
        let mut run = UpdateRun::new("steam");
        run.games = vec![game(GameStatus::Done, None), game(GameStatus::Queued, None)];
        run.finish(false);
        assert_eq!(run.status, RunStatus::Ok);
        assert_eq!(run.games[1].status, GameStatus::Cancelled);
        assert!(run.finished.is_some() && run.phase.is_none());

        let mut run = UpdateRun::new("steam");
        run.games = vec![game(GameStatus::Failed, Some("x"))];
        run.finish(false);
        assert_eq!(run.status, RunStatus::Error);

        let mut run = UpdateRun::new("steam");
        run.games = vec![game(GameStatus::Done, None), game(GameStatus::Queued, None)];
        run.finish(true);
        assert_eq!(run.status, RunStatus::Cancelled);

        // Cancel requested once every game had finished: not a cancellation.
        let mut run = UpdateRun::new("steam");
        run.games = vec![game(GameStatus::Done, None)];
        run.finish(true);
        assert_eq!(run.status, RunStatus::Ok);
    }

    #[test]
    fn incomplete_runs_stop_unfinished_games() {
        let mut run = UpdateRun::new("steam");
        run.phase = Some(Phase::Downloading);
        run.games = vec![
            game(GameStatus::Done, None),
            game(GameStatus::Downloading, None),
        ];
        run.mark_incomplete();
        assert_eq!(run.status, RunStatus::Incomplete);
        assert_eq!(run.phase, None);
        assert_eq!(run.finished, None);
        assert_eq!(run.games[0].status, GameStatus::Done);
        assert_eq!(run.games[1].status, GameStatus::Cancelled);
    }

    #[test]
    fn remaining_bytes_and_totals() {
        let mut g = game(GameStatus::Downloading, None);
        g.bytes_to_download = Some(100);
        g.report.bytes_downloaded = 30;
        assert_eq!(g.bytes_remaining(), 70);
        g.report.bytes_downloaded = 130;
        assert_eq!(g.bytes_remaining(), 0);
        g.status = GameStatus::Done;
        g.report.bytes_downloaded = 30;
        assert_eq!(g.bytes_remaining(), 0);

        let mut run = UpdateRun::new("epic");
        run.games = vec![g, game(GameStatus::Pending, None)];
        run.sum_games();
        assert_eq!((run.bytes_to_download, run.bytes_downloaded), (100, 30));
    }

    #[test]
    fn json_shape_and_live_fields_not_read_back() {
        let mut run = UpdateRun::new("steam");
        run.rate_bps = Some(5);
        let mut g = game(GameStatus::Downloading, None);
        g.eta_secs = Some(9);
        run.games = vec![g];
        let v = serde_json::to_value(&run).unwrap();
        assert_eq!(v["status"], "running");
        assert_eq!(v["phase"], "listing");
        assert_eq!(v["rate_bps"], 5);
        assert_eq!(v["games"][0]["status"], "downloading");
        assert_eq!(v["games"][0]["game_id"], "g");
        assert_eq!(v["games"][0]["eta_secs"], 9);
        let back: UpdateRun = serde_json::from_value(v).unwrap();
        assert_eq!(back.rate_bps, None);
        assert_eq!(back.games[0].eta_secs, None);
        assert_eq!(back.id, run.id);
    }

    #[test]
    fn legacy_runs_convert() {
        let v = serde_json::json!({
            "started": "2026-11-01T00:00:00Z", "finished": "2026-11-01T01:00:00Z",
            "games": [{"game_id": "1", "name": "A", "previous_version": null,
                       "current_version": "2", "updated": true, "bytes_downloaded": 5,
                       "bytes_already_cached": 0, "files": 3, "error": "boom"}]
        });
        let run = serde_json::from_value::<LegacyRun>(v.clone())
            .unwrap()
            .into_run("steam");
        assert_eq!(run.status, RunStatus::Error);
        assert_eq!(run.games[0].status, GameStatus::Failed);
        assert_eq!(run.games[0].report.name, "A");
        assert_eq!(run.games[0].objects_done, 3);
        assert_eq!(run.bytes_downloaded, 5);
        assert!(run.id.starts_with("legacy-"));

        let mut unfinished = v;
        unfinished["finished"] = serde_json::Value::Null;
        let run = serde_json::from_value::<LegacyRun>(unfinished)
            .unwrap()
            .into_run("steam");
        assert_eq!(run.status, RunStatus::Incomplete);
    }
}
