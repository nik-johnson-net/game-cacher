//! One managed game's update as plan → download → commit. A platform's
//! `plan_game` lists the objects the game needs (`GamePlan`); `execute` warms
//! them through `Prefill`; the platform's `commit` records the new version
//! once the `Outcome` allows it.

use std::any::Any;

use anyhow::Result;
use futures::stream::{self, Stream, StreamExt};
use tokio_util::sync::CancellationToken;

use super::{Prefill, UpdateReport, WarmResult};

/// One object to warm, relative to `GamePlan::host`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedObject {
    pub path: String,
    /// Expected download size when the platform knows it (manifest data).
    pub size: Option<u64>,
    /// Index into `GamePlan::groups`.
    pub group: u16,
    /// Found complete in the cache by `estimate`: `execute` skips it.
    pub cached: bool,
}

/// Objects whose failures are reported together (e.g. one Steam depot).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectGroup {
    /// Prefix for the group's error message; empty for none.
    pub label: String,
    /// Failures in a non-required group are logged but neither fail the game
    /// nor block `commit` (Blizzard loose files).
    pub required: bool,
}

pub struct GamePlan {
    /// Cache identifier the objects are stored under (the platform id).
    pub identifier: &'static str,
    pub game_id: String,
    pub name: String,
    pub previous_version: Option<String>,
    pub current_version: Option<String>,
    pub version_changed: bool,
    pub host: String,
    pub objects: Vec<PlannedObject>,
    pub groups: Vec<ObjectGroup>,
    /// Objects warmed in parallel.
    pub concurrency: usize,
    /// A planning problem that doesn't stop the download (e.g. one Steam depot's
    /// manifest failed): reported as the game's error and blocks a full commit.
    pub error: Option<String>,
    /// Platform data `commit` needs (downcast with `payload()`).
    pub payload: Box<dyn Any + Send + Sync>,
}

impl GamePlan {
    /// An empty plan; declare object groups with `group`.
    pub fn new(identifier: &'static str, game_id: &str, name: &str) -> Self {
        Self {
            identifier,
            game_id: game_id.to_string(),
            name: name.to_string(),
            previous_version: None,
            current_version: None,
            version_changed: false,
            host: String::new(),
            objects: Vec::new(),
            groups: Vec::new(),
            concurrency: 8,
            error: None,
            payload: Box::new(()),
        }
    }

    /// Declares an object group; returns its index for `add`.
    pub fn group(&mut self, label: impl Into<String>, required: bool) -> u16 {
        self.groups.push(ObjectGroup {
            label: label.into(),
            required,
        });
        (self.groups.len() - 1) as u16
    }

    /// Adds an object to a group declared with `group`.
    pub fn add(&mut self, path: String, size: Option<u64>, group: u16) {
        assert!(
            (group as usize) < self.groups.len(),
            "object group {group} not declared"
        );
        self.objects.push(PlannedObject {
            path,
            size,
            group,
            cached: false,
        });
    }

    pub fn payload<T: 'static>(&self) -> Option<&T> {
        self.payload.downcast_ref()
    }
}

/// Live progress of `execute`.
pub trait Progress: Send + Sync {
    /// Bytes fetched upstream, reported as they arrive.
    fn downloaded(&self, _bytes: u64) {}
    /// One object finished (warmed or failed).
    fn object_done(&self) {}
}

pub struct NoProgress;

impl Progress for NoProgress {}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Outcome {
    pub bytes_downloaded: u64,
    pub bytes_already_cached: u64,
    /// Objects warmed successfully.
    pub files: u64,
    /// Per group: summary of its failures, None when none failed.
    pub group_errors: Vec<Option<String>>,
    /// Cancellation skipped or dropped at least one object.
    pub cancelled: bool,
}

impl Outcome {
    /// Folds in what `estimate` found: the objects it found cached (skipped by
    /// `execute`) count as warmed and already cached, and the bytes it fetched
    /// while sizing move from "already cached" to "downloaded" (`execute`
    /// found them cached, but this run downloaded them).
    pub fn add_estimate(&mut self, est: &Estimate) {
        self.files += est.objects_cached;
        self.bytes_already_cached += est.bytes_already_cached;
        self.bytes_downloaded += est.bytes_fetched;
        self.bytes_already_cached = self.bytes_already_cached.saturating_sub(est.bytes_fetched);
    }

    /// True if every object of group `g` that was attempted succeeded and the run wasn't cancelled.
    pub fn group_ok(&self, g: u16) -> bool {
        !self.cancelled
            && self
                .group_errors
                .get(g as usize)
                .is_none_or(Option::is_none)
    }

    /// No planning error, no failed required group, not cancelled: the platform may
    /// record the new version.
    pub fn succeeded(&self, plan: &GamePlan) -> bool {
        plan.error.is_none() && !self.cancelled && self.required_error(plan).is_none()
    }

    fn required_error(&self, plan: &GamePlan) -> Option<&String> {
        plan.groups
            .iter()
            .zip(&self.group_errors)
            .find_map(|(g, e)| e.as_ref().filter(|_| g.required))
    }

    pub fn report(&self, plan: &GamePlan) -> UpdateReport {
        let error = plan
            .error
            .clone()
            .or_else(|| self.required_error(plan).cloned())
            .or_else(|| self.cancelled.then(|| "cancelled".to_string()));
        UpdateReport {
            game_id: plan.game_id.clone(),
            name: plan.name.clone(),
            previous_version: plan.previous_version.clone(),
            current_version: plan.current_version.clone(),
            // A version change, or bytes fetched while verifying (evicted or
            // purged content), counts as an update.
            updated: plan.version_changed || self.bytes_downloaded > 0,
            bytes_downloaded: self.bytes_downloaded,
            bytes_already_cached: self.bytes_already_cached,
            files: self.files,
            error,
        }
    }
}

/// Download estimate for a plan, before `execute`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Estimate {
    /// Bytes not cached when the estimate started (includes `bytes_fetched`).
    pub bytes_to_download: u64,
    /// Bytes fetched while sizing objects with no listed size (their first slice);
    /// `execute` later reports them as already cached.
    pub bytes_fetched: u64,
    /// Objects found complete in the cache (marked `cached`), and their bytes.
    pub objects_cached: u64,
    pub bytes_already_cached: u64,
}

/// Concurrent size lookups for objects with no listed size.
const SIZE_CONCURRENCY: usize = 16;

/// Estimates what `plan` still has to download. Objects the in-memory index
/// holds in full are marked `cached` (and marked used) so `execute` skips them.
/// Others count their listed size minus the bytes stored; those with no listed
/// size are sized through `Prefill::object_size` (stored metadata, else a
/// first-slice fetch upstream); objects that can't be sized count as nothing
/// to download.
pub async fn estimate(
    prefill: &dyn Prefill,
    plan: &mut GamePlan,
    cancel: &CancellationToken,
) -> Estimate {
    let mut est = Estimate::default();
    let mut unlisted = Vec::new();
    let id = plan.identifier;
    for (i, o) in plan.objects.iter_mut().enumerate() {
        if let Some(total) = prefill.touch_if_cached(id, &o.path) {
            o.cached = true;
            est.objects_cached += 1;
            est.bytes_already_cached += total;
            continue;
        }
        let stored = prefill.cached_bytes(id, &o.path).unwrap_or(0);
        match o.size {
            Some(size) => est.bytes_to_download += size.saturating_sub(stored),
            None => unlisted.push((i, stored)),
        }
    }
    let (missing, fetched) = size_unlisted(prefill, plan, unlisted, cancel).await;
    est.bytes_to_download += missing;
    est.bytes_fetched += fetched;
    est
}

/// Sizes objects with no listed size, given with the bytes stored for each:
/// (bytes missing, bytes fetched while sizing).
async fn size_unlisted(
    prefill: &dyn Prefill,
    plan: &GamePlan,
    objects: Vec<(usize, u64)>,
    cancel: &CancellationToken,
) -> (u64, u64) {
    let mut sizes = stream::iter(objects)
        .map(|(i, before)| async move {
            let o = &plan.objects[i];
            if cancel.is_cancelled() {
                return None;
            }
            match prefill
                .object_size(plan.identifier, &plan.host, &o.path)
                .await
            {
                Ok(Some(total)) => {
                    let after = prefill.cached_bytes(plan.identifier, &o.path).unwrap_or(0);
                    Some((total.saturating_sub(before), after.saturating_sub(before)))
                }
                Ok(None) => None,
                Err(e) => {
                    log::debug!(
                        "{} {}: sizing {}: {e:#}",
                        plan.identifier,
                        plan.game_id,
                        o.path
                    );
                    None
                }
            }
        })
        .buffer_unordered(SIZE_CONCURRENCY);
    let (mut missing, mut fetched) = (0, 0);
    while let Some(r) = sizes.next().await {
        if let Some((m, f)) = r {
            missing += m;
            fetched += f;
        }
    }
    (missing, fetched)
}

#[derive(Debug, Clone, Default)]
struct Tally {
    total: u64,
    failed: u64,
    first: Option<String>,
}

impl Tally {
    fn error(&self, g: &ObjectGroup) -> Option<String> {
        let first = self.first.as_ref()?;
        let prefix = if g.label.is_empty() {
            String::new()
        } else {
            format!("{}: ", g.label)
        };
        Some(if self.total == 1 {
            format!("{prefix}{first}")
        } else {
            format!(
                "{prefix}{} of {} objects failed: {first}",
                self.failed, self.total
            )
        })
    }
}

/// Warms every object of `plan` not marked `cached`, `plan.concurrency` at a
/// time (the outcome covers only those; see `Outcome::add_estimate`). Object
/// failures are tallied per group, never fatal. On cancellation no new object
/// starts and in-flight warms are dropped.
pub async fn execute(
    prefill: &dyn Prefill,
    plan: &GamePlan,
    progress: &dyn Progress,
    cancel: &CancellationToken,
) -> Outcome {
    let mut tally = vec![Tally::default(); plan.groups.len()];
    for o in &plan.objects {
        tally[o.group as usize].total += 1;
    }
    let mut out = Outcome::default();
    let mut results = warm_all(prefill, plan, progress, cancel);
    while let Some((o, r)) = results.next().await {
        let Some(r) = r else {
            out.cancelled = true;
            continue;
        };
        progress.object_done();
        match r {
            Ok(w) => {
                out.files += 1;
                out.bytes_downloaded += w.bytes_downloaded;
                out.bytes_already_cached += w.bytes_already_cached;
            }
            Err(e) => {
                log::warn!("{} {}: {}: {e:#}", plan.identifier, plan.game_id, o.path);
                let t = &mut tally[o.group as usize];
                t.failed += 1;
                t.first.get_or_insert_with(|| format!("{}: {e:#}", o.path));
            }
        }
    }
    out.group_errors = plan
        .groups
        .iter()
        .zip(&tally)
        .map(|(g, t)| t.error(g))
        .collect();
    out
}

/// Each uncached object's warm result, or None when cancellation skipped or dropped it.
fn warm_all<'a>(
    prefill: &'a dyn Prefill,
    plan: &'a GamePlan,
    progress: &'a dyn Progress,
    cancel: &'a CancellationToken,
) -> impl Stream<Item = (&'a PlannedObject, Option<Result<WarmResult>>)> + Send + 'a {
    // Indices rather than references: closures over `&PlannedObject` trip the
    // compiler's higher-ranked lifetime checks inside `Send` futures.
    stream::iter((0..plan.objects.len()).filter(move |&i| !plan.objects[i].cached))
        .map(move |i| async move {
            let o = &plan.objects[i];
            if cancel.is_cancelled() {
                return (o, None);
            }
            let on_bytes = |n: u64| progress.downloaded(n);
            let warm = prefill.warm_observed(plan.identifier, &plan.host, &o.path, &on_bytes);
            let r = tokio::select! {
                biased;
                _ = cancel.cancelled() => None,
                r = warm => Some(r),
            };
            (o, r)
        })
        .buffer_unordered(plan.concurrency.max(1))
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU64, Ordering};

    use anyhow::bail;
    use async_trait::async_trait;
    use bytes::Bytes;

    use super::*;

    #[derive(Default)]
    struct MockPrefill {
        warmed: Mutex<Vec<String>>,
        /// Cancels this token on the first warm.
        cancel_on_warm: Option<CancellationToken>,
        /// path → stored bytes (the index).
        cached: Mutex<std::collections::HashMap<String, u64>>,
        /// path → full size of objects the index holds in full.
        complete: std::collections::HashMap<String, u64>,
        /// Complete objects marked used, in order.
        touched: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl Prefill for MockPrefill {
        async fn warm(&self, id: &str, host: &str, path: &str) -> Result<WarmResult> {
            assert_eq!((id, host), ("test", "h"));
            self.warmed.lock().unwrap().push(path.into());
            if let Some(c) = &self.cancel_on_warm {
                c.cancel();
                std::future::pending::<()>().await;
            }
            if path.contains("bad") {
                bail!("boom");
            }
            Ok(WarmResult {
                bytes_downloaded: 10,
                bytes_already_cached: 1,
            })
        }
        async fn get(&self, _: &str, _: &str, _: &str) -> Result<Bytes> {
            bail!("unused")
        }
        async fn get_range(&self, _: &str, _: &str, _: &str, _: u64, _: u64) -> Result<Bytes> {
            bail!("unused")
        }
        fn cached_bytes(&self, _: &str, path: &str) -> Option<u64> {
            self.cached.lock().unwrap().get(path).copied()
        }
        fn touch_if_cached(&self, id: &str, path: &str) -> Option<u64> {
            assert_eq!(id, "test");
            let total = self.complete.get(path).copied()?;
            self.touched.lock().unwrap().push(path.into());
            Some(total)
        }
        /// "/sized<N>" objects are N bytes; sizing stores a 10-byte first slice.
        async fn object_size(&self, _: &str, _: &str, path: &str) -> Result<Option<u64>> {
            if path.contains("bad") {
                bail!("upstream 503");
            }
            let Some(n) = path.strip_prefix("/sized") else {
                return Ok(None);
            };
            let mut cached = self.cached.lock().unwrap();
            let stored = cached.entry(path.to_string()).or_insert(0);
            *stored = (*stored).max(10);
            Ok(Some(n.parse()?))
        }
    }

    #[derive(Default)]
    struct Count(AtomicU64, AtomicU64);

    impl Progress for Count {
        fn downloaded(&self, bytes: u64) {
            self.0.fetch_add(bytes, Ordering::SeqCst);
        }
        fn object_done(&self) {
            self.1.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn plan(paths: &[(&str, u16)]) -> GamePlan {
        let mut p = GamePlan::new("test", "g", "Game");
        p.host = "h".into();
        assert_eq!(p.group("depot 1", true), 0);
        assert_eq!(p.group("", false), 1);
        for (path, g) in paths {
            p.add(path.to_string(), None, *g);
        }
        p
    }

    #[tokio::test]
    async fn warms_everything_and_reports_progress() {
        let prefill = MockPrefill::default();
        let p = plan(&[("/a", 0), ("/b", 0), ("/c", 1)]);
        let count = Count::default();
        let out = execute(&prefill, &p, &count, &CancellationToken::new()).await;
        assert_eq!(
            (out.files, out.bytes_downloaded, out.bytes_already_cached),
            (3, 30, 3)
        );
        assert_eq!(out.group_errors, vec![None, None]);
        assert!(out.succeeded(&p));
        assert_eq!(count.0.load(Ordering::SeqCst), 30);
        assert_eq!(count.1.load(Ordering::SeqCst), 3);
        let r = out.report(&p);
        assert!(r.updated && r.error.is_none());
        assert_eq!((r.game_id.as_str(), r.name.as_str()), ("g", "Game"));
    }

    #[tokio::test]
    async fn failures_are_tallied_per_group() {
        let prefill = MockPrefill::default();
        let p = plan(&[("/a", 0), ("/bad1", 0), ("/bad2", 1)]);
        let out = execute(&prefill, &p, &NoProgress, &CancellationToken::new()).await;
        assert_eq!(out.files, 1);
        assert_eq!(
            out.group_errors[0].as_deref(),
            Some("depot 1: 1 of 2 objects failed: /bad1: boom")
        );
        assert_eq!(out.group_errors[1].as_deref(), Some("/bad2: boom"));
        assert!(!out.group_ok(0) && !out.group_ok(1));
        assert!(!out.succeeded(&p));
        assert!(out.report(&p).error.unwrap().starts_with("depot 1:"));
    }

    #[tokio::test]
    async fn optional_group_failures_do_not_fail_the_game() {
        let prefill = MockPrefill::default();
        let mut p = plan(&[("/a", 0), ("/bad", 1)]);
        p.version_changed = true;
        let out = execute(&prefill, &p, &NoProgress, &CancellationToken::new()).await;
        assert!(out.succeeded(&p));
        assert_eq!(out.report(&p).error, None);

        p.error = Some("loose files: x".into());
        assert!(!out.succeeded(&p));
        assert_eq!(out.report(&p).error.as_deref(), Some("loose files: x"));
    }

    #[tokio::test]
    async fn cancellation_stops_new_and_in_flight_warms() {
        let cancel = CancellationToken::new();
        let prefill = MockPrefill {
            cancel_on_warm: Some(cancel.clone()),
            ..Default::default()
        };
        let mut p = plan(&[("/a", 0), ("/b", 0), ("/c", 0)]);
        p.concurrency = 1;
        let out = execute(&prefill, &p, &NoProgress, &cancel).await;
        assert!(out.cancelled);
        assert_eq!(out.files, 0);
        assert_eq!(*prefill.warmed.lock().unwrap(), vec!["/a".to_string()]);
        assert!(!out.group_ok(0) && !out.succeeded(&p));
        let r = out.report(&p);
        assert_eq!(r.error.as_deref(), Some("cancelled"));
        assert!(!r.updated);
    }

    #[tokio::test]
    async fn estimate_subtracts_cached_bytes_and_sizes_unlisted_objects() {
        let prefill = MockPrefill::default();
        prefill
            .cached
            .lock()
            .unwrap()
            .extend([("/listed".to_string(), 30), ("/sized100".to_string(), 40)]);
        let mut p = plan(&[]);
        p.add("/listed".into(), Some(100), 0); // 70 missing
        p.add("/full".into(), Some(5), 0); // 5 missing
        p.add("/sized100".into(), None, 0); // 60 missing, nothing fetched (already 40)
        p.add("/sized50".into(), None, 0); // 50 missing, 10 fetched
        p.add("/unknown".into(), None, 0); // can't be sized
        p.add("/bad".into(), None, 0); // sizing fails
        let est = estimate(&prefill, &mut p, &CancellationToken::new()).await;
        assert_eq!(
            est,
            Estimate {
                bytes_to_download: 70 + 5 + 60 + 50,
                bytes_fetched: 10,
                ..Default::default()
            }
        );
        assert!(p.objects.iter().all(|o| !o.cached));

        let cancel = CancellationToken::new();
        cancel.cancel();
        let est = estimate(&prefill, &mut plan(&[("/sized70", 0)]), &cancel).await;
        assert_eq!(est, Estimate::default());
    }

    #[tokio::test]
    async fn estimate_marks_complete_objects_cached() {
        let prefill = MockPrefill {
            // "/sized50" has no listed size: complete, so it isn't probed.
            complete: [("/whole", 100), ("/sized50", 50)]
                .map(|(p, n)| (p.to_string(), n))
                .into(),
            ..Default::default()
        };
        prefill.cached.lock().unwrap().insert("/part".into(), 30);
        let mut p = plan(&[]);
        p.add("/whole".into(), Some(90), 0); // listed size is ignored when complete
        p.add("/part".into(), Some(100), 0);
        p.add("/sized50".into(), None, 1);
        let est = estimate(&prefill, &mut p, &CancellationToken::new()).await;
        assert_eq!(
            est,
            Estimate {
                bytes_to_download: 70,
                bytes_fetched: 0,
                objects_cached: 2,
                bytes_already_cached: 150,
            }
        );
        let cached: Vec<bool> = p.objects.iter().map(|o| o.cached).collect();
        assert_eq!(cached, vec![true, false, true]);
        assert_eq!(*prefill.touched.lock().unwrap(), vec!["/whole", "/sized50"]);
        assert_eq!(
            prefill.cached.lock().unwrap().get("/sized50"),
            None,
            "not probed"
        );
    }

    #[tokio::test]
    async fn execute_skips_cached_objects() {
        let prefill = MockPrefill::default();
        let mut p = plan(&[("/a", 0), ("/b", 0), ("/bad", 1)]);
        p.objects[1].cached = true;
        p.objects[2].cached = true;
        let count = Count::default();
        let mut out = execute(&prefill, &p, &count, &CancellationToken::new()).await;
        assert_eq!(*prefill.warmed.lock().unwrap(), vec!["/a".to_string()]);
        assert_eq!(count.1.load(Ordering::SeqCst), 1);
        assert_eq!(
            (out.files, out.bytes_downloaded, out.bytes_already_cached),
            (1, 10, 1)
        );
        assert!(out.succeeded(&p), "skipped objects don't fail their group");

        out.add_estimate(&Estimate {
            bytes_to_download: 10,
            bytes_fetched: 0,
            objects_cached: 2,
            bytes_already_cached: 500,
        });
        assert_eq!(
            (out.files, out.bytes_downloaded, out.bytes_already_cached),
            (3, 10, 501)
        );
        let r = out.report(&p);
        assert_eq!((r.files, r.bytes_already_cached), (3, 501));
    }

    #[test]
    fn estimate_folds_into_the_outcome() {
        let mut out = Outcome {
            bytes_downloaded: 5,
            bytes_already_cached: 8,
            files: 1,
            ..Default::default()
        };
        // Fetched while sizing: execute found those bytes cached, but this run
        // downloaded them.
        out.add_estimate(&Estimate {
            bytes_fetched: 10,
            ..Default::default()
        });
        assert_eq!((out.bytes_downloaded, out.bytes_already_cached), (15, 0));
        assert_eq!(out.files, 1);
    }

    #[tokio::test]
    async fn cancel_after_the_last_object_is_not_a_cancellation() {
        /// Cancels once the last object is done, before `execute` returns.
        struct CancelOnDone(CancellationToken);
        impl Progress for CancelOnDone {
            fn object_done(&self) {
                self.0.cancel();
            }
        }
        let prefill = MockPrefill::default();
        let p = plan(&[("/a", 0)]);
        let cancel = CancellationToken::new();
        let out = execute(&prefill, &p, &CancelOnDone(cancel.clone()), &cancel).await;
        assert!(cancel.is_cancelled());
        assert!(!out.cancelled && out.succeeded(&p));
    }

    #[test]
    #[should_panic(expected = "not declared")]
    fn objects_need_a_declared_group() {
        GamePlan::new("test", "g", "Game").add("/a".into(), None, 0);
    }

    #[test]
    fn payload_downcasts() {
        let mut p = GamePlan::new("test", "g", "Game");
        p.payload = Box::new(42u32);
        assert_eq!(p.payload::<u32>(), Some(&42));
        assert_eq!(p.payload::<String>(), None);
    }
}
