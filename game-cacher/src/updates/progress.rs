//! Live state of an active run: the record being built plus download rates
//! (sliding window) for the current game and the whole run.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use tokio_util::sync::CancellationToken;

use super::model::{GameStatus, UpdateRun};
use crate::platforms::plan::Progress;

/// Rates average the bytes of roughly this long.
const WINDOW: Duration = Duration::from_secs(20);
/// At most one sample per this interval.
const SAMPLE_EVERY: Duration = Duration::from_secs(1);

/// Download rate over a sliding window of `(time, cumulative bytes)` samples.
#[derive(Debug, Default)]
pub struct RateMeter {
    samples: VecDeque<(Instant, u64)>,
    total: u64,
}

impl RateMeter {
    /// Starts measuring from `now` (forgets earlier samples).
    pub fn restart(&mut self, now: Instant) {
        self.samples.clear();
        self.samples.push_back((now, self.total));
    }

    pub fn add(&mut self, now: Instant, bytes: u64) {
        self.total += bytes;
        let due = self
            .samples
            .back()
            .is_none_or(|(t, _)| now.duration_since(*t) >= SAMPLE_EVERY);
        if due {
            self.samples.push_back((now, self.total));
        }
        while self.samples.len() > 1
            && self
                .samples
                .front()
                .is_some_and(|(t, _)| now.duration_since(*t) > WINDOW)
        {
            self.samples.pop_front();
        }
    }

    /// Bytes per second since the oldest sample; None until a second has passed.
    /// Decays while no bytes arrive.
    pub fn rate(&self, now: Instant) -> Option<u64> {
        let (t0, b0) = self.samples.front()?;
        let dt = now.duration_since(*t0).as_secs_f64();
        (dt >= SAMPLE_EVERY.as_secs_f64()).then(|| ((self.total - b0) as f64 / dt) as u64)
    }
}

/// Seconds to fetch `remaining` bytes at `rate`, when the rate is known and non-zero.
pub fn eta(remaining: u64, rate: Option<u64>) -> Option<u64> {
    rate.filter(|r| *r > 0).map(|r| remaining.div_ceil(r))
}

struct Live {
    run: UpdateRun,
    /// Index of the downloading game.
    current: Option<usize>,
    run_rate: RateMeter,
    game_rate: RateMeter,
}

/// An active run, shared by the runner, the API and the snapshot task.
pub struct ActiveRun {
    pub cancel: CancellationToken,
    live: Mutex<Live>,
}

impl ActiveRun {
    pub fn new(run: UpdateRun) -> Self {
        Self {
            cancel: CancellationToken::new(),
            live: Mutex::new(Live {
                run,
                current: None,
                run_rate: RateMeter::default(),
                game_rate: RateMeter::default(),
            }),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Live> {
        self.live.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Applies `f` to the record.
    pub fn update<T>(&self, f: impl FnOnce(&mut UpdateRun) -> T) -> T {
        f(&mut self.lock().run)
    }

    /// Starts the download phase's rate measurement.
    pub fn start_downloads(&self, now: Instant) {
        self.lock().run_rate.restart(now);
    }

    /// Marks game `i` as the one downloading.
    pub fn start_game(&self, i: usize, now: Instant) {
        let mut live = self.lock();
        live.current = Some(i);
        live.game_rate.restart(now);
        if let Some(g) = live.run.games.get_mut(i) {
            g.status = GameStatus::Downloading;
        }
    }

    /// Clears the current game (its record is final).
    pub fn end_game(&self) {
        self.lock().current = None;
    }

    fn on_current(&self, f: impl FnOnce(&mut Live, usize)) {
        let mut live = self.lock();
        if let Some(i) = live.current.filter(|i| *i < live.run.games.len()) {
            f(&mut live, i);
        }
    }

    /// The record with run totals, rates and ETAs filled in as of `now`.
    pub fn view(&self, now: Instant) -> UpdateRun {
        let live = self.lock();
        let mut run = live.run.clone();
        run.sum_games();
        if let Some(i) = live.current {
            if let Some(g) = run.games.get_mut(i) {
                g.rate_bps = live.game_rate.rate(now);
                g.eta_secs = eta(g.bytes_remaining(), g.rate_bps);
            }
            run.rate_bps = live.run_rate.rate(now);
            let remaining = run.games.iter().map(|g| g.bytes_remaining()).sum();
            run.eta_secs = eta(remaining, run.rate_bps);
        }
        run
    }
}

impl Progress for ActiveRun {
    fn downloaded(&self, bytes: u64) {
        let now = Instant::now();
        self.on_current(|live, i| {
            live.run.games[i].report.bytes_downloaded += bytes;
            live.run_rate.add(now, bytes);
            live.game_rate.add(now, bytes);
        });
    }

    fn object_done(&self) {
        self.on_current(|live, i| live.run.games[i].objects_done += 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::updates::model::GameRun;

    #[test]
    fn rate_over_sliding_window() {
        let t0 = Instant::now();
        let at = |s: u64| t0 + Duration::from_secs(s);
        let mut m = RateMeter::default();
        assert_eq!(m.rate(t0), None);
        m.restart(t0);
        assert_eq!(m.rate(t0), None); // less than a second measured
        for s in 1..=10 {
            m.add(at(s), 100);
        }
        assert_eq!(m.rate(at(10)), Some(100));
        // Faster for the last 20s: the old samples fall out of the window.
        for s in 11..=40 {
            m.add(at(s), 300);
        }
        assert_eq!(m.rate(at(40)), Some(300));
        // Stalled: the rate decays.
        assert!(m.rate(at(60)).unwrap() < 200);
        m.restart(at(60));
        assert_eq!(m.rate(at(60)), None);
    }

    #[test]
    fn eta_needs_a_rate() {
        assert_eq!(eta(1000, Some(300)), Some(4));
        assert_eq!(eta(1000, Some(0)), None);
        assert_eq!(eta(1000, None), None);
        assert_eq!(eta(0, Some(5)), Some(0));
    }

    #[test]
    fn progress_goes_to_the_current_game() {
        let mut run = UpdateRun::new("steam");
        let mut a = GameRun::pending("a", "A".into());
        a.status = GameStatus::Queued;
        a.bytes_to_download = Some(1000);
        let mut b = a.clone();
        b.report.game_id = "b".into();
        run.games = vec![a, b];
        let active = ActiveRun::new(run);
        active.downloaded(5); // no current game: ignored
        let t0 = Instant::now() - Duration::from_secs(5);
        active.start_downloads(t0);
        active.start_game(0, t0);
        active.downloaded(400);
        active.object_done();
        let v = active.view(Instant::now());
        assert_eq!(v.games[0].status, GameStatus::Downloading);
        assert_eq!(v.games[0].report.bytes_downloaded, 400);
        assert_eq!(v.games[0].objects_done, 1);
        assert_eq!(v.bytes_downloaded, 400);
        assert_eq!(v.bytes_to_download, 2000);
        let rate = v.games[0].rate_bps.unwrap();
        assert!((70..=90).contains(&rate), "{rate}");
        // Game: 600 left; run: 600 + 1000 queued.
        assert_eq!(v.games[0].eta_secs, eta(600, Some(rate)));
        assert_eq!(v.eta_secs, eta(1600, v.rate_bps));
        active.end_game();
        let v = active.view(Instant::now());
        assert_eq!((v.rate_bps, v.games[0].rate_bps), (None, None));
    }
}
