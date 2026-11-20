//! Cache engine: object metadata, sliced upstream fetches with coalescing, and the
//! in-process `Prefill` implementation. The HTTP handler lives in `proxy.rs`.
use std::sync::Arc;

use anyhow::{Result, anyhow, bail};
use async_trait::async_trait;
use bytes::{Buf, Bytes, BytesMut};
use dashmap::DashMap;
use futures::stream::{BoxStream, StreamExt};
use http::header::{
    CONTENT_RANGE, CONTENT_TYPE, ETAG, HeaderMap, LAST_MODIFIED, RANGE, USER_AGENT,
};
use http::{HeaderValue, StatusCode};
use tokio::sync::OnceCell;

use super::LruIndex;
use super::key::ObjectKey;
use super::meta::{MAX_OBJECT_SIZE, Meta};
use super::range::parse_content_range;
use crate::metrics::Metrics;
use crate::platforms::{CachePolicy, Platforms, Prefill, WarmResult};
use crate::settings::SettingsStore;
use crate::storage::DynStorage;
use crate::upstream::{LoopDetected, PROCESSED_BY_HEADER, Upstream};

/// Upper bound for `Prefill::get` / `get_range` bodies held in memory.
pub const MAX_PREFILL_BODY: u64 = 512 * 1024 * 1024;
/// The in-memory meta cache is cleared when it grows beyond this many entries.
const META_CACHE_MAX: usize = 10_000;
/// Parallel slice fetches per `Prefill::warm` call.
const WARM_PARALLELISM: usize = 4;

pub struct CacheEngine {
    pub(crate) storage: DynStorage,
    pub(crate) platforms: Arc<Platforms>,
    pub(crate) settings: Arc<SettingsStore>,
    pub(crate) upstream: Upstream,
    pub(crate) index: Arc<LruIndex>,
    pub(crate) metrics: Arc<Metrics>,
    slice_size: u64,
    read_ahead: usize,
    metas: DashMap<String, Arc<Meta>>,
    probes: DashMap<String, Arc<OnceCell<ProbeShared>>>,
    slices: DashMap<String, Arc<OnceCell<Result<Bytes, CacheError>>>>,
}

pub struct EngineDeps {
    pub storage: DynStorage,
    pub platforms: Arc<Platforms>,
    pub settings: Arc<SettingsStore>,
    pub upstream: Upstream,
    pub index: Arc<LruIndex>,
    pub metrics: Arc<Metrics>,
    pub slice_size: u64,
    /// Slices fetched in parallel per client download (min 1).
    pub read_ahead: usize,
}

/// Failure while serving a cached object. Cloneable so coalesced waiters share it.
#[derive(Debug, Clone, thiserror::Error)]
pub enum CacheError {
    #[error("loop detected: {0}")]
    Loop(String),
    #[error("object changed upstream: {0}")]
    Changed(String),
    #[error("upstream: {0}")]
    Upstream(String),
}

impl CacheError {
    pub(crate) fn upstream(e: impl Into<anyhow::Error>) -> Self {
        let e = e.into();
        let is_loop = e
            .chain()
            .any(|c| c.downcast_ref::<LoopDetected>().is_some());
        if is_loop {
            CacheError::Loop(format!("{e:#}"))
        } else {
            CacheError::Upstream(format!("{e:#}"))
        }
    }

    pub fn status(&self) -> StatusCode {
        match self {
            CacheError::Loop(_) => StatusCode::LOOP_DETECTED,
            _ => StatusCode::BAD_GATEWAY,
        }
    }
}

/// One cached object as requested: its key plus how to reach it upstream.
pub(crate) struct Target {
    pub key: ObjectKey,
    pub prefix: String,
    pub host: String,
    pub path_and_query: String,
    pub user_agent: Option<HeaderValue>,
}

impl Target {
    pub fn new(
        identifier: &str,
        host: &str,
        path_and_query: &str,
        user_agent: Option<HeaderValue>,
    ) -> Self {
        let key = ObjectKey::new(identifier, path_and_query);
        Self {
            prefix: key.prefix(),
            key,
            host: host.to_string(),
            path_and_query: path_and_query.to_string(),
            user_agent,
        }
    }

    pub fn identifier(&self) -> &str {
        &self.key.identifier
    }
}

/// Probe result shared with coalesced waiters: meta (+ slice 0 if just fetched),
/// or None when the leader got a response that can't be shared.
type ProbeShared = Result<Option<(Arc<Meta>, Option<Bytes>)>, CacheError>;

enum ProbeOutcome {
    Stored(Arc<Meta>, Bytes),
    Full(Arc<Meta>, reqwest::Response),
    Passthrough(reqwest::Response),
}

/// How a request for an object will be served.
pub(crate) enum Opened {
    /// Metadata known; `fresh` holds slice 0 when this request just fetched it.
    Ready {
        meta: Arc<Meta>,
        fresh: Option<Bytes>,
    },
    /// Upstream ignored Range and is sending the whole body: slice it as it streams.
    Full { meta: Arc<Meta>, body: FullBody },
    /// Not cacheable (error status, unknown length, ...): forward verbatim.
    Passthrough(reqwest::Response),
}

impl From<ProbeOutcome> for Opened {
    fn from(p: ProbeOutcome) -> Self {
        match p {
            ProbeOutcome::Stored(meta, b) => Opened::Ready {
                meta,
                fresh: Some(b),
            },
            ProbeOutcome::Full(meta, resp) => Opened::Full {
                meta,
                body: FullBody::new(resp),
            },
            ProbeOutcome::Passthrough(resp) => Opened::Passthrough(resp),
        }
    }
}

/// A whole upstream body (200 to a ranged request) cut into slices as it arrives.
pub(crate) struct FullBody {
    stream: BoxStream<'static, reqwest::Result<Bytes>>,
    buf: BytesMut,
    next: u64,
}

impl FullBody {
    fn new(resp: reqwest::Response) -> Self {
        Self {
            stream: resp.bytes_stream().boxed(),
            buf: BytesMut::new(),
            next: 0,
        }
    }

    /// Index of the slice the next call yields.
    pub fn next_index(&self) -> u64 {
        self.next
    }

    /// Reads the next complete slice, stores it, and returns it (None at the end).
    pub async fn next_slice(
        &mut self,
        engine: &CacheEngine,
        t: &Target,
        meta: &Meta,
    ) -> Result<Option<(u64, Bytes)>, CacheError> {
        if self.next >= meta.slice_count() {
            return Ok(None);
        }
        let want = meta.slice_len(self.next) as usize;
        while self.buf.len() < want {
            match self.stream.next().await {
                Some(Ok(chunk)) => self.buf.extend_from_slice(&chunk),
                Some(Err(e)) => return Err(CacheError::upstream(e)),
                None => return Err(CacheError::Upstream("upstream body ended early".into())),
            }
        }
        let slice = self.buf.split_to(want).freeze();
        let index = self.next;
        self.next += 1;
        engine.store_slice(t, index, slice.clone()).await;
        Ok(Some((index, slice)))
    }
}

/// The part of slice `index` (holding `bytes`) inside the inclusive range `[a, b]`.
pub(crate) fn trim(meta: &Meta, index: u64, bytes: Bytes, a: u64, b: u64) -> Bytes {
    let start = index * meta.slice_size;
    let len = bytes.len() as u64;
    let from = a.saturating_sub(start).min(len) as usize;
    let to = (b + 1).saturating_sub(start).min(len) as usize;
    if from >= to {
        Bytes::new()
    } else {
        bytes.slice(from..to)
    }
}

fn too_large(total: u64) -> CacheError {
    CacheError::Upstream(format!(
        "object size {total} exceeds the {MAX_OBJECT_SIZE}-byte cache limit"
    ))
}

fn header_string(headers: &HeaderMap, name: http::HeaderName) -> Option<String> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
}

/// Reads `len` bytes after skipping `skip` from a response body; errors if it is short.
async fn read_exact(resp: reqwest::Response, skip: u64, len: u64) -> Result<Bytes, CacheError> {
    let mut stream = resp.bytes_stream();
    let mut skip = skip as usize;
    let len = len as usize;
    let mut out = BytesMut::with_capacity(len);
    while out.len() < len {
        let mut chunk = match stream.next().await {
            Some(Ok(c)) => c,
            Some(Err(e)) => return Err(CacheError::upstream(e)),
            None => break,
        };
        let skipped = skip.min(chunk.len());
        chunk.advance(skipped);
        skip -= skipped;
        let take = (len - out.len()).min(chunk.len());
        out.extend_from_slice(&chunk[..take]);
    }
    if out.len() != len {
        return Err(CacheError::Upstream(format!(
            "short upstream body: {} of {len} bytes",
            out.len()
        )));
    }
    Ok(out.freeze())
}

impl CacheEngine {
    pub fn new(deps: EngineDeps) -> Arc<Self> {
        let engine = Arc::new(Self {
            storage: deps.storage,
            platforms: deps.platforms,
            settings: deps.settings,
            upstream: deps.upstream,
            index: deps.index,
            metrics: deps.metrics,
            slice_size: deps.slice_size.max(1),
            read_ahead: deps.read_ahead.max(1),
            metas: DashMap::new(),
            probes: DashMap::new(),
            slices: DashMap::new(),
        });
        // Evicted/purged objects must not be served from a stale in-memory meta.
        let weak = Arc::downgrade(&engine);
        engine.index.set_remove_hook(Arc::new(move |prefix: &str| {
            if let Some(e) = weak.upgrade() {
                e.metas.remove(prefix);
            }
        }));
        engine
    }

    /// Slices to keep in flight when streaming an object.
    pub(crate) fn read_ahead(&self) -> usize {
        self.read_ahead
    }

    /// Router for the cache HTTP listener: every request is proxied by Host header.
    pub fn router(self: &Arc<Self>) -> axum::Router {
        axum::Router::new()
            .fallback(super::proxy::handle)
            .with_state(self.clone())
    }

    /// Deletes every cached object of `identifier`; returns (deleted keys, freed bytes).
    pub async fn purge(&self, identifier: &str) -> Result<(u64, u64)> {
        let res = self
            .storage
            .delete_prefix(&format!("cache/{identifier}/"))
            .await?;
        self.index.remove_identifier(identifier);
        self.metas
            .retain(|_, m: &mut Arc<Meta>| m.identifier != identifier);
        Ok(res)
    }

    fn upstream_get(&self, t: &Target, range: Option<(u64, u64)>) -> reqwest::RequestBuilder {
        let mut rb = self
            .upstream
            .client()
            .get(self.upstream.http_url(&t.host, &t.path_and_query))
            .header(PROCESSED_BY_HEADER, self.upstream.instance_id());
        if let Some((a, b)) = range {
            rb = rb.header(RANGE, format!("bytes={a}-{b}"));
        }
        if let Some(ua) = &t.user_agent {
            rb = rb.header(USER_AGENT, ua.clone());
        }
        rb
    }

    async fn send(rb: reqwest::RequestBuilder) -> Result<reqwest::Response, CacheError> {
        rb.send().await.map_err(CacheError::upstream)
    }

    fn cache_meta(&self, prefix: &str, meta: Arc<Meta>) {
        if self.metas.len() >= META_CACHE_MAX {
            self.metas.clear();
        }
        self.metas.insert(prefix.to_string(), meta);
    }

    async fn load_meta(&self, t: &Target) -> Option<Arc<Meta>> {
        if let Some(m) = self.metas.get(&t.prefix) {
            return Some(m.clone());
        }
        let bytes = match self.storage.get(&t.key.meta_key()).await {
            Ok(Some(b)) => b,
            Ok(None) => return None,
            Err(e) => {
                log::warn!("reading meta for {}: {e:#}", t.prefix);
                return None;
            }
        };
        match serde_json::from_slice::<Meta>(&bytes) {
            Ok(m) if m.is_valid() => {
                let m = Arc::new(m);
                self.cache_meta(&t.prefix, m.clone());
                Some(m)
            }
            _ => {
                log::warn!("ignoring invalid meta for {}", t.prefix);
                None
            }
        }
    }

    async fn store_meta(&self, t: &Target, meta: &Arc<Meta>) {
        let json = serde_json::to_vec(meta.as_ref()).unwrap_or_default();
        let len = json.len() as u64;
        match self.storage.put(&t.key.meta_key(), Bytes::from(json)).await {
            Ok(()) => self.index.record_meta(&t.prefix, t.identifier(), len),
            Err(e) => log::warn!("storing meta for {}: {e:#}", t.prefix),
        }
        self.cache_meta(&t.prefix, meta.clone());
    }

    /// Stores a verified slice; storage failures are logged, not fatal to the request.
    async fn store_slice(&self, t: &Target, index: u64, data: Bytes) {
        let len = data.len() as u64;
        match self.storage.put(&t.key.slice_key(index), data).await {
            Ok(()) => self
                .index
                .record_slice(&t.prefix, t.identifier(), index, len),
            Err(e) => log::warn!("storing slice {index} of {}: {e:#}", t.prefix),
        }
    }

    fn new_meta(&self, t: &Target, total: u64, headers: &HeaderMap) -> Arc<Meta> {
        Arc::new(Meta {
            identifier: t.identifier().to_string(),
            host: t.host.clone(),
            path: t.key.path.clone(),
            total_size: total,
            slice_size: self.slice_size,
            content_type: header_string(headers, CONTENT_TYPE)
                .unwrap_or_else(|| "application/octet-stream".into()),
            etag: header_string(headers, ETAG),
            last_modified: header_string(headers, LAST_MODIFIED),
            created: chrono::Utc::now().to_rfc3339(),
        })
    }

    /// Fetches slice 0 of an object we have no metadata for.
    async fn probe(&self, t: &Target) -> Result<ProbeOutcome, CacheError> {
        let resp = Self::send(self.upstream_get(t, Some((0, self.slice_size - 1)))).await?;
        match resp.status() {
            StatusCode::PARTIAL_CONTENT => self.probe_partial(t, resp).await,
            StatusCode::OK => match resp.content_length() {
                Some(n) if n > MAX_OBJECT_SIZE => Err(too_large(n)),
                Some(n) if n > 0 => {
                    let meta = self.new_meta(t, n, resp.headers());
                    self.store_meta(t, &meta).await;
                    Ok(ProbeOutcome::Full(meta, resp))
                }
                _ => Ok(ProbeOutcome::Passthrough(resp)),
            },
            _ => Ok(ProbeOutcome::Passthrough(resp)),
        }
    }

    async fn probe_partial(
        &self,
        t: &Target,
        resp: reqwest::Response,
    ) -> Result<ProbeOutcome, CacheError> {
        let cr = header_string(resp.headers(), CONTENT_RANGE)
            .as_deref()
            .and_then(parse_content_range);
        let Some(cr) = cr else {
            return Ok(ProbeOutcome::Passthrough(resp));
        };
        if cr.total > MAX_OBJECT_SIZE {
            return Err(too_large(cr.total));
        }
        let meta = self.new_meta(t, cr.total, resp.headers());
        if (cr.start, cr.end) != meta.slice_bounds(0) {
            return Err(CacheError::Upstream(format!(
                "unexpected Content-Range {}-{}/{} for slice 0",
                cr.start, cr.end, cr.total
            )));
        }
        let body = read_exact(resp, 0, meta.slice_len(0)).await?;
        self.store_slice(t, 0, body.clone()).await;
        self.store_meta(t, &meta).await;
        Ok(ProbeOutcome::Stored(meta, body))
    }

    /// Resolves an object's metadata, probing upstream (coalesced) when unknown.
    pub(crate) async fn open(&self, t: &Target) -> Result<Opened, CacheError> {
        if let Some(meta) = self.load_meta(t).await {
            return Ok(Opened::Ready { meta, fresh: None });
        }
        let cell = self.probes.entry(t.prefix.clone()).or_default().clone();
        let mut own: Option<ProbeOutcome> = None;
        let shared = cell
            .get_or_init(|| async {
                if let Some(meta) = self.load_meta(t).await {
                    return Ok(Some((meta, None)));
                }
                match self.probe(t).await? {
                    ProbeOutcome::Stored(meta, b) => Ok(Some((meta, Some(b)))),
                    other => {
                        own = Some(other);
                        Ok(None)
                    }
                }
            })
            .await
            .clone();
        self.probes
            .remove_if(&t.prefix, |_, v| Arc::ptr_eq(v, &cell));
        match shared? {
            Some((meta, fresh)) => Ok(Opened::Ready { meta, fresh }),
            None => match (own, self.load_meta(t).await) {
                (Some(o), _) => Ok(o.into()),
                (None, Some(meta)) => Ok(Opened::Ready { meta, fresh: None }),
                (None, None) => Ok(self.probe(t).await?.into()),
            },
        }
    }

    async fn cached_slice(&self, t: &Target, meta: &Meta, index: u64) -> Option<Bytes> {
        match self.storage.get(&t.key.slice_key(index)).await {
            Ok(Some(b)) if b.len() as u64 == meta.slice_len(index) => {
                self.index.mark_slice(&t.prefix, t.identifier(), index);
                Some(b)
            }
            Ok(Some(_)) => {
                log::warn!("slice {index} of {} has the wrong length", t.prefix);
                self.index
                    .unmark_slice(&t.prefix, index, meta.slice_len(index));
                None
            }
            Ok(None) => {
                self.index
                    .unmark_slice(&t.prefix, index, meta.slice_len(index));
                None
            }
            Err(e) => {
                log::warn!("reading slice {index} of {}: {e:#}", t.prefix);
                None
            }
        }
    }

    /// Returns a slice and whether it came from storage; misses are coalesced.
    pub(crate) async fn slice(
        &self,
        t: &Target,
        meta: &Meta,
        index: u64,
    ) -> Result<(Bytes, bool), CacheError> {
        if let Some(b) = self.cached_slice(t, meta, index).await {
            return Ok((b, true));
        }
        let key = t.key.slice_key(index);
        let cell = self.slices.entry(key.clone()).or_default().clone();
        let res = cell
            .get_or_init(|| async {
                // A previous leader may have stored it since our check.
                if let Some(b) = self.cached_slice(t, meta, index).await {
                    return Ok(b);
                }
                self.fetch_slice(t, meta, index).await
            })
            .await
            .clone();
        self.slices.remove_if(&key, |_, v| Arc::ptr_eq(v, &cell));
        res.map(|b| (b, false))
    }

    /// True if the slice is stored (touches the object).
    pub(crate) async fn slice_stored(&self, t: &Target, meta: &Meta, index: u64) -> bool {
        let stored = self
            .storage
            .exists(&t.key.slice_key(index))
            .await
            .unwrap_or(false);
        if stored {
            self.index.mark_slice(&t.prefix, t.identifier(), index);
        } else {
            self.index
                .unmark_slice(&t.prefix, index, meta.slice_len(index));
        }
        stored
    }

    async fn fetch_slice(&self, t: &Target, meta: &Meta, index: u64) -> Result<Bytes, CacheError> {
        let (start, end) = meta.slice_bounds(index);
        let resp = Self::send(self.upstream_get(t, Some((start, end)))).await?;
        let skip = match resp.status() {
            StatusCode::PARTIAL_CONTENT => {
                let cr = header_string(resp.headers(), CONTENT_RANGE)
                    .as_deref()
                    .and_then(parse_content_range);
                match cr {
                    Some(cr) if cr.total != meta.total_size => {
                        return Err(self.invalidate(t, meta, cr.total).await);
                    }
                    Some(cr) if (cr.start, cr.end) == (start, end) => 0,
                    _ => {
                        return Err(CacheError::Upstream(format!(
                            "bad Content-Range for slice {index}"
                        )));
                    }
                }
            }
            StatusCode::OK => match resp.content_length() {
                Some(n) if n != meta.total_size => return Err(self.invalidate(t, meta, n).await),
                _ => start,
            },
            s => return Err(CacheError::Upstream(format!("upstream status {s}"))),
        };
        let body = read_exact(resp, skip, meta.slice_len(index)).await?;
        self.store_slice(t, index, body.clone()).await;
        Ok(body)
    }

    /// The object changed size upstream: drop everything we have for it.
    async fn invalidate(&self, t: &Target, meta: &Meta, new_total: u64) -> CacheError {
        log::warn!(
            "{} changed upstream ({} -> {new_total} bytes), invalidating",
            t.prefix,
            meta.total_size
        );
        if let Err(e) = self.storage.delete_prefix(&format!("{}/", t.prefix)).await {
            log::warn!("deleting {}: {e:#}", t.prefix);
        }
        self.metas.remove(&t.prefix);
        self.index.remove_object(&t.prefix);
        CacheError::Changed(format!("{}{}", t.host, t.key.path))
    }

    // ---------------------------------------------------------------------
    // Prefill helpers
    // ---------------------------------------------------------------------

    /// Builds a prefill target; the bool is false when the platform bypasses this path.
    /// Disabled platforms are refused so nothing is cached for them.
    async fn prefill_target(
        &self,
        identifier: &str,
        host: &str,
        pq: &str,
    ) -> Result<(Target, bool)> {
        let platform = self
            .platforms
            .get(identifier)
            .ok_or_else(|| anyhow!("unknown platform {identifier}"))?;
        if !self.settings.get().await.platform_enabled(identifier) {
            bail!("platform {identifier} is disabled");
        }
        let host = super::proxy::strip_port(host);
        let t = Target::new(identifier, &host, pq, None);
        let cacheable = platform.cache_policy(&t.key.path) == CachePolicy::Cache;
        Ok((t, cacheable))
    }

    /// Reads `[a, b]` of an opened object (None = whole object) into memory.
    async fn read_opened(
        &self,
        t: &Target,
        opened: Opened,
        range: Option<(u64, u64)>,
    ) -> Result<Bytes> {
        let (meta, fresh, mut full) = match opened {
            Opened::Ready { meta, fresh } => (meta, fresh, None),
            Opened::Full { meta, body } => (meta, None, Some(body)),
            Opened::Passthrough(resp) => return read_uncached(resp, range).await,
        };
        let (a, b) = check_range(meta.total_size, range)?;
        let mut out = BytesMut::with_capacity((b - a + 1) as usize);
        let (first, last) = (a / meta.slice_size, b / meta.slice_size);
        if let Some(body) = full.as_mut() {
            while body.next_index() <= last {
                let Some((i, data)) = body.next_slice(self, t, &meta).await? else {
                    break;
                };
                out.extend_from_slice(&trim(&meta, i, data, a, b));
            }
            return Ok(out.freeze());
        }
        let meta = &meta;
        let mut slices = futures::stream::iter(first..=last)
            .map(|i| {
                let fresh = fresh.clone();
                async move {
                    let data = match (fresh, i) {
                        (Some(f), 0) => f,
                        _ => self.slice(t, meta, i).await?.0,
                    };
                    Ok::<_, CacheError>(trim(meta, i, data, a, b))
                }
            })
            .buffered(self.read_ahead);
        while let Some(part) = slices.next().await {
            out.extend_from_slice(&part?);
        }
        Ok(out.freeze())
    }

    async fn prefill_read(
        &self,
        identifier: &str,
        host: &str,
        pq: &str,
        range: Option<(u64, u64)>,
    ) -> Result<Bytes> {
        let (t, cacheable) = self.prefill_target(identifier, host, pq).await?;
        if !cacheable {
            let resp = Self::send(self.upstream_get(&t, None)).await?;
            return read_uncached(resp, range).await;
        }
        let opened = self.open(&t).await?;
        self.read_opened(&t, opened, range).await
    }
}

/// Validates an inclusive range against `total` (None = whole object) and the memory cap.
fn check_range(total: u64, range: Option<(u64, u64)>) -> Result<(u64, u64)> {
    let (a, b) = range.unwrap_or((0, total.saturating_sub(1)));
    if total == 0 || a > b || b >= total {
        bail!("range {a}-{b} outside object of {total} bytes");
    }
    if b - a + 1 > MAX_PREFILL_BODY {
        bail!("refusing to buffer {} bytes", b - a + 1);
    }
    Ok((a, b))
}

/// Reads an uncacheable whole-body 200 response (bounded) and cuts out `range`.
async fn read_uncached(resp: reqwest::Response, range: Option<(u64, u64)>) -> Result<Bytes> {
    let status = resp.status();
    if status != StatusCode::OK {
        bail!("upstream returned {status} for {}", resp.url());
    }
    let mut stream = resp.bytes_stream();
    let mut out = BytesMut::new();
    while let Some(chunk) = stream.next().await {
        out.extend_from_slice(&chunk?);
        if out.len() as u64 > MAX_PREFILL_BODY {
            bail!("upstream body exceeds {MAX_PREFILL_BODY} bytes");
        }
    }
    let body = out.freeze();
    match range {
        None => Ok(body),
        Some((a, b)) => {
            let (a, b) = check_range(body.len() as u64, Some((a, b)))?;
            Ok(body.slice(a as usize..=b as usize))
        }
    }
}

#[async_trait]
impl Prefill for CacheEngine {
    async fn warm(&self, identifier: &str, host: &str, path_and_query: &str) -> Result<WarmResult> {
        let (t, cacheable) = self
            .prefill_target(identifier, host, path_and_query)
            .await?;
        if !cacheable {
            log::debug!("prefill: {path_and_query} bypasses the cache, skipping warm");
            return Ok(WarmResult::default());
        }
        let mut r = WarmResult::default();
        let (meta, start) = match self.open(&t).await? {
            Opened::Ready { meta, fresh: None } => {
                // Fast path: the index says every slice was written, so skip
                // per-slice existence checks (a verify pass over a large game
                // would otherwise cost one storage HEAD per slice).
                if self.index.has_all_slices(&t.prefix, meta.slice_count()) {
                    self.index.touch(&t.prefix, identifier);
                    r.bytes_already_cached = meta.total_size;
                    return Ok(r);
                }
                (meta, 0)
            }
            Opened::Ready {
                meta,
                fresh: Some(f),
            } => {
                r.bytes_downloaded += f.len() as u64;
                (meta, 1)
            }
            Opened::Full { meta, mut body } => {
                while let Some((_, data)) = body.next_slice(self, &t, &meta).await? {
                    r.bytes_downloaded += data.len() as u64;
                }
                return Ok(r);
            }
            Opened::Passthrough(resp) => bail!(
                "upstream returned {} for {host}{path_and_query}; not cacheable",
                resp.status()
            ),
        };
        let mut results = futures::stream::iter(start..meta.slice_count())
            .map(|i| {
                let (t, meta) = (&t, &meta);
                async move {
                    if self.slice_stored(t, meta, i).await {
                        return Ok((meta.slice_len(i), true));
                    }
                    self.slice(t, meta, i)
                        .await
                        .map(|(b, _)| (b.len() as u64, false))
                }
            })
            .buffer_unordered(WARM_PARALLELISM);
        while let Some(res) = results.next().await {
            match res? {
                (n, true) => r.bytes_already_cached += n,
                (n, false) => r.bytes_downloaded += n,
            }
        }
        Ok(r)
    }

    async fn get(&self, identifier: &str, host: &str, path_and_query: &str) -> Result<Bytes> {
        self.prefill_read(identifier, host, path_and_query, None)
            .await
    }

    async fn get_range(
        &self,
        identifier: &str,
        host: &str,
        path_and_query: &str,
        start: u64,
        end: u64,
    ) -> Result<Bytes> {
        if start > end {
            bail!("invalid range {start}-{end}");
        }
        self.prefill_read(identifier, host, path_and_query, Some((start, end)))
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trimming() {
        let meta = Meta {
            identifier: "x".into(),
            host: "h".into(),
            path: "/".into(),
            total_size: 250,
            slice_size: 100,
            content_type: "a/b".into(),
            etag: None,
            last_modified: None,
            created: String::new(),
        };
        let s1 = Bytes::from((100u8..200).collect::<Vec<_>>());
        assert_eq!(
            trim(&meta, 1, s1.clone(), 150, 160).as_ref(),
            &(150u8..=160).collect::<Vec<_>>()[..]
        );
        assert_eq!(trim(&meta, 1, s1.clone(), 0, 249).len(), 100);
        assert!(trim(&meta, 1, s1.clone(), 0, 50).is_empty());
        assert!(trim(&meta, 1, s1, 200, 249).is_empty());
    }

    #[test]
    fn ranges_checked() {
        assert_eq!(check_range(10, None).unwrap(), (0, 9));
        assert_eq!(check_range(10, Some((2, 3))).unwrap(), (2, 3));
        assert!(check_range(10, Some((2, 10))).is_err());
        assert!(check_range(0, None).is_err());
        assert!(check_range(MAX_PREFILL_BODY + 1, None).is_err());
    }

    #[test]
    fn errors_classified() {
        let e = CacheError::upstream(anyhow::Error::new(LoopDetected("h".into())).context("x"));
        assert_eq!(e.status(), StatusCode::LOOP_DETECTED);
        let e = CacheError::upstream(anyhow!("boom"));
        assert_eq!(e.status(), StatusCode::BAD_GATEWAY);
    }

    #[tokio::test]
    async fn loop_guard_through_reqwest() {
        use crate::upstream::{LoopGuard, StaticResolver};
        let ip: std::net::IpAddr = [127, 0, 0, 1].into();
        let guard = LoopGuard::new(Arc::new(StaticResolver(ip)), vec![ip]);
        let u = Upstream::new(Arc::new(guard), 9, "i".into()).unwrap();
        let err = u
            .client()
            .get("http://x.example/")
            .send()
            .await
            .unwrap_err();
        assert_eq!(
            CacheError::upstream(err).status(),
            StatusCode::LOOP_DETECTED
        );
    }
}
