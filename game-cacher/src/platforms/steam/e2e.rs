//! End-to-end Steam managed update through the real admin API and cache
//! listener. The Steam network (CM / auth) is a fake backend; content comes
//! from a local server standing in for the Steam CDN.

use std::collections::{BTreeSet, HashMap};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use axum::Router;
use axum::response::IntoResponse;
use axum::routing::get;
use bytes::Bytes;
use clap::Parser;
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

use super::appinfo::{AppInfo, DepotInfo};
use super::backend::*;
use super::manifest::tests::build_manifest;
use super::*;
use crate::app::{self, Overrides};
use crate::config::{Config, EnvSecrets};
use crate::platforms::DynPlatform;
use crate::secrets::SecretStore;
use crate::storage::{DynStorage, MemoryStorage};
use crate::upstream::StaticResolver;

const PW: &str = "hunter2";
const CDN_HOST: &str = "cache1.steamcontent.com";
const APP: u32 = 100;
const DEPOT: u32 = 101;
const GID: u64 = 555;
const CODE: u64 = 4242;
const CHUNK_SIZE: usize = 150 * 1024;
const SHAS: [[u8; 20]; 2] = [[0x11; 20], [0x22; 20]];

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

/// Fake Steam: one owned game with one depot, one CDN server, no login flow.
struct FakeSteam;

#[async_trait]
impl SteamAuth for FakeSteam {
    async fn password_rsa_key(&self, _: &str) -> Result<RsaKey> {
        anyhow::bail!("login not used")
    }
    async fn begin_qr(&self) -> Result<AuthSessionStart> {
        anyhow::bail!("login not used")
    }
    async fn begin_credentials(&self, _: &str, _: &str, _: u64) -> Result<AuthSessionStart> {
        anyhow::bail!("login not used")
    }
    async fn submit_guard_code(&self, _: u64, _: u64, _: &str, _: GuardKind) -> Result<()> {
        anyhow::bail!("login not used")
    }
    async fn poll(&self, _: u64, _: &[u8]) -> Result<PollResult> {
        anyhow::bail!("login not used")
    }
}

#[async_trait]
impl SteamBackend for FakeSteam {
    async fn connect(&self, _: &str, _: &str) -> Result<Box<dyn SteamSession>> {
        Ok(Box::new(FakeSteam))
    }
}

#[async_trait]
impl SteamSession for FakeSteam {
    fn renewed_refresh_token(&self) -> Option<String> {
        None
    }
    async fn ownership(&self) -> Result<Ownership> {
        Ok(Ownership {
            app_ids: BTreeSet::from([APP]),
            depot_ids: BTreeSet::from([DEPOT]),
        })
    }
    async fn app_info(&self, ids: &[u32]) -> Result<Vec<AppInfo>> {
        Ok(ids
            .contains(&APP)
            .then(|| AppInfo {
                app_id: APP,
                name: "Test Game".into(),
                app_type: "game".into(),
                public_buildid: Some("77".into()),
                depots: vec![DepotInfo {
                    depot_id: DEPOT,
                    has_manifests: true,
                    public_manifest: Some(GID),
                    size: Some(1000),
                    ..Default::default()
                }],
                ..Default::default()
            })
            .into_iter()
            .collect())
    }
    async fn manifest_request_code(&self, _: u32, _: u32, _: u64, _: &str) -> Result<u64> {
        Ok(CODE)
    }
    async fn cdn_servers(&self) -> Result<Vec<CdnServer>> {
        Ok(vec![CdnServer {
            kind: "CDN".into(),
            host: "ip-1".into(),
            vhost: CDN_HOST.into(),
            ..Default::default()
        }])
    }
}

fn chunk_body(sha: &[u8; 20]) -> Vec<u8> {
    (0..CHUNK_SIZE).map(|i| (i % 251) as u8 ^ sha[0]).collect()
}

/// Serves the depot manifest and deterministic chunk bodies, with Range support.
async fn content_server() -> SocketAddr {
    let manifest = Bytes::from(build_manifest(DEPOT, GID, &[&SHAS[..1], &SHAS[1..]]));
    let manifest_path = format!("/depot/{DEPOT}/manifest/{GID}/5/{CODE}");
    let handler = move |uri: axum::http::Uri, headers: axum::http::HeaderMap| {
        let manifest = manifest.clone();
        let manifest_path = manifest_path.clone();
        async move {
            let p = uri.path();
            let chunk_prefix = format!("/depot/{DEPOT}/chunk/");
            let body: Vec<u8> = if p == manifest_path {
                manifest.to_vec()
            } else if let Some(sha) = p.strip_prefix(&chunk_prefix) {
                match SHAS.iter().find(|s| hex::encode(s) == sha) {
                    Some(s) => chunk_body(s),
                    None => return StatusCode::NOT_FOUND.into_response(),
                }
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
    let router = Router::new().fallback(get(handler));
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, router).await.unwrap() });
    addr
}

struct Harness {
    app: app::RunningApp,
    client: reqwest::Client,
    secrets: Arc<MemSecrets>,
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
        self.call(Method::GET, "/api/platforms/steam/managed", None)
            .await
            .1
    }

    /// Starts a run and waits for it to finish; returns the game's report.
    async fn run_update(&self) -> Value {
        let prev = self.status().await["last_run"]["started"].clone();
        // The previous run may still be releasing its running flag.
        let mut started = false;
        for _ in 0..100 {
            let (s, b) = self
                .call(Method::POST, "/api/platforms/steam/managed/run", None)
                .await;
            if s == StatusCode::ACCEPTED {
                started = true;
                break;
            }
            assert_eq!(s, StatusCode::CONFLICT, "{b}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(started, "run never started");
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

    /// LAN-client GET of a chunk as the Steam client; returns X-GameCacher-Status.
    async fn chunk_status(&self, sha: &[u8; 20]) -> String {
        let url = format!(
            "http://{}/depot/{DEPOT}/chunk/{}",
            self.app.http_addr,
            hex::encode(sha)
        );
        let r = self
            .client
            .get(url)
            .header("host", "lan-client.example")
            .header("user-agent", "Valve/Steam HTTP Client 1.0")
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::OK);
        let st = r
            .headers()
            .get("x-gamecacher-status")
            .map(|v| v.to_str().unwrap().to_string())
            .unwrap_or_default();
        assert_eq!(r.bytes().await.unwrap().to_vec(), chunk_body(sha));
        st
    }
}

async fn start_app(content: SocketAddr) -> Harness {
    start_app_with(content, Arc::new(FakeSteam), true).await
}

/// Starts the app on `backend`; `seeded` pre-stores a completed login.
async fn start_app_with(
    content: SocketAddr,
    backend: Arc<dyn SteamBackend>,
    seeded: bool,
) -> Harness {
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
    let secrets = Arc::new(MemSecrets::default());
    if seeded {
        // Pre-seed what a completed login would have stored.
        storage
            .put(
                "state/steam.json",
                Bytes::from(json!({"account": "alice"}).to_string()),
            )
            .await
            .unwrap();
        secrets.put(REFRESH_TOKEN_SECRET, "refresh").await.unwrap();
    }
    let steam = Steam {
        managed: SteamManaged::new(Box::new(move |_| backend.clone()), auth::SESSION_TTL),
    };
    let app = app::start(
        config,
        env,
        Overrides {
            resolver: Some(Arc::new(StaticResolver([127, 0, 0, 1].into()))),
            storage: Some(storage),
            secrets: Some(secrets.clone()),
            platforms: Some(vec![Arc::new(steam) as DynPlatform]),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    Harness {
        app,
        client: reqwest::Client::new(),
        secrets,
    }
}

#[tokio::test]
async fn steam_select_run_purge_repair() {
    let h = start_app(content_server().await).await;
    // The scheduler ticks once at startup; let it pass with nothing selected so
    // it cannot start a run of its own after the selection below.
    tokio::time::sleep(Duration::from_millis(300)).await;

    let (s, v) = h
        .call(
            Method::PUT,
            "/api/platforms/steam/managed/selection",
            Some(json!({"selected": [APP.to_string()]})),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["selected"], json!([APP.to_string()]));

    let total = (2 * CHUNK_SIZE) as u64;
    let r = h.run_update().await;
    assert_eq!(r["error"], Value::Null);
    assert_eq!(r["name"], "Test Game");
    assert_eq!(r["updated"], true);
    assert_eq!(r["files"], 2);
    assert_eq!(r["bytes_downloaded"], total);
    assert_eq!(r["current_version"], "77");
    for sha in &SHAS {
        assert_eq!(h.chunk_status(sha).await, "HIT");
    }

    // Unchanged build and warm cache: verified, nothing downloaded.
    let r = h.run_update().await;
    assert_eq!(r["error"], Value::Null);
    assert_eq!(r["updated"], false);
    assert_eq!(r["bytes_downloaded"], 0);
    assert_eq!(r["bytes_already_cached"], total);

    // Purge, then the next run repairs the cache although the build is unchanged.
    let (s, _) = h
        .call(Method::DELETE, "/api/platforms/steam/cache", None)
        .await;
    assert_eq!(s, StatusCode::OK);
    let r = h.run_update().await;
    assert_eq!(r["error"], Value::Null);
    assert_eq!(r["updated"], true);
    assert!(r["bytes_downloaded"].as_u64().unwrap() >= total);
    for sha in &SHAS {
        assert_eq!(h.chunk_status(sha).await, "HIT");
    }
    h.app.shutdown().await;
}

const CODE_OK: &str = "12345";

/// Scripted login: QR / credentials start fine; the poll yields a refresh
/// token once `approved` is set (by the test for QR, by a correct guard code).
struct ScriptedAuth {
    guard: GuardKind,
    approved: AtomicBool,
    rsa: rsa::RsaPrivateKey,
}

impl ScriptedAuth {
    fn new(guard: GuardKind) -> Arc<Self> {
        Arc::new(Self {
            guard,
            approved: AtomicBool::new(false),
            rsa: rsa::RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 512).unwrap(),
        })
    }
}

fn start(steam_id: u64, confirmations: Vec<GuardKind>) -> AuthSessionStart {
    AuthSessionStart {
        client_id: 9,
        request_id: vec![1, 2, 3],
        interval: 0.0,
        allowed_confirmations: confirmations,
        steam_id,
        challenge_url: "https://s.team/q/1/9".into(),
    }
}

#[async_trait]
impl SteamAuth for ScriptedAuth {
    async fn password_rsa_key(&self, _: &str) -> Result<RsaKey> {
        use rsa::traits::PublicKeyParts;
        Ok(RsaKey {
            modulus_hex: self.rsa.n().to_str_radix(16),
            exponent_hex: self.rsa.e().to_str_radix(16),
            timestamp: 1,
        })
    }
    async fn begin_qr(&self) -> Result<AuthSessionStart> {
        Ok(start(0, vec![GuardKind::DeviceConfirmation]))
    }
    async fn begin_credentials(
        &self,
        account: &str,
        enc: &str,
        _: u64,
    ) -> Result<AuthSessionStart> {
        use base64::Engine;
        let plain = self.rsa.decrypt(
            rsa::Pkcs1v15Encrypt,
            &base64::prelude::BASE64_STANDARD.decode(enc)?,
        )?;
        anyhow::ensure!(plain == b"s3cret" && account == "bob", "invalid password");
        Ok(start(76561, vec![self.guard]))
    }
    async fn submit_guard_code(&self, _: u64, _: u64, code: &str, kind: GuardKind) -> Result<()> {
        anyhow::ensure!(kind == self.guard, "wrong guard kind {kind:?}");
        anyhow::ensure!(code == CODE_OK, "invalid Steam Guard code (EResult 65)");
        self.approved.store(true, Ordering::SeqCst);
        Ok(())
    }
    async fn poll(&self, _: u64, _: &[u8]) -> Result<PollResult> {
        let approved = self.approved.load(Ordering::SeqCst);
        Ok(PollResult {
            refresh_token: approved.then(|| "rt-new".to_string()),
            // QR polls report the account; credential logins fall back to the username.
            account_name: (approved && self.guard == GuardKind::DeviceConfirmation)
                .then(|| "alice".to_string()),
            ..Default::default()
        })
    }
}

struct Scripted(Arc<ScriptedAuth>);

#[async_trait]
impl SteamAuth for Scripted {
    async fn password_rsa_key(&self, a: &str) -> Result<RsaKey> {
        self.0.password_rsa_key(a).await
    }
    async fn begin_qr(&self) -> Result<AuthSessionStart> {
        self.0.begin_qr().await
    }
    async fn begin_credentials(&self, a: &str, e: &str, t: u64) -> Result<AuthSessionStart> {
        self.0.begin_credentials(a, e, t).await
    }
    async fn submit_guard_code(&self, c: u64, s: u64, code: &str, k: GuardKind) -> Result<()> {
        self.0.submit_guard_code(c, s, code, k).await
    }
    async fn poll(&self, c: u64, r: &[u8]) -> Result<PollResult> {
        self.0.poll(c, r).await
    }
}

#[async_trait]
impl SteamBackend for Scripted {
    async fn connect(&self, _: &str, _: &str) -> Result<Box<dyn SteamSession>> {
        anyhow::bail!("no CM in login tests")
    }
}

async fn login_harness(guard: GuardKind) -> (Harness, Arc<ScriptedAuth>) {
    let script = ScriptedAuth::new(guard);
    let h = start_app_with(
        SocketAddr::from(([127, 0, 0, 1], 1)),
        Arc::new(Scripted(script.clone())),
        false,
    )
    .await;
    (h, script)
}

impl Harness {
    async fn login(&self, body: Value) -> Value {
        let (s, v) = self
            .call(
                Method::POST,
                "/api/platforms/steam/managed/login",
                Some(body),
            )
            .await;
        assert_eq!(s, StatusCode::OK, "{v}");
        v
    }

    async fn assert_logged_in(&self, account: &str) {
        let v = self.status().await;
        assert_eq!(v["auth"]["logged_in"], true, "{v}");
        assert_eq!(v["auth"]["account"], account);
        let token = self.secrets.get(REFRESH_TOKEN_SECRET).await.unwrap();
        assert_eq!(token.as_deref(), Some("rt-new"));
    }
}

#[tokio::test]
async fn steam_qr_login_and_logout() {
    let (h, script) = login_harness(GuardKind::DeviceConfirmation).await;
    let v = h.status().await;
    assert_eq!(v["auth"]["logged_in"], false);
    assert!(h.secrets.get(REFRESH_TOKEN_SECRET).await.unwrap().is_none());

    let v = h.login(json!({"method": "qr"})).await;
    assert_eq!(v["status"], "qr");
    assert_eq!(v["url"], "https://s.team/q/1/9");
    let id = v["challenge_id"].as_str().unwrap().to_string();

    let poll = json!({"method": "qr_poll", "challenge_id": id});
    let v = h.login(poll.clone()).await;
    assert_eq!(v["status"], "pending");
    assert_eq!(v["challenge_id"], id.as_str());

    script.approved.store(true, Ordering::SeqCst);
    let v = h.login(poll.clone()).await;
    assert_eq!(v, json!({"status": "logged_in", "account": "alice"}));
    h.assert_logged_in("alice").await;

    // The challenge is consumed by success.
    assert_eq!(h.login(poll).await["status"], "failed");

    let (s, _) = h
        .call(Method::POST, "/api/platforms/steam/managed/logout", None)
        .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    assert_eq!(h.status().await["auth"]["logged_in"], false);
    assert!(h.secrets.get(REFRESH_TOKEN_SECRET).await.unwrap().is_none());
    h.app.shutdown().await;
}

#[tokio::test]
async fn steam_password_email_guard_login() {
    let (h, _) = login_harness(GuardKind::EmailCode).await;
    let v = h
        .login(json!({"method": "password", "username": "bob", "password": "s3cret"}))
        .await;
    assert_eq!(v["status"], "guard_required");
    assert_eq!(v["guard_type"], "email");
    let id = v["challenge_id"].as_str().unwrap().to_string();

    // A wrong code fails but keeps the challenge so the user can retry.
    let v = h
        .login(json!({"method": "guard", "challenge_id": id, "code": "00000"}))
        .await;
    assert_eq!(v["status"], "failed");
    assert!(v["message"].as_str().unwrap().contains("rejected"), "{v}");
    assert_eq!(h.status().await["auth"]["logged_in"], false);

    let v = h
        .login(json!({"method": "guard", "challenge_id": id, "code": format!(" {CODE_OK} ")}))
        .await;
    assert_eq!(v, json!({"status": "logged_in", "account": "bob"}));
    h.assert_logged_in("bob").await;
    h.app.shutdown().await;
}

#[tokio::test]
async fn steam_password_authenticator_login() {
    let (h, _) = login_harness(GuardKind::DeviceCode).await;
    let v = h
        .login(json!({"method": "password", "username": "bob", "password": "s3cret"}))
        .await;
    assert_eq!(v["status"], "guard_required");
    assert_eq!(v["guard_type"], "device");
    let id = v["challenge_id"].as_str().unwrap();
    let v = h
        .login(json!({"method": "guard", "challenge_id": id, "code": CODE_OK}))
        .await;
    assert_eq!(v, json!({"status": "logged_in", "account": "bob"}));
    h.assert_logged_in("bob").await;
    h.app.shutdown().await;
}

#[tokio::test]
async fn steam_login_bad_password_and_unknown_challenge() {
    let (h, _) = login_harness(GuardKind::EmailCode).await;
    let v = h
        .login(json!({"method": "password", "username": "bob", "password": "nope"}))
        .await;
    assert_eq!(v["status"], "failed");

    for body in [
        json!({"method": "qr_poll", "challenge_id": "nonexistent"}),
        json!({"method": "guard", "challenge_id": "nonexistent", "code": CODE_OK}),
    ] {
        let v = h.login(body).await;
        assert_eq!(v["status"], "failed", "{v}");
        assert!(v["message"].as_str().unwrap().contains("expired"), "{v}");
    }
    assert_eq!(h.status().await["auth"]["logged_in"], false);
    h.app.shutdown().await;
}
