//! End-to-end managed updates for Blizzard (TACT) and Windows Update through the real
//! admin API and cache listener, against a local patch server + CDN.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use clap::Parser;
use game_cacher::app::{self, Overrides, RunningApp};
use game_cacher::config::{Config, EnvSecrets};
use game_cacher::platforms::blizzard::Blizzard;
use game_cacher::platforms::windows_update::WindowsUpdate;
use game_cacher::secrets::SecretStore;
use game_cacher::storage::{DynStorage, MemoryStorage};
use game_cacher::upstream::StaticResolver;
use serde_json::{Value, json};

const PW: &str = "hunter2";
const BLIZZ_HOST: &str = "level3.blizzard.com";
const WU_HOST: &str = "download.windowsupdate.com";

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

// --- synthetic TACT content ------------------------------------------------

const ARCHIVE: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const ARCHIVED_EKEY: [u8; 16] = [0xa1; 16];
const LOOSE_EKEY: [u8; 16] = [0x1d; 16];
const ENC_EKEY: &str = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";
const BUILD_CFG: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const CDN_CFG: &str = "cccccccccccccccccccccccccccccccc";

fn cdn_path(kind: &str, hash: &str, suffix: &str) -> String {
    format!(
        "/tpr/wow/{kind}/{}/{}/{hash}{suffix}",
        &hash[0..2],
        &hash[2..4]
    )
}

fn pattern(len: usize, seed: u8) -> Vec<u8> {
    (0..len).map(|i| (i as u8).wrapping_add(seed)).collect()
}

/// Archive index: one 4 KiB block of 16-byte key + 4-byte size + 4-byte offset, then footer.
fn archive_index(keys: &[[u8; 16]]) -> Vec<u8> {
    let mut block = Vec::new();
    for k in keys {
        block.extend(k);
        block.extend(100u32.to_be_bytes());
        block.extend(0u32.to_be_bytes());
    }
    block.resize(4096, 0);
    block.extend([0u8; 8]);
    block.extend([1, 0, 0, 4, 4, 4, 16, 8]);
    block.extend((keys.len() as u32).to_le_bytes());
    block.extend([0u8; 8]);
    block
}

/// Encoding file with a single CEKey page, wrapped in a one-chunk raw BLTE.
fn encoding_blte(ekeys: &[[u8; 16]]) -> Vec<u8> {
    let mut f = b"EN".to_vec();
    f.extend([1, 16, 16, 0, 1, 0, 1]);
    f.extend(1u32.to_be_bytes());
    f.extend(0u32.to_be_bytes());
    f.push(0);
    f.extend(0u32.to_be_bytes());
    f.extend([0u8; 32]); // page index entry
    let mut page = Vec::new();
    for (i, e) in ekeys.iter().enumerate() {
        page.push(1);
        page.extend([0, 0, 0, 0, 1]);
        page.extend([i as u8 + 1; 16]);
        page.extend(e);
    }
    page.resize(1024, 0);
    f.extend(page);
    let mut out = b"BLTE".to_vec();
    out.extend(0u32.to_be_bytes());
    out.push(b'N');
    out.extend(f);
    out
}

type Files = Arc<HashMap<String, Vec<u8>>>;

fn content() -> HashMap<String, Vec<u8>> {
    let mut m = HashMap::new();
    let versions = format!(
        "Region!STRING:0|BuildConfig!HEX:16|CDNConfig!HEX:16|VersionsName!String:0\n## seqn = 1\nus|{BUILD_CFG}|{CDN_CFG}|1.0.0\n"
    );
    let cdns = format!(
        "Name!STRING:0|Path!STRING:0|Hosts!STRING:0\n## seqn = 1\nus|tpr/wow|{BLIZZ_HOST}\n"
    );
    m.insert("/wow/versions".into(), versions.into_bytes());
    m.insert("/wow/cdns".into(), cdns.into_bytes());
    m.insert(
        cdn_path("config", BUILD_CFG, ""),
        format!("root = r1 r2\nencoding = ec1 {ENC_EKEY}\n").into_bytes(),
    );
    m.insert(
        cdn_path("config", CDN_CFG, ""),
        format!("archives = {ARCHIVE}\n").into_bytes(),
    );
    m.insert(cdn_path("data", ARCHIVE, ""), pattern(150_000, 3));
    m.insert(
        cdn_path("data", ARCHIVE, ".index"),
        archive_index(&[ARCHIVED_EKEY]),
    );
    m.insert(
        cdn_path("data", ENC_EKEY, ""),
        encoding_blte(&[ARCHIVED_EKEY, LOOSE_EKEY]),
    );
    m.insert(cdn_path("data", &hex(&LOOSE_EKEY), ""), pattern(70_000, 9));
    m.insert("/d/msdownload/update/x.cab".into(), pattern(90_000, 5));
    m
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

async fn serve_file(State(files): State<Files>, uri: Uri, headers: HeaderMap) -> Response {
    let Some(body) = files.get(uri.path()) else {
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
    body.clone().into_response()
}

async fn content_server() -> SocketAddr {
    let router = Router::new()
        .fallback(serve_file)
        .with_state(Arc::new(content()));
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, router).await.unwrap() });
    addr
}

// --- harness ---------------------------------------------------------------

struct Harness {
    app: RunningApp,
    client: reqwest::Client,
}

impl Harness {
    async fn new() -> Self {
        let up = content_server().await;
        let port = up.port().to_string();
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
                platforms: Some(vec![
                    Arc::new(Blizzard::with_patch_base(&format!("http://{up}"))),
                    Arc::new(WindowsUpdate::new()),
                ]),
                scheduler_period: Some(Duration::from_secs(3600)),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        // The scheduler ticks once immediately; let that pass while nothing is selected so
        // it cannot start a run of its own later.
        tokio::time::sleep(Duration::from_millis(200)).await;
        Self {
            app,
            client: reqwest::Client::new(),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.app.admin_addr)
    }

    async fn api(&self, method: reqwest::Method, path: &str, body: Option<Value>) -> Value {
        let mut r = self.client.request(method, self.url(path)).bearer_auth(PW);
        if let Some(b) = body {
            r = r.json(&b);
        }
        let r = r.send().await.unwrap();
        assert!(r.status().is_success(), "{path}: {}", r.status());
        r.json().await.unwrap_or(Value::Null)
    }

    async fn select(&self, platform: &str, ids: &[&str]) {
        self.api(
            reqwest::Method::PUT,
            &format!("/api/platforms/{platform}/managed/selection"),
            Some(json!({ "selected": ids })),
        )
        .await;
    }

    /// Starts a run and waits for it; returns the per-game reports.
    async fn run(&self, platform: &str) -> Vec<Value> {
        let status = format!("/api/platforms/{platform}/managed");
        let before =
            self.api(reqwest::Method::GET, &status, None).await["last_run"]["started"].clone();
        self.api(
            reqwest::Method::POST,
            &format!("{status}/run"),
            Some(json!({})),
        )
        .await;
        for _ in 0..200 {
            let s = self.api(reqwest::Method::GET, &status, None).await;
            let run = &s["last_run"];
            if !s["running"].as_bool().unwrap()
                && !run["finished"].is_null()
                && run["started"] != before
            {
                return run["games"].as_array().unwrap().clone();
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("{platform} run did not finish");
    }

    async fn cache_get(&self, host: &str, path: &str) -> reqwest::Response {
        self.client
            .get(format!("http://{}{path}", self.app.http_addr))
            .header("host", host)
            .send()
            .await
            .unwrap()
    }

    async fn assert_hit(&self, host: &str, path: &str, len: usize) {
        let r = self.cache_get(host, path).await;
        assert_eq!(r.status(), StatusCode::OK, "{path}");
        assert_eq!(r.headers()["x-gamecacher-status"], "HIT", "{path}");
        assert_eq!(r.bytes().await.unwrap().len(), len, "{path}");
    }
}

#[tokio::test]
async fn blizzard_prefill_verify_and_repair() {
    let h = Harness::new().await;
    h.select("blizzard", &["wow"]).await;

    let first = h.run("blizzard").await;
    let r = &first[0];
    assert_eq!(r["game_id"], "wow");
    assert_eq!(r["error"], Value::Null, "{r}");
    assert_eq!(r["updated"], true);
    assert_eq!(r["current_version"], "1.0.0");
    // archive, its index, the encoding file and one loose file.
    assert_eq!(r["files"], 4);
    assert!(r["bytes_downloaded"].as_u64().unwrap() >= 150_000 + 70_000);

    let archive = cdn_path("data", ARCHIVE, "");
    let loose = cdn_path("data", &hex(&LOOSE_EKEY), "");
    h.assert_hit(BLIZZ_HOST, &archive, 150_000).await;
    h.assert_hit(BLIZZ_HOST, &loose, 70_000).await;

    // Nothing changed and everything is cached: verified, not updated.
    let again = h.run("blizzard").await;
    let r = &again[0];
    assert_eq!(r["error"], Value::Null, "{r}");
    assert_eq!(r["updated"], false);
    assert_eq!(r["bytes_downloaded"], 0);
    assert!(r["bytes_already_cached"].as_u64().unwrap() >= 220_000);

    // Purge, then the next run repairs the cache even though the build is unchanged.
    let purged = h
        .api(
            reqwest::Method::DELETE,
            "/api/platforms/blizzard/cache",
            None,
        )
        .await;
    assert!(purged["deleted_objects"].as_u64().unwrap() > 0);
    let repaired = h.run("blizzard").await;
    let r = &repaired[0];
    assert_eq!(r["error"], Value::Null, "{r}");
    assert_eq!(r["updated"], true);
    assert!(r["bytes_downloaded"].as_u64().unwrap() >= 220_000);
    h.assert_hit(BLIZZ_HOST, &archive, 150_000).await;
    h.assert_hit(BLIZZ_HOST, &loose, 70_000).await;
    h.app.shutdown().await;
}

#[tokio::test]
async fn windows_update_prefill_and_foreign_host_error() {
    let h = Harness::new().await;
    let good = format!("http://{WU_HOST}/d/msdownload/update/x.cab");
    let foreign = "http://example.com/x.cab";
    h.select("windowsupdate", &[&good, foreign]).await;

    let games = h.run("windowsupdate").await;
    let by_id = |id: &str| games.iter().find(|g| g["game_id"] == id).unwrap();
    let ok = by_id(&good);
    assert_eq!(ok["error"], Value::Null, "{ok}");
    assert_eq!(ok["updated"], true);
    assert_eq!(ok["bytes_downloaded"], 90_000);
    let bad = by_id(foreign);
    assert!(
        bad["error"]
            .as_str()
            .unwrap()
            .contains("not a Windows Update"),
        "{bad}"
    );
    assert_eq!(bad["updated"], false);

    h.assert_hit(WU_HOST, "/d/msdownload/update/x.cab", 90_000)
        .await;
    h.app.shutdown().await;
}
