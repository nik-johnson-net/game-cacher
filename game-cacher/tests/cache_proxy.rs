//! Cache proxy integration tests: a local fake CDN upstream, the cache engine
//! on MemoryStorage, and reqwest clients sending explicit Host headers.
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::extract::{Request, State};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use game_cacher::cache::key::ObjectKey;
use game_cacher::cache::range::{RangeRequest, resolve_range};
use game_cacher::cache::{CacheEngine, EngineDeps, LruIndex, STATUS_HEADER};
use game_cacher::metrics::Metrics;
use game_cacher::platforms::{self, Platforms, Prefill};
use game_cacher::settings::{PlatformSettings, SettingsStore};
use game_cacher::storage::{DynStorage, MemoryStorage};
use game_cacher::upstream::{HostResolver, LoopGuard, StaticResolver, Upstream};
use http::StatusCode;
use http::header::{CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, ETAG, LOCATION, RANGE};

const SLICE: u64 = 64 * 1024;
const LOCALHOST: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);
const EPIC: &str = "cdn1.epicgames.com";
const BLIZZARD: &str = "us.cdn.blizzard.com";
const WSUS: &str = "x.dl.delivery.mp.microsoft.com";
const STEAM_UA: &str = "Valve/Steam HTTP Client 1.0";

fn data(size: u64) -> Vec<u8> {
    (0..size).map(|i| ((i * 31 + 7) % 251) as u8).collect()
}

// ---------------------------------------------------------------------------
// Fake upstream CDN
// ---------------------------------------------------------------------------

#[derive(Clone, Default)]
struct FakeCdn {
    counts: Arc<Mutex<HashMap<String, usize>>>,
    user_agents: Arc<Mutex<HashMap<String, String>>>,
    mutable_size: Arc<AtomicU64>,
}

fn serve_bytes(size: u64, range: Option<&str>, etag: bool) -> Response {
    let body = data(size);
    let mut resp = match resolve_range(range, size) {
        RangeRequest::Full => (StatusCode::OK, body).into_response(),
        RangeRequest::Partial(a, b) => (
            StatusCode::PARTIAL_CONTENT,
            [(CONTENT_RANGE, format!("bytes {a}-{b}/{size}"))],
            body[a as usize..=b as usize].to_vec(),
        )
            .into_response(),
        RangeRequest::Unsatisfiable => (
            StatusCode::RANGE_NOT_SATISFIABLE,
            [(CONTENT_RANGE, format!("bytes */{size}"))],
        )
            .into_response(),
    };
    let h = resp.headers_mut();
    h.insert(CONTENT_TYPE, "application/x-test".parse().unwrap());
    if etag {
        h.insert(ETAG, "\"v1\"".parse().unwrap());
        h.insert(
            "last-modified",
            "Wed, 01 Jan 2025 00:00:00 GMT".parse().unwrap(),
        );
    }
    resp
}

async fn upstream_handler(State(s): State<FakeCdn>, req: Request) -> Response {
    let path = req.uri().path().to_string();
    *s.counts.lock().unwrap().entry(path.clone()).or_default() += 1;
    let ua = req
        .headers()
        .get("user-agent")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    s.user_agents
        .lock()
        .unwrap()
        .insert(path.clone(), ua.into());
    let range = req
        .headers()
        .get(RANGE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let range = range.as_deref();
    let segments: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    match segments.as_slice() {
        ["obj", n] => serve_bytes(n.parse().unwrap(), range, false),
        ["etag", n] => serve_bytes(n.parse().unwrap(), range, true),
        ["norange", n] => serve_bytes(n.parse().unwrap(), None, false),
        ["mutable"] => serve_bytes(s.mutable_size.load(Ordering::SeqCst), range, false),
        ["redirect"] => (StatusCode::FOUND, [(LOCATION, "/obj/300000")]).into_response(),
        ["missing"] => (StatusCode::NOT_FOUND, "nope").into_response(),
        // Claims an absurd total size (2^60 bytes) for its first slice.
        ["huge"] => (
            StatusCode::PARTIAL_CONTENT,
            [(
                axum::http::header::CONTENT_RANGE,
                "bytes 0-65535/1152921504606846976",
            )],
            vec![0u8; 65536],
        )
            .into_response(),
        ["chunked"] => {
            let chunks = vec![Ok::<_, std::io::Error>(Bytes::from(data(1000)))];
            Body::from_stream(futures::stream::iter(chunks)).into_response()
        }
        ["echo"] => {
            let seen = req
                .headers()
                .get("x-gamecacher-processed-by")
                .cloned()
                .unwrap_or_else(|| "none".parse().unwrap());
            let method = req.method().to_string();
            let body = axum::body::to_bytes(req.into_body(), usize::MAX)
                .await
                .unwrap();
            (
                [
                    ("x-seen-processed-by", seen),
                    ("x-method", method.parse().unwrap()),
                ],
                body,
            )
                .into_response()
        }
        _ if path.ends_with("authrootstl.cab") => serve_bytes(1000, range, false),
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn spawn(router: axum::Router) -> u16 {
    let listener = tokio::net::TcpListener::bind((LOCALHOST, 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    port
}

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

struct H {
    base: String,
    client: reqwest::Client,
    up: FakeCdn,
    engine: Arc<CacheEngine>,
    storage: DynStorage,
    index: Arc<LruIndex>,
    settings: Arc<SettingsStore>,
}

#[derive(Default)]
struct Opts {
    max_bytes: Option<u64>,
    resolver: Option<Arc<dyn HostResolver>>,
    dead_upstream: bool,
}

async fn harness() -> H {
    harness_with(Opts::default()).await
}

async fn harness_with(opts: Opts) -> H {
    let up = FakeCdn::default();
    let mut up_port = spawn(
        axum::Router::new()
            .fallback(upstream_handler)
            .with_state(up.clone()),
    )
    .await;
    if opts.dead_upstream {
        let l = std::net::TcpListener::bind((LOCALHOST, 0)).unwrap();
        up_port = l.local_addr().unwrap().port();
        drop(l);
    }
    let storage: DynStorage = Arc::new(MemoryStorage::new());
    let metrics = Metrics::new();
    let settings = Arc::new(SettingsStore::load(storage.clone()).await.unwrap());
    let index = LruIndex::load_or_rebuild(storage.clone(), opts.max_bytes, metrics.clone())
        .await
        .unwrap();
    let resolver = opts
        .resolver
        .unwrap_or_else(|| Arc::new(StaticResolver(LOCALHOST)));
    let engine = CacheEngine::new(EngineDeps {
        storage: storage.clone(),
        platforms: Arc::new(Platforms::new(platforms::all())),
        settings: settings.clone(),
        upstream: Upstream::new(resolver, up_port, "test-instance".into()).unwrap(),
        index: index.clone(),
        metrics,
        slice_size: SLICE,
    });
    let port = spawn(engine.router()).await;
    H {
        base: format!("http://127.0.0.1:{port}"),
        client: reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .build()
            .unwrap(),
        up,
        engine,
        storage,
        index,
        settings,
    }
}

impl H {
    fn req(&self, method: reqwest::Method, host: &str, path: &str) -> reqwest::RequestBuilder {
        self.client
            .request(method, format!("{}{path}", self.base))
            .header("host", host)
    }

    fn get(&self, host: &str, path: &str) -> reqwest::RequestBuilder {
        self.req(reqwest::Method::GET, host, path)
    }

    async fn fetch(
        &self,
        host: &str,
        path: &str,
        range: Option<&str>,
    ) -> (StatusCode, String, Bytes) {
        let mut rb = self.get(host, path);
        if let Some(r) = range {
            rb = rb.header(RANGE, r);
        }
        let resp = rb.send().await.unwrap();
        let status = resp.status();
        let cache = header(&resp, STATUS_HEADER);
        (status, cache, resp.bytes().await.unwrap())
    }

    fn count(&self, path: &str) -> usize {
        self.up
            .counts
            .lock()
            .unwrap()
            .get(path)
            .copied()
            .unwrap_or(0)
    }

    async fn keys(&self, prefix: &str) -> Vec<String> {
        let mut keys: Vec<String> = self
            .storage
            .list(prefix)
            .await
            .unwrap()
            .into_iter()
            .map(|o| o.key)
            .collect();
        keys.sort();
        keys
    }
}

fn header(resp: &reqwest::Response, name: &str) -> String {
    resp.headers()
        .get(name)
        .map(|v| v.to_str().unwrap().to_string())
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn miss_then_hit() {
    let h = harness().await;
    let resp = h
        .get(EPIC, "/obj/200000")
        .header("user-agent", "EpicGamesLauncher/1.0")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(header(&resp, STATUS_HEADER), "MISS");
    assert_eq!(header(&resp, "accept-ranges"), "bytes");
    assert_eq!(header(&resp, CONTENT_TYPE.as_str()), "application/x-test");
    assert_eq!(header(&resp, CONTENT_LENGTH.as_str()), "200000");
    assert_eq!(resp.bytes().await.unwrap(), data(200000));
    assert_eq!(h.count("/obj/200000"), 4); // one request per 64 KiB slice
    assert_eq!(
        h.up.user_agents.lock().unwrap()["/obj/200000"],
        "EpicGamesLauncher/1.0"
    );

    let (status, cache, body) = h.fetch(EPIC, "/obj/200000", None).await;
    assert_eq!((status, cache.as_str()), (StatusCode::OK, "HIT"));
    assert_eq!(body, data(200000));
    assert_eq!(h.count("/obj/200000"), 4);

    let k = ObjectKey::new("epic", "/obj/200000");
    assert_eq!(h.keys(&k.prefix()).await.len(), 5); // meta + 4 slices
    let usage = h.index.usage();
    assert_eq!(usage.per_identifier["epic"].objects, 1);
    assert!(usage.total_bytes > 200000);
}

#[tokio::test]
async fn range_requests() {
    let h = harness().await;
    let full = data(200000);
    let resp = h
        .get(EPIC, "/obj/200000")
        .header(RANGE, "bytes=60000-140000")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        header(&resp, CONTENT_RANGE.as_str()),
        "bytes 60000-140000/200000"
    );
    assert_eq!(header(&resp, STATUS_HEADER), "MISS");
    assert_eq!(resp.bytes().await.unwrap(), full[60000..=140000]);
    // slice 0 (probe) + slices 1 and 2
    assert_eq!(h.count("/obj/200000"), 3);

    let (s, cache, body) = h.fetch(EPIC, "/obj/200000", Some("bytes=-100")).await;
    assert_eq!((s, cache.as_str()), (StatusCode::PARTIAL_CONTENT, "MISS"));
    assert_eq!(body, full[199900..]);

    let (s, cache, body) = h.fetch(EPIC, "/obj/200000", Some("bytes=150000-")).await;
    assert_eq!((s, cache.as_str()), (StatusCode::PARTIAL_CONTENT, "HIT"));
    assert_eq!(body, full[150000..]);

    let (s, _, body) = h
        .fetch(EPIC, "/obj/200000", Some("bytes=65536-65536"))
        .await;
    assert_eq!(
        (s, body.as_ref()),
        (StatusCode::PARTIAL_CONTENT, &full[65536..65537])
    );

    let resp = h
        .get(EPIC, "/obj/200000")
        .header(RANGE, "bytes=300000-")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(header(&resp, CONTENT_RANGE.as_str()), "bytes */200000");

    let (s, _, body) = h.fetch(EPIC, "/obj/200000", Some("bytes=0-1,5-6")).await;
    assert_eq!((s, body.len()), (StatusCode::OK, 200000));
    assert_eq!(h.count("/obj/200000"), 4);
}

#[tokio::test]
async fn head_requests() {
    let h = harness().await;
    let resp = h
        .req(reqwest::Method::HEAD, EPIC, "/obj/200000")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(header(&resp, STATUS_HEADER), "MISS");
    assert_eq!(header(&resp, CONTENT_LENGTH.as_str()), "200000");
    assert!(resp.bytes().await.unwrap().is_empty());

    let resp = h
        .req(reqwest::Method::HEAD, EPIC, "/obj/200000")
        .header(RANGE, "bytes=10-19")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(header(&resp, STATUS_HEADER), "HIT");
    assert_eq!(header(&resp, CONTENT_LENGTH.as_str()), "10");
    assert_eq!(h.count("/obj/200000"), 1);
}

#[tokio::test]
async fn upstream_ignoring_range() {
    let h = harness().await;
    let (s, cache, body) = h.fetch(EPIC, "/norange/200000", None).await;
    assert_eq!((s, cache.as_str()), (StatusCode::OK, "MISS"));
    assert_eq!(body, data(200000));
    assert_eq!(h.count("/norange/200000"), 1);
    let (s, cache, body) = h
        .fetch(EPIC, "/norange/200000", Some("bytes=70000-70009"))
        .await;
    assert_eq!((s, cache.as_str()), (StatusCode::PARTIAL_CONTENT, "HIT"));
    assert_eq!(body, data(200000)[70000..70010]);
    assert_eq!(h.count("/norange/200000"), 1);

    // First request is a range in the middle: only the slices up to it get cached,
    // later slices are cut out of further full-body responses.
    let full = data(150000);
    let (s, _, body) = h
        .fetch(EPIC, "/norange/150000", Some("bytes=70000-70009"))
        .await;
    assert_eq!(
        (s, body.as_ref()),
        (StatusCode::PARTIAL_CONTENT, &full[70000..70010])
    );
    let (s, _, body) = h.fetch(EPIC, "/norange/150000", None).await;
    assert_eq!((s, body), (StatusCode::OK, Bytes::from(full)));
    let (_, cache, _) = h.fetch(EPIC, "/norange/150000", None).await;
    assert_eq!(cache, "HIT");
}

#[tokio::test]
async fn unknown_length_is_not_cached() {
    let h = harness().await;
    let (s, cache, body) = h.fetch(EPIC, "/chunked", None).await;
    assert_eq!((s, cache.as_str()), (StatusCode::OK, "BYPASS"));
    assert_eq!(body, data(1000));
    assert!(h.keys("cache/").await.is_empty());
}

#[tokio::test]
async fn redirect_followed_and_cached_under_original_path() {
    let h = harness().await;
    let (s, cache, body) = h.fetch(EPIC, "/redirect", None).await;
    assert_eq!((s, cache.as_str()), (StatusCode::OK, "MISS"));
    assert_eq!(body, data(300000));
    let (redirects, objs) = (h.count("/redirect"), h.count("/obj/300000"));
    let (_, cache, body) = h.fetch(EPIC, "/redirect", None).await;
    assert_eq!(cache, "HIT");
    assert_eq!(body, data(300000));
    assert_eq!(
        (h.count("/redirect"), h.count("/obj/300000")),
        (redirects, objs)
    );
    let k = ObjectKey::new("epic", "/redirect");
    assert!(h.storage.exists(&k.meta_key()).await.unwrap());
    assert!(
        h.keys(&ObjectKey::new("epic", "/obj/300000").prefix())
            .await
            .is_empty()
    );
}

#[tokio::test]
async fn not_found_passthrough() {
    let h = harness().await;
    for _ in 0..2 {
        let (s, cache, body) = h.fetch(EPIC, "/missing", None).await;
        assert_eq!(
            (s, cache.as_str(), body.as_ref()),
            (StatusCode::NOT_FOUND, "BYPASS", &b"nope"[..])
        );
    }
    assert_eq!(h.count("/missing"), 2);
    assert!(h.keys("cache/").await.is_empty());
}

#[tokio::test]
async fn bypass_cases() {
    let h = harness().await;
    // Unknown hosts and IP literals are refused (no open proxy).
    let (s, _, _) = h.fetch("example.com", "/obj/1000", None).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _, _) = h.fetch("10.0.0.5", "/obj/1000", None).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    // Platform host with ?nocache is proxied uncached.
    let (s, cache, body) = h.fetch(EPIC, "/obj/1000?nocache=1", None).await;
    assert_eq!(
        (s, cache.as_str(), body),
        (StatusCode::OK, "BYPASS", Bytes::from(data(1000)))
    );
    // nocache query parameter.
    let (_, cache, _) = h.fetch(EPIC, "/obj/1000?nocache=1", None).await;
    assert_eq!(cache, "BYPASS");
    // Windows Update trust list.
    let (s, cache, _) = h.fetch(WSUS, "/msdownload/authrootstl.cab", None).await;
    assert_eq!((s, cache.as_str()), (StatusCode::OK, "BYPASS"));
    assert!(h.keys("cache/").await.is_empty());
    let (_, cache, _) = h.fetch(WSUS, "/obj/1000", None).await;
    assert_eq!(cache, "MISS");
    assert!(!h.keys("cache/windowsupdate/").await.is_empty());

    // Disabled platform.
    h.settings
        .update(|s| {
            s.platforms
                .insert("epic".into(), PlatformSettings { enabled: false });
        })
        .await
        .unwrap();
    let before = h.count("/obj/5000");
    for _ in 0..2 {
        let (_, cache, body) = h.fetch(EPIC, "/obj/5000", None).await;
        assert_eq!((cache.as_str(), body.len()), ("BYPASS", 5000));
    }
    assert_eq!(h.count("/obj/5000"), before + 2);
    assert!(h.keys("cache/epic/").await.is_empty());
}

#[tokio::test]
async fn post_passthrough() {
    let h = harness().await;
    let resp = h
        .req(reqwest::Method::POST, EPIC, "/echo")
        .body("hello upstream")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(header(&resp, STATUS_HEADER), "BYPASS");
    assert_eq!(header(&resp, "x-seen-processed-by"), "test-instance");
    assert_eq!(header(&resp, "x-method"), "POST");
    assert_eq!(resp.text().await.unwrap(), "hello upstream");
}

#[tokio::test]
async fn loop_detection() {
    let h = harness().await;
    let resp = h
        .get(EPIC, "/obj/1000")
        .header("x-gamecacher-processed-by", "test-instance")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::LOOP_DETECTED);
    assert_eq!(h.count("/obj/1000"), 0);

    // Upstream resolving to one of our own IPs is a loop too.
    let h = harness_with(Opts {
        resolver: Some(Arc::new(LoopGuard::new(
            Arc::new(StaticResolver(LOCALHOST)),
            vec![LOCALHOST],
        ))),
        ..Opts::default()
    })
    .await;
    let (s, _, _) = h.fetch(EPIC, "/obj/1000", None).await;
    assert_eq!(s, StatusCode::LOOP_DETECTED);
    let (s, _, _) = h.fetch(EPIC, "/obj/1000?nocache=1", None).await;
    assert_eq!(s, StatusCode::LOOP_DETECTED);
}

#[tokio::test]
async fn dead_upstream_is_502() {
    let h = harness_with(Opts {
        dead_upstream: true,
        ..Opts::default()
    })
    .await;
    let (s, _, _) = h.fetch(EPIC, "/obj/1000", None).await;
    assert_eq!(s, StatusCode::BAD_GATEWAY);
    let (s, _, _) = h.fetch(EPIC, "/obj/1000?nocache=1", None).await;
    assert_eq!(s, StatusCode::BAD_GATEWAY);
}

#[tokio::test]
async fn steam_user_agent_on_any_host() {
    let h = harness().await;
    let resp = h
        .get("lancache.example.net", "/obj/1000")
        .header("user-agent", STEAM_UA)
        .send()
        .await
        .unwrap();
    assert_eq!(header(&resp, STATUS_HEADER), "MISS");
    assert_eq!(resp.bytes().await.unwrap(), data(1000));
    let k = ObjectKey::new("steam", "/obj/1000");
    assert!(h.storage.exists(&k.meta_key()).await.unwrap());
    assert!(h.keys("cache/steam/").await.len() == 2);
}

#[tokio::test]
async fn steam_user_agent_is_not_an_open_relay() {
    let h = harness().await;
    // IP literals in URL-parser spellings, and URL-altering characters.
    for host in ["127.0.0.1/", "0x7f.1", "2130706433", "evil.com@127.0.0.1"] {
        let resp = h
            .get(host, "/obj/1000")
            .header("user-agent", STEAM_UA)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN, "{host}");
    }
    // Non-GET with the Steam UA on a non-platform host is never relayed.
    let resp = h
        .req(reqwest::Method::POST, "lancache.example.net", "/echo")
        .header("user-agent", STEAM_UA)
        .body("x")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    // Same when Steam is disabled: UA-only traffic isn't passed through.
    h.settings
        .update(|s| {
            s.platforms.insert(
                "steam".into(),
                game_cacher::settings::PlatformSettings { enabled: false },
            );
        })
        .await
        .unwrap();
    let resp = h
        .get("lancache.example.net", "/obj/1000")
        .header("user-agent", STEAM_UA)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn absurd_upstream_size_is_refused() {
    let h = harness().await;
    let resp = h
        .get(EPIC, "/huge")
        .header("range", "bytes=1152921504606846975-")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
    assert!(h.keys("cache/epic/").await.is_empty(), "nothing cached");
}

#[tokio::test]
async fn etag_hidden_for_blizzard_only() {
    let h = harness().await;
    for _ in 0..2 {
        let resp = h.get(BLIZZARD, "/etag/1000").send().await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(resp.headers().get(ETAG).is_none());
        assert!(resp.headers().get("last-modified").is_none());
        let resp = h.get(EPIC, "/etag/1000").send().await.unwrap();
        assert_eq!(header(&resp, ETAG.as_str()), "\"v1\"");
        assert_eq!(
            header(&resp, "last-modified"),
            "Wed, 01 Jan 2025 00:00:00 GMT"
        );
    }
}

#[tokio::test]
async fn concurrent_requests_coalesce() {
    let h = Arc::new(harness().await);
    let tasks: Vec<_> = (0..20)
        .map(|_| {
            let h = h.clone();
            tokio::spawn(async move { h.fetch(EPIC, "/obj/300000", None).await })
        })
        .collect();
    for t in tasks {
        let (s, _, body) = t.await.unwrap();
        assert_eq!(s, StatusCode::OK);
        assert_eq!(body, data(300000));
    }
    assert_eq!(h.count("/obj/300000"), 5); // ceil(300000 / 65536)
}

#[tokio::test]
async fn query_variants_share_entry() {
    let h = harness().await;
    let (_, cache, _) = h.fetch(EPIC, "/obj/100000?token=a", None).await;
    assert_eq!(cache, "MISS");
    let (_, cache, body) = h.fetch(EPIC, "/obj/100000?token=b", None).await;
    assert_eq!(cache, "HIT");
    assert_eq!(body, data(100000));
    assert_eq!(h.count("/obj/100000"), 2);
}

#[tokio::test]
async fn object_changed_upstream() {
    let h = harness().await;
    h.up.mutable_size.store(200000, Ordering::SeqCst);
    let (s, _, _) = h.fetch(EPIC, "/mutable", Some("bytes=0-9")).await;
    assert_eq!(s, StatusCode::PARTIAL_CONTENT);
    h.up.mutable_size.store(250000, Ordering::SeqCst);
    let (s, _, _) = h.fetch(EPIC, "/mutable", Some("bytes=150000-150009")).await;
    assert_eq!(s, StatusCode::BAD_GATEWAY);
    assert!(
        h.keys(&ObjectKey::new("epic", "/mutable").prefix())
            .await
            .is_empty()
    );
    let (s, cache, body) = h.fetch(EPIC, "/mutable", None).await;
    assert_eq!((s, cache.as_str()), (StatusCode::OK, "MISS"));
    assert_eq!(body, data(250000));
}

#[tokio::test]
async fn eviction_removes_oldest() {
    let h = harness_with(Opts {
        max_bytes: Some(300_000),
        ..Opts::default()
    })
    .await;
    for p in ["/obj/100000", "/obj/100001", "/obj/100002"] {
        h.fetch(EPIC, p, None).await;
    }
    assert!(h.index.usage().total_bytes > 300_000);
    assert_eq!(h.index.evict_if_needed().await.unwrap(), 1);
    assert!(h.index.usage().total_bytes <= 270_000);
    assert!(
        h.keys(&ObjectKey::new("epic", "/obj/100000").prefix())
            .await
            .is_empty()
    );
    assert!(
        !h.keys(&ObjectKey::new("epic", "/obj/100001").prefix())
            .await
            .is_empty()
    );
    // The engine forgot the evicted meta, so the object is fetched again.
    let (_, cache, body) = h.fetch(EPIC, "/obj/100000", None).await;
    assert_eq!((cache.as_str(), body), ("MISS", Bytes::from(data(100000))));
    assert_eq!(h.count("/obj/100000"), 4);
}

#[tokio::test]
async fn index_rebuild_and_snapshot() {
    let h = harness().await;
    h.fetch(EPIC, "/obj/100000", None).await;
    h.fetch(BLIZZARD, "/obj/2000", None).await;
    let usage = h.index.usage();
    assert_eq!(usage.objects, 2);

    h.index.snapshot().await.unwrap();
    let loaded = LruIndex::load_or_rebuild(h.storage.clone(), Some(10), Metrics::new())
        .await
        .unwrap();
    assert_eq!(loaded.usage(), usage);
    assert_eq!(loaded.max_bytes(), Some(10));

    h.storage.delete("index/lru.json").await.unwrap();
    let rebuilt = LruIndex::load_or_rebuild(h.storage.clone(), None, Metrics::new())
        .await
        .unwrap();
    assert_eq!(rebuilt.usage(), usage);
}

#[tokio::test]
async fn prefill() {
    let h = harness().await;
    let e = h.engine.clone();
    let r = e.warm("epic", EPIC, "/obj/200000?sig=1").await.unwrap();
    assert_eq!((r.bytes_downloaded, r.bytes_already_cached), (200000, 0));
    let r = e.warm("epic", EPIC, "/obj/200000").await.unwrap();
    assert_eq!((r.bytes_downloaded, r.bytes_already_cached), (0, 200000));
    assert_eq!(h.count("/obj/200000"), 4);

    assert_eq!(
        e.get("epic", EPIC, "/obj/200000").await.unwrap(),
        data(200000)
    );
    let part = e
        .get_range("epic", EPIC, "/obj/200000", 65000, 140000)
        .await
        .unwrap();
    assert_eq!(part, data(200000)[65000..=140000]);
    assert_eq!(h.count("/obj/200000"), 4);
    assert!(
        e.get_range("epic", EPIC, "/obj/200000", 0, 200000)
            .await
            .is_err()
    );

    // Partially cached object: get_range populates only what it needs, warm the rest.
    e.get_range("epic", EPIC, "/obj/300000", 0, 10)
        .await
        .unwrap();
    let r = e.warm("epic", EPIC, "/obj/300000").await.unwrap();
    assert_eq!(r.bytes_already_cached, SLICE);
    assert_eq!(r.bytes_downloaded, 300000 - SLICE);

    // Identifier need not match the host; a later LAN request is a HIT.
    let body = e
        .get("steam", "depot.example.net", "/obj/1000")
        .await
        .unwrap();
    assert_eq!(body, data(1000));
    assert_eq!(h.keys("cache/steam/").await.len(), 2);
    let resp = h
        .get("other.example.net", "/obj/1000")
        .header("user-agent", STEAM_UA)
        .send()
        .await
        .unwrap();
    assert_eq!(header(&resp, STATUS_HEADER), "HIT");

    // Range-ignoring upstream and bypassed paths.
    let r = e.warm("epic", EPIC, "/norange/100000").await.unwrap();
    assert_eq!(r.bytes_downloaded, 100000);
    assert_eq!(
        e.get("epic", EPIC, "/norange/100000").await.unwrap(),
        data(100000)
    );
    let cab = e
        .get("windowsupdate", WSUS, "/x/authrootstl.cab")
        .await
        .unwrap();
    assert_eq!(cab, data(1000));
    assert!(h.keys("cache/windowsupdate/").await.is_empty());

    assert!(e.get("epic", EPIC, "/missing").await.is_err());
    assert!(e.warm("epic", EPIC, "/missing").await.is_err());
    assert!(e.get("nope", EPIC, "/obj/1").await.is_err());
}
