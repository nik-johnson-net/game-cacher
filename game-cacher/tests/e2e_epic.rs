//! End-to-end Epic managed update through the real admin API and cache listener.
//! Epic's OAuth/launcher/catalog APIs are wiremock; content comes from a local server.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use axum::Router;
use axum::response::IntoResponse;
use axum::routing::get;
use clap::Parser;
use game_cacher::app::{self, Overrides};
use game_cacher::config::{Config, EnvSecrets};
use game_cacher::platforms::DynPlatform;
use game_cacher::platforms::epic::manifest::{Chunk, testutil::build_manifest};
use game_cacher::platforms::epic::{Endpoints, Epic};
use game_cacher::secrets::SecretStore;
use game_cacher::storage::{DynStorage, MemoryStorage};
use game_cacher::upstream::StaticResolver;
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const PW: &str = "hunter2";
const CDN_HOST: &str = "download.epicgames.com";
const BASE: &str = "/Builds/Org/o-1/abc/default";
const CHUNK_SIZE: usize = 100 * 1024;

#[derive(Default)]
struct MemSecrets(Mutex<HashMap<String, String>>);

#[async_trait]
impl SecretStore for MemSecrets {
    fn name(&self) -> &'static str {
        "memory"
    }
    async fn get(&self, name: &str) -> Result<Option<String>> {
        Ok(self.0.lock().unwrap().get(name).cloned())
    }
    async fn put(&self, name: &str, value: &str) -> Result<()> {
        self.0.lock().unwrap().insert(name.into(), value.into());
        Ok(())
    }
    async fn delete(&self, name: &str) -> Result<()> {
        self.0.lock().unwrap().remove(name);
        Ok(())
    }
}

fn chunks() -> Vec<Chunk> {
    (1..=2u32)
        .map(|i| Chunk {
            guid: [i, 0, 0, 0xFF],
            hash: i as u64 * 16,
            group: i as u8,
            window_size: CHUNK_SIZE as u32,
            file_size: CHUNK_SIZE as i64,
        })
        .collect()
}

/// Serves the manifest and deterministic chunk bodies, with Range support.
async fn content_server(manifest: Vec<u8>) -> SocketAddr {
    let manifest = Arc::new(manifest);
    let handler = move |axum::extract::Path(p): axum::extract::Path<String>,
                        headers: axum::http::HeaderMap| {
        let manifest = manifest.clone();
        async move {
            let body: Vec<u8> = if p.ends_with(".manifest") {
                manifest.to_vec()
            } else if p.ends_with(".chunk") {
                (0..CHUNK_SIZE).map(|i| (i % 241) as u8).collect()
            } else {
                return StatusCode::NOT_FOUND.into_response();
            };
            let total = body.len();
            if let Some(r) = headers.get("range").and_then(|v| v.to_str().ok()) {
                let (a, b) = r.trim_start_matches("bytes=").split_once('-').unwrap();
                let a: usize = a.parse().unwrap();
                let b: usize = b.parse::<usize>().unwrap_or(total - 1).min(total - 1);
                return (
                    StatusCode::PARTIAL_CONTENT,
                    [("content-range", format!("bytes {a}-{b}/{total}"))],
                    body[a..=b].to_vec(),
                )
                    .into_response();
            }
            body.into_response()
        }
    };
    let router = Router::new().route("/{*p}", get(handler));
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, router).await.unwrap() });
    addr
}

async fn mock_epic_apis(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/account/api/oauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "at", "refresh_token": "rt", "expires_in": 7200,
            "account_id": "acc1", "displayName": "Player One"})))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/launcher/api/public/assets/Windows"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"appName": "Sugar", "labelName": "Live-Windows", "buildVersion": "1.0.0",
             "catalogItemId": "cat1", "namespace": "ns1"}])))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/catalog/api/shared/namespace/ns1/bulk/items"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"cat1": {"title": "Sugar Rush"}})),
        )
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path(
            "/launcher/api/public/assets/v2/platform/Windows/namespace/ns1/catalogItem/cat1/app/Sugar/label/Live",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"elements": [{
            "appName": "Sugar",
            "manifests": [{"uri": format!("https://{CDN_HOST}{BASE}/m.manifest"),
                           "queryParams": []}]
        }]})))
        .mount(server)
        .await;
}

struct Harness {
    app: app::RunningApp,
    client: reqwest::Client,
}

impl Harness {
    async fn call(&self, m: Method, path: &str, body: Option<Value>) -> (StatusCode, Value) {
        let url = format!("http://{}{path}", self.app.admin_addr);
        let mut r = self.client.request(m, url).bearer_auth(PW);
        if let Some(b) = body {
            r = r.json(&b);
        }
        let r = r.send().await.unwrap();
        let s = r.status();
        (s, r.json().await.unwrap_or(Value::Null))
    }

    async fn status(&self) -> Value {
        self.call(Method::GET, "/api/platforms/epic/managed", None)
            .await
            .1
    }

    /// Starts a run and waits for it to finish; returns the Sugar report.
    async fn run_update(&self) -> Value {
        let prev = self.status().await["last_run"]["started"].clone();
        let (s, _) = self
            .call(Method::POST, "/api/platforms/epic/managed/run", None)
            .await;
        assert_eq!(s, StatusCode::ACCEPTED);
        for _ in 0..200 {
            let st = self.status().await;
            let run = &st["last_run"];
            if run["started"] != prev && !run["finished"].is_null() {
                return run["games"][0].clone();
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("update run did not finish");
    }

    /// LAN-client GET of a chunk; returns (status, X-GameCacher-Status).
    async fn chunk_status(&self, chunk: &Chunk) -> (StatusCode, String) {
        let url = format!("http://{}{BASE}/{}", self.app.http_addr, chunk.path(18));
        let r = self
            .client
            .get(url)
            .header("host", CDN_HOST)
            .send()
            .await
            .unwrap();
        let st = r
            .headers()
            .get("x-gamecacher-status")
            .map(|v| v.to_str().unwrap().to_string())
            .unwrap_or_default();
        let s = r.status();
        assert_eq!(r.bytes().await.unwrap().len(), CHUNK_SIZE);
        (s, st)
    }
}

async fn start_app(content: SocketAddr, epic: Epic) -> Harness {
    let port = content.port().to_string();
    let config = Config::parse_from([
        "game-cacher",
        "--http-listen",
        "127.0.0.1:0",
        "--https-listen",
        "127.0.0.1:0",
        "--dns-listen",
        "127.0.0.1:0",
        "--admin-listen",
        "127.0.0.1:0",
        "--allow-private-upstream",
        "--cache-ip",
        "10.99.0.1",
        "--slice-size",
        "65536",
        "--upstream-http-port",
        &port,
        "--ui-path",
        "/nonexistent",
    ]);
    let env = EnvSecrets {
        admin_password: Some(PW.into()),
        ..Default::default()
    };
    let storage: DynStorage = Arc::new(MemoryStorage::new());
    let app = app::start(
        config,
        env,
        Overrides {
            resolver: Some(Arc::new(StaticResolver([127, 0, 0, 1].into()))),
            storage: Some(storage),
            secrets: Some(Arc::new(MemSecrets::default())),
            platforms: Some(vec![Arc::new(epic) as DynPlatform]),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    Harness {
        app,
        client: reqwest::Client::new(),
    }
}

#[tokio::test]
async fn epic_login_select_run_purge_repair() {
    let chunks = chunks();
    let content = content_server(build_manifest("Sugar", "1.0.0", 18, &chunks, true)).await;
    let epic_api = MockServer::start().await;
    mock_epic_apis(&epic_api).await;
    let uri = epic_api.uri();
    let epic = Epic::with_endpoints(Endpoints {
        oauth: uri.clone(),
        launcher: uri.clone(),
        catalog: uri,
    });
    let h = start_app(content, epic).await;

    let (s, v) = h
        .call(
            Method::POST,
            "/api/platforms/epic/managed/login",
            Some(json!({"method": "auth_code", "code": "abc123"})),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v, json!({"status": "logged_in", "account": "Player One"}));

    let (s, v) = h
        .call(Method::GET, "/api/platforms/epic/managed/games", None)
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v[0]["id"], "Sugar");
    assert_eq!(v[0]["name"], "Sugar Rush");

    let (s, v) = h
        .call(
            Method::PUT,
            "/api/platforms/epic/managed/selection",
            Some(json!({"selected": ["Sugar"]})),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["selected"], json!(["Sugar"]));

    let total = (2 * CHUNK_SIZE) as u64;
    let r = h.run_update().await;
    assert_eq!(r["error"], Value::Null);
    assert_eq!(r["name"], "Sugar Rush");
    assert_eq!(r["updated"], true);
    assert_eq!(r["files"], 2);
    assert_eq!(r["bytes_downloaded"], total);
    assert_eq!(r["current_version"], "1.0.0");
    for c in &chunks {
        assert_eq!(h.chunk_status(c).await, (StatusCode::OK, "HIT".into()));
    }

    // Unchanged build and warm cache: verified, nothing downloaded.
    let r = h.run_update().await;
    assert_eq!(r["error"], Value::Null);
    assert_eq!(r["updated"], false);
    assert_eq!(r["bytes_downloaded"], 0);
    assert_eq!(r["bytes_already_cached"], total);

    // Purge, then the next run repairs the cache although the build is unchanged.
    let (s, _) = h
        .call(Method::DELETE, "/api/platforms/epic/cache", None)
        .await;
    assert_eq!(s, StatusCode::OK);
    let r = h.run_update().await;
    assert_eq!(r["error"], Value::Null);
    assert_eq!(r["updated"], true);
    assert!(r["bytes_downloaded"].as_u64().unwrap() >= total);
    for c in &chunks {
        assert_eq!(h.chunk_status(c).await, (StatusCode::OK, "HIT".into()));
    }
    h.app.shutdown().await;
}
