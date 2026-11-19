//! HTTP handler for the cache listener: bypass/plain proxy decisions, Range
//! handling and slice-by-slice streaming of cached objects.
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Request, State};
use axum::response::{IntoResponse, Response};
use futures::stream::{self, StreamExt, TryStreamExt};
use http::header::{
    ACCEPT_RANGES, CONNECTION, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, ETAG, HOST, HeaderMap,
    HeaderName, HeaderValue, LAST_MODIFIED, RANGE, TRANSFER_ENCODING, USER_AGENT,
};
use http::{Method, StatusCode};

use super::engine::{CacheEngine, CacheError, FullBody, Opened, Target, trim};
use super::meta::Meta;
use super::range::{RangeRequest, resolve_range};
use crate::metrics::CacheResult;
use crate::platforms::{CachePolicy, DynPlatform};
use crate::upstream::PROCESSED_BY_HEADER;

pub const STATUS_HEADER: &str = "x-gamecacher-status";

const HOP_BY_HOP: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "proxy-connection",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];

/// Lower-cased host without port (handles `[v6]:port`).
pub(crate) fn strip_port(host: &str) -> String {
    let host = host.trim();
    let bare = match host.strip_prefix('[') {
        Some(rest) => rest.split(']').next().unwrap_or_default(),
        None => host.split(':').next().unwrap_or_default(),
    };
    bare.trim_end_matches('.').to_ascii_lowercase()
}

/// True for a plain DNS name: rejects IP literals in every form the URL parser
/// accepts (`10.0.0.1`, `0x7f.1`, `3232235777`, `[::1]`) and anything with
/// characters that would change the URL (`/`, `@`, `?`, `#`).
pub(crate) fn is_domain_name(host: &str) -> bool {
    host.split('.').all(|l| {
        !l.is_empty() && l.len() <= 63 && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
    }) && host.len() <= 253
        && matches!(url::Host::parse(host), Ok(url::Host::Domain(_)))
}

fn request_host(req: &Request) -> Option<String> {
    let raw = match req.headers().get(HOST) {
        Some(h) => h.to_str().ok()?.to_string(),
        None => req.uri().authority()?.as_str().to_string(),
    };
    let host = strip_port(&raw);
    (!host.is_empty()).then_some(host)
}

fn has_nocache(path_and_query: &str) -> bool {
    path_and_query
        .split_once('?')
        .map(|(_, q)| {
            q.split('&')
                .any(|kv| kv.split('=').next() == Some("nocache"))
        })
        .unwrap_or(false)
}

/// Copies headers minus hop-by-hop ones (including those named in `Connection`).
fn strip_hop_by_hop(headers: &HeaderMap) -> HeaderMap {
    let named: Vec<String> = headers
        .get_all(CONNECTION)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(|s| s.trim().to_ascii_lowercase())
        .collect();
    let mut out = headers.clone();
    for name in HOP_BY_HOP
        .iter()
        .copied()
        .chain(named.iter().map(String::as_str))
    {
        out.remove(name);
    }
    out
}

fn text(status: StatusCode, msg: &str) -> Response {
    (status, msg.to_string()).into_response()
}

pub(crate) async fn handle(State(engine): State<Arc<CacheEngine>>, req: Request) -> Response {
    let Some(host) = request_host(&req) else {
        return text(StatusCode::BAD_REQUEST, "missing Host header");
    };
    let instance = engine.upstream.instance_id().as_bytes();
    if req
        .headers()
        .get_all(PROCESSED_BY_HEADER)
        .iter()
        .any(|v| v.as_bytes() == instance)
    {
        return text(StatusCode::LOOP_DETECTED, "loop detected");
    }
    let ua = req.headers().get(USER_AGENT).cloned();
    let ua_str = ua.as_ref().and_then(|v| v.to_str().ok()).unwrap_or("");
    let pq = req
        .uri()
        .path_and_query()
        .map(|p| p.as_str().to_string())
        .unwrap_or_else(|| "/".into());
    // Only platform traffic is served: anything else (unknown hosts, IP literals in
    // any spelling) is refused so the cache can't be used as an open proxy.
    if !is_domain_name(&host) {
        return text(StatusCode::FORBIDDEN, "only DNS host names are served");
    }
    let platform = engine.platforms.identify(&host, ua_str).cloned();
    if platform.is_none() {
        engine.metrics.request("other", CacheResult::Bypass);
        return text(StatusCode::FORBIDDEN, "host is not served by this cache");
    }
    // Identified by user agent alone (Steam CDN hosts aren't enumerable): serve from
    // the cache only, never relay other methods or disabled-platform traffic.
    let by_ua_only = engine.platforms.by_host(&host).is_none();
    let label = platform.as_ref().map(|p| p.id()).unwrap_or("other");
    let cacheable = match &platform {
        Some(p) => is_cacheable(&engine, p, req.method(), &pq).await,
        None => false,
    };
    if by_ua_only && !cacheable {
        engine.metrics.request(label, CacheResult::Bypass);
        return text(StatusCode::FORBIDDEN, "not cacheable for this host");
    }
    match platform {
        Some(p) if cacheable => {
            let head = req.method() == Method::HEAD;
            let range = req
                .headers()
                .get(RANGE)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string);
            let t = Arc::new(Target::new(p.id(), &host, &pq, ua));
            serve_cached(engine, p, t, head, range).await
        }
        _ => plain_proxy(&engine, req, &host, label).await,
    }
}

async fn is_cacheable(engine: &CacheEngine, p: &DynPlatform, method: &Method, pq: &str) -> bool {
    let path = pq.split('?').next().unwrap_or_default();
    (method == Method::GET || method == Method::HEAD)
        && !has_nocache(pq)
        && p.cache_policy(path) == CachePolicy::Cache
        && engine.settings.get().await.platform_enabled(p.id())
}

fn error_response(engine: &CacheEngine, platform: &str, e: &CacheError) -> Response {
    log::warn!("cache request failed ({platform}): {e}");
    engine.metrics.upstream_error(platform);
    text(e.status(), &e.to_string())
}

/// Forwards an upstream response verbatim (minus hop-by-hop headers), marked BYPASS.
fn forward_response(
    engine: &CacheEngine,
    label: &str,
    resp: reqwest::Response,
    head: bool,
) -> Response {
    let status = resp.status();
    let mut headers = strip_hop_by_hop(resp.headers());
    headers.insert(STATUS_HEADER, HeaderValue::from_static("BYPASS"));
    let body = if head {
        Body::empty()
    } else {
        let metrics = engine.metrics.clone();
        let label = label.to_string();
        Body::from_stream(
            resp.bytes_stream()
                .inspect_ok(move |c| metrics.bytes_served(&label, false, c.len() as u64)),
        )
    };
    let mut out = Response::new(body);
    *out.status_mut() = status;
    *out.headers_mut() = headers;
    out
}

/// Proxies a request upstream without caching.
async fn plain_proxy(engine: &CacheEngine, req: Request, host: &str, label: &str) -> Response {
    engine.metrics.request(label, CacheResult::Bypass);
    let (parts, body) = req.into_parts();
    let pq = parts
        .uri
        .path_and_query()
        .map(|p| p.as_str())
        .unwrap_or("/");
    let mut headers = strip_hop_by_hop(&parts.headers);
    headers.remove(HOST);
    if let Ok(v) = HeaderValue::from_str(engine.upstream.instance_id()) {
        headers.insert(PROCESSED_BY_HEADER, v);
    }
    let mut rb = engine
        .upstream
        .client()
        .request(parts.method.clone(), engine.upstream.http_url(host, pq))
        .headers(headers);
    if parts.headers.contains_key(CONTENT_LENGTH) || parts.headers.contains_key(TRANSFER_ENCODING) {
        rb = rb.body(reqwest::Body::wrap_stream(body.into_data_stream()));
    }
    match rb.send().await {
        Ok(resp) => forward_response(engine, label, resp, parts.method == Method::HEAD),
        Err(e) => error_response(engine, label, &CacheError::upstream(e)),
    }
}

/// Headers for a response served from slices.
fn object_headers(
    meta: &Meta,
    hide_etag: bool,
    partial: bool,
    a: u64,
    b: u64,
    hit: bool,
) -> HeaderMap {
    let mut h = HeaderMap::new();
    let mut set = |name: HeaderName, value: &str| {
        if let Ok(v) = HeaderValue::from_str(value) {
            h.insert(name, v);
        }
    };
    set(CONTENT_TYPE, &meta.content_type);
    set(CONTENT_LENGTH, &(b - a + 1).to_string());
    set(ACCEPT_RANGES, "bytes");
    if partial {
        set(CONTENT_RANGE, &format!("bytes {a}-{b}/{}", meta.total_size));
    }
    if !hide_etag {
        if let Some(etag) = &meta.etag {
            set(ETAG, etag);
        }
        if let Some(lm) = &meta.last_modified {
            set(LAST_MODIFIED, lm);
        }
    }
    set(
        HeaderName::from_static(STATUS_HEADER),
        if hit { "HIT" } else { "MISS" },
    );
    h
}

fn unsatisfiable(total: u64) -> Response {
    let mut h = HeaderMap::new();
    h.insert(ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    if let Ok(v) = HeaderValue::from_str(&format!("bytes */{total}")) {
        h.insert(CONTENT_RANGE, v);
    }
    (StatusCode::RANGE_NOT_SATISFIABLE, h).into_response()
}

async fn serve_cached(
    engine: Arc<CacheEngine>,
    platform: DynPlatform,
    t: Arc<Target>,
    head: bool,
    range: Option<String>,
) -> Response {
    let id = platform.id();
    let opened = match engine.open(&t).await {
        Ok(o) => o,
        Err(e) => {
            engine.metrics.request(id, CacheResult::Miss);
            return error_response(&engine, id, &e);
        }
    };
    let (meta, fresh, full) = match opened {
        Opened::Passthrough(resp) => {
            engine.metrics.request(id, CacheResult::Bypass);
            return forward_response(&engine, id, resp, head);
        }
        Opened::Ready { meta, fresh } => (meta, fresh, None),
        Opened::Full { meta, body } => (meta, None, Some(body)),
    };
    let total = meta.total_size;
    let (partial, a, b) = match resolve_range(range.as_deref(), total) {
        RangeRequest::Full => (false, 0, total - 1),
        RangeRequest::Partial(a, b) => (true, a, b),
        RangeRequest::Unsatisfiable => {
            let hit = fresh.is_none() && full.is_none();
            engine.metrics.request(
                id,
                if hit {
                    CacheResult::Hit
                } else {
                    CacheResult::Miss
                },
            );
            return unsatisfiable(total);
        }
    };
    let status = if partial {
        StatusCode::PARTIAL_CONTENT
    } else {
        StatusCode::OK
    };
    let hide_etag = platform.hide_etag();
    if let Some(body) = full {
        engine.metrics.request(id, CacheResult::Miss);
        let headers = object_headers(&meta, hide_etag, partial, a, b, false);
        if head {
            return (status, headers).into_response();
        }
        let stream = full_stream(engine, t, meta, body, a, b);
        return (status, headers, Body::from_stream(stream)).into_response();
    }
    // Fetch the first slice up front: it decides HIT/MISS and lets upstream
    // failures (or a changed object) surface as a proper error status.
    let first = a / meta.slice_size;
    let first_item = match (head, first, fresh) {
        (true, _, fresh) => {
            let hit = fresh.is_none() && engine.slice_stored(&t, &meta, first).await;
            engine.metrics.request(
                id,
                if hit {
                    CacheResult::Hit
                } else {
                    CacheResult::Miss
                },
            );
            return (status, object_headers(&meta, hide_etag, partial, a, b, hit)).into_response();
        }
        (false, 0, Some(f)) => (f, false),
        (false, _, _) => match engine.slice(&t, &meta, first).await {
            Ok(x) => x,
            Err(e) => {
                engine.metrics.request(id, CacheResult::Miss);
                return error_response(&engine, id, &e);
            }
        },
    };
    let hit = first_item.1;
    engine.metrics.request(
        id,
        if hit {
            CacheResult::Hit
        } else {
            CacheResult::Miss
        },
    );
    let headers = object_headers(&meta, hide_etag, partial, a, b, hit);
    let stream = slice_stream(engine, t, meta, first_item, a, b);
    (status, headers, Body::from_stream(stream)).into_response()
}

type BodyChunk = Result<bytes::Bytes, std::io::Error>;

/// Records metrics for one trimmed chunk or turns an error into a body abort.
fn emit(engine: &CacheEngine, id: &str, r: Result<(bytes::Bytes, bool), CacheError>) -> BodyChunk {
    match r {
        Ok((chunk, from_cache)) => {
            engine
                .metrics
                .bytes_served(id, from_cache, chunk.len() as u64);
            Ok(chunk)
        }
        Err(e) => {
            log::warn!("aborting response body ({id}): {e}");
            engine.metrics.upstream_error(id);
            Err(std::io::Error::other(e))
        }
    }
}

/// Streams `[a, b]` slice by slice, starting with the already-fetched first slice.
fn slice_stream(
    engine: Arc<CacheEngine>,
    t: Arc<Target>,
    meta: Arc<Meta>,
    first: (bytes::Bytes, bool),
    a: u64,
    b: u64,
) -> impl futures::Stream<Item = BodyChunk> + Send + 'static {
    let (first_idx, last_idx) = (a / meta.slice_size, b / meta.slice_size);
    let head = stream::once(futures::future::ready(Ok((first_idx, first.0, first.1))));
    let (e2, t2, m2) = (engine.clone(), t.clone(), meta.clone());
    let rest = stream::iter(first_idx + 1..=last_idx).then(move |i| {
        let (e, t, m) = (e2.clone(), t2.clone(), m2.clone());
        async move { e.slice(&t, &m, i).await.map(|(d, c)| (i, d, c)) }
    });
    head.chain(rest).map(move |r| {
        let r = r.map(|(i, data, cached)| (trim(&meta, i, data, a, b), cached));
        emit(&engine, t.identifier(), r)
    })
}

/// Streams `[a, b]` from a whole-body upstream response, caching every slice it passes.
fn full_stream(
    engine: Arc<CacheEngine>,
    t: Arc<Target>,
    meta: Arc<Meta>,
    body: FullBody,
    a: u64,
    b: u64,
) -> impl futures::Stream<Item = BodyChunk> + Send + 'static {
    let last = b / meta.slice_size;
    let state = (engine.clone(), t.clone(), meta.clone(), body);
    let slices = stream::try_unfold(state, move |(e, t, m, mut body)| async move {
        if body.next_index() > last {
            return Ok(None);
        }
        Ok(body
            .next_slice(&e, &t, &m)
            .await?
            .map(|(i, data)| ((i, data), (e.clone(), t.clone(), m.clone(), body))))
    });
    slices
        .map_ok(move |(i, data)| trim(&meta, i, data, a, b))
        .try_filter(|chunk| futures::future::ready(!chunk.is_empty()))
        .map(move |r| emit(&engine, t.identifier(), r.map(|c| (c, false))))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domain_names() {
        for ok in [
            "cdn1.epicgames.com",
            "cache5-fra1.steamcontent.com",
            "localhost",
        ] {
            assert!(is_domain_name(ok), "{ok}");
        }
        for bad in [
            "10.0.0.1",
            "0x7f.1",
            "3232235777",
            "::1",
            "192.168.1.1/",
            "a@b.com",
            "x.com?y",
            "x..com",
            "",
            "under_score.com",
            "127.1",
        ] {
            assert!(!is_domain_name(bad), "{bad}");
        }
    }

    #[test]
    fn hosts() {
        assert_eq!(strip_port("CDN.Example.com:8080"), "cdn.example.com");
        assert_eq!(strip_port("example.com."), "example.com");
        assert_eq!(strip_port("[::1]:80"), "::1");
        assert_eq!(strip_port("10.0.0.1"), "10.0.0.1");
    }

    #[test]
    fn nocache() {
        assert!(has_nocache("/a?nocache=1"));
        assert!(has_nocache("/a?x=1&nocache"));
        assert!(!has_nocache("/a?nocachex=1"));
        assert!(!has_nocache("/nocache"));
    }

    #[test]
    fn hop_by_hop() {
        let mut h = HeaderMap::new();
        h.insert(CONNECTION, HeaderValue::from_static("keep-alive, x-custom"));
        h.insert("x-custom", HeaderValue::from_static("1"));
        h.insert(TRANSFER_ENCODING, HeaderValue::from_static("chunked"));
        h.insert(CONTENT_TYPE, HeaderValue::from_static("a/b"));
        let out = strip_hop_by_hop(&h);
        assert_eq!(out.len(), 1);
        assert!(out.contains_key(CONTENT_TYPE));
    }
}
