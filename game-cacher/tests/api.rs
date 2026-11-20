//! End-to-end tests of the admin API and wiring, against real listeners.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;
use axum::Router;
use axum::routing::get;
use clap::Parser;
use game_cacher::app::{self, Overrides, RunningApp};
use game_cacher::config::{Config, EnvSecrets};
use game_cacher::platforms::plan::{GamePlan, Outcome};
use game_cacher::platforms::{
    self, AuthStatus, DynPlatform, Game, LoginMethod, LoginRequest, LoginResponse, ManagedUpdates,
    Platform, PlatformCtx, UpdateReport,
};
use game_cacher::secrets::SecretStore;
use game_cacher::storage::{DynStorage, MemoryStorage};
use game_cacher::upstream::StaticResolver;
use reqwest::StatusCode;
use serde_json::{Value, json};

const PW: &str = "hunter2";

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

/// A managed platform whose "updates" warm one object from the test upstream.
struct Fake;

impl Platform for Fake {
    fn id(&self) -> &'static str {
        "fake"
    }
    fn name(&self) -> &'static str {
        "Fake"
    }
    fn description(&self) -> &'static str {
        "test platform"
    }
    fn domains(&self) -> &'static [&'static str] {
        &["fake.example.com"]
    }
    fn managed(&self) -> Option<&dyn ManagedUpdates> {
        Some(self)
    }
}

#[async_trait]
impl ManagedUpdates for Fake {
    fn login_methods(&self) -> &'static [LoginMethod] {
        &[LoginMethod::AuthCode]
    }
    async fn auth_status(&self, ctx: &PlatformCtx) -> Result<AuthStatus> {
        let account = ctx.secrets.get("fake.token").await?;
        Ok(AuthStatus {
            logged_in: account.is_some(),
            account,
            required: true,
            auth_url: None,
        })
    }
    async fn login(&self, ctx: &PlatformCtx, req: LoginRequest) -> Result<LoginResponse> {
        Ok(match req {
            LoginRequest::AuthCode { code } if code == "good" => {
                ctx.secrets.put("fake.token", "player1").await?;
                LoginResponse::LoggedIn {
                    account: "player1".into(),
                }
            }
            _ => LoginResponse::Failed {
                message: "bad code".into(),
            },
        })
    }
    async fn logout(&self, ctx: &PlatformCtx) -> Result<()> {
        ctx.secrets.delete("fake.token").await
    }
    async fn list_games(&self, _ctx: &PlatformCtx) -> Result<Vec<Game>> {
        Ok(vec![
            Game {
                id: "g1".into(),
                name: "Game One".into(),
                version: Some("1".into()),
                size_bytes: None,
                selected: false,
            },
            Game {
                id: "g2".into(),
                name: "Game Two".into(),
                version: None,
                size_bytes: None,
                selected: false,
            },
        ])
    }
    async fn plan_game(&self, _ctx: &PlatformCtx, game_id: &str) -> Result<GamePlan> {
        tokio::time::sleep(Duration::from_millis(300)).await;
        let mut plan = GamePlan::new("fake", game_id, game_id);
        plan.host = "fake.example.com".into();
        let g = plan.group("", true);
        plan.add(format!("/obj/{game_id}"), None, g);
        Ok(plan)
    }
    async fn commit(
        &self,
        _ctx: &PlatformCtx,
        _plan: &GamePlan,
        _outcome: &Outcome,
        _report: &mut UpdateReport,
    ) -> Result<()> {
        Ok(())
    }
}

/// Upstream serving 100 KiB deterministic objects with Range support.
async fn upstream() -> SocketAddr {
    async fn obj(headers: axum::http::HeaderMap) -> axum::response::Response {
        use axum::response::IntoResponse;
        let body: Vec<u8> = (0..100 * 1024u32).map(|i| (i % 251) as u8).collect();
        let total = body.len();
        if let Some(r) = headers.get("range").and_then(|v| v.to_str().ok()) {
            let r = r.trim_start_matches("bytes=");
            let (a, b) = r.split_once('-').unwrap();
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
    let router = Router::new().route("/obj/{name}", get(obj));
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(l, router).await.unwrap() });
    addr
}

struct Harness {
    app: RunningApp,
    storage: DynStorage,
    client: reqwest::Client,
    upstream: SocketAddr,
}

impl Harness {
    async fn new(extra: &[&str], scheduler: Duration) -> Self {
        Self::with_storage(extra, scheduler, Arc::new(MemoryStorage::new())).await
    }

    async fn with_storage(extra: &[&str], scheduler: Duration, storage: DynStorage) -> Self {
        let up = upstream().await;
        let port = up.port().to_string();
        let mut args = vec![
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
        ];
        if !extra.contains(&"--ui-path") {
            args.extend_from_slice(&["--ui-path", "/nonexistent"]);
        }
        args.extend_from_slice(extra);
        let config = Config::parse_from(args);
        let env = EnvSecrets {
            admin_password: Some(PW.into()),
            ..Default::default()
        };
        let mut list = platforms::all();
        list.push(Arc::new(Fake) as DynPlatform);
        let app = app::start(
            config,
            env,
            Overrides {
                resolver: Some(Arc::new(StaticResolver([127, 0, 0, 1].into()))),
                storage: Some(storage.clone()),
                secrets: Some(Arc::new(MemSecrets::default())),
                platforms: Some(list),
                scheduler_period: Some(scheduler),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        Self {
            app,
            storage,
            client: reqwest::Client::new(),
            upstream: up,
        }
    }

    fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.app.admin_addr)
    }

    async fn get(&self, path: &str) -> (StatusCode, Value) {
        let r = self
            .client
            .get(self.url(path))
            .bearer_auth(PW)
            .send()
            .await
            .unwrap();
        let s = r.status();
        (s, r.json().await.unwrap_or(Value::Null))
    }

    async fn send(&self, method: reqwest::Method, path: &str, body: Value) -> (StatusCode, Value) {
        let r = self
            .client
            .request(method, self.url(path))
            .bearer_auth(PW)
            .json(&body)
            .send()
            .await
            .unwrap();
        let s = r.status();
        (s, r.json().await.unwrap_or(Value::Null))
    }

    async fn cache_get(&self, host: &str, path: &str) -> reqwest::Response {
        self.client
            .get(format!("http://{}{path}", self.app.http_addr))
            .header("host", host)
            .send()
            .await
            .unwrap()
    }
}

#[tokio::test]
async fn health_metrics_and_auth() {
    let h = Harness::new(&[], Duration::from_secs(3600)).await;
    let r = h.client.get(h.url("/healthz")).send().await.unwrap();
    assert_eq!(r.text().await.unwrap(), "ok");
    let r = h.client.get(h.url("/metrics")).send().await.unwrap();
    assert!(
        r.headers()["content-type"]
            .to_str()
            .unwrap()
            .starts_with("application/openmetrics-text")
    );
    assert!(r.text().await.unwrap().ends_with("# EOF\n"));

    // Unauthenticated.
    let r = h.client.get(h.url("/api/status")).send().await.unwrap();
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
    let st: Value = h
        .client
        .get(h.url("/api/auth/status"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(st, json!({"authenticated": false, "auth_enabled": true}));

    // Wrong password.
    let r = h
        .client
        .post(h.url("/api/auth/login"))
        .json(&json!({"password": "x"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);

    // Cookie login.
    let r = h
        .client
        .post(h.url("/api/auth/login"))
        .json(&json!({"password": PW}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::NO_CONTENT);
    let cookie = r.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    assert!(cookie.starts_with("gc_session="));
    let r = h
        .client
        .get(h.url("/api/status"))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    let status: Value = r.json().await.unwrap();
    assert_eq!(status["storage"]["backend"], "memory");
    assert_eq!(status["secrets_backend"], "memory");
    assert_eq!(status["dns"]["cache_ips"], json!(["10.99.0.1"]));
    assert_eq!(status["sni_enabled"], true);

    // Logout invalidates the session.
    let r = h
        .client
        .post(h.url("/api/auth/logout"))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::NO_CONTENT);
    let r = h
        .client
        .get(h.url("/api/status"))
        .header("cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
    h.app.shutdown().await;
}

#[tokio::test]
async fn platforms_toggle_cache_stats_and_purge() {
    let h = Harness::new(&[], Duration::from_secs(3600)).await;
    let (s, list) = h.get("/api/platforms").await;
    assert_eq!(s, StatusCode::OK);
    let ids: Vec<&str> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        [
            "steam",
            "epic",
            "blizzard",
            "origin",
            "windowsupdate",
            "fake"
        ]
    );
    assert_eq!(h.get("/api/platforms/nope").await.0, StatusCode::NOT_FOUND);

    // Traffic through the cache shows up in stats.
    let r = h.cache_get("cdn1.epicgames.com", "/obj/a").await;
    assert_eq!(r.headers()["x-gamecacher-status"], "MISS");
    assert_eq!(r.bytes().await.unwrap().len(), 100 * 1024);
    let r = h.cache_get("cdn1.epicgames.com", "/obj/a").await;
    assert_eq!(r.headers()["x-gamecacher-status"], "HIT");
    r.bytes().await.unwrap();
    let (_, epic) = h.get("/api/platforms/epic").await;
    assert_eq!(epic["stats"]["hits"], 1);
    assert_eq!(epic["stats"]["misses"], 1);
    assert_eq!(epic["stats"]["objects"], 1);
    assert!(epic["stats"]["storage_bytes"].as_u64().unwrap() >= 100 * 1024);
    let (_, status) = h.get("/api/status").await;
    assert!(status["storage"]["used_bytes"].as_u64().unwrap() >= 100 * 1024);

    // Disable → bypass + persisted.
    let (s, epic) = h
        .send(
            reqwest::Method::PUT,
            "/api/platforms/epic",
            json!({"enabled": false}),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(epic["enabled"], false);
    let saved = h.storage.get("settings.json").await.unwrap().unwrap();
    let saved: Value = serde_json::from_slice(&saved).unwrap();
    assert_eq!(saved["platforms"]["epic"]["enabled"], false);
    let r = h.cache_get("cdn1.epicgames.com", "/obj/a").await;
    assert_eq!(r.headers()["x-gamecacher-status"], "BYPASS");
    h.send(
        reqwest::Method::PUT,
        "/api/platforms/epic",
        json!({"enabled": true}),
    )
    .await;

    // Purge.
    let (s, purge) = h
        .send(
            reqwest::Method::DELETE,
            "/api/platforms/epic/cache",
            Value::Null,
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(purge["deleted_objects"], 1);
    assert!(purge["freed_bytes"].as_u64().unwrap() >= 100 * 1024);
    assert!(h.storage.list("cache/epic/").await.unwrap().is_empty());
    let (_, epic) = h.get("/api/platforms/epic").await;
    assert_eq!(epic["stats"]["storage_bytes"], 0);
    let r = h.cache_get("cdn1.epicgames.com", "/obj/a").await;
    assert_eq!(r.headers()["x-gamecacher-status"], "MISS");
    assert_eq!(r.bytes().await.unwrap().len(), 100 * 1024);
    h.app.shutdown().await;
}

#[tokio::test]
async fn managed_updates_flow() {
    let h = Harness::new(&[], Duration::from_secs(3600)).await;
    use reqwest::Method;
    assert_eq!(
        h.get("/api/platforms/origin/managed").await.0,
        StatusCode::NOT_FOUND
    );

    let (s, st) = h.get("/api/platforms/fake/managed").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(st["auth"]["logged_in"], false);
    assert_eq!(st["login_methods"], json!(["auth_code"]));
    assert_eq!(st["running"], false);
    assert_eq!(st["next_run"], Value::Null);

    // Selection is trimmed and de-duplicated preserving order; no next_run while logged out.
    let (_, st) = h
        .send(
            Method::PUT,
            "/api/platforms/fake/managed/selection",
            json!({"selected":["g2","g1"," g2 "]}),
        )
        .await;
    assert_eq!(st["selected"], json!(["g2", "g1"]));
    assert_eq!(st["next_run"], Value::Null, "login required first");

    // Runs on a disabled platform are refused.
    h.send(
        Method::PUT,
        "/api/platforms/fake",
        json!({"enabled": false}),
    )
    .await;
    let (s, _) = h
        .send(Method::POST, "/api/platforms/fake/managed/run", Value::Null)
        .await;
    assert_eq!(s, StatusCode::CONFLICT);
    h.send(Method::PUT, "/api/platforms/fake", json!({"enabled": true}))
        .await;

    let (_, r) = h
        .send(
            Method::POST,
            "/api/platforms/fake/managed/login",
            json!({"method":"auth_code","code":"bad"}),
        )
        .await;
    assert_eq!(r["status"], "failed");
    let (_, r) = h
        .send(
            Method::POST,
            "/api/platforms/fake/managed/login",
            json!({"method":"auth_code","code":"good"}),
        )
        .await;
    assert_eq!(r, json!({"status":"logged_in","account":"player1"}));
    let (s, _) = h
        .send(
            Method::POST,
            "/api/platforms/fake/managed/login",
            json!({"method":"bogus"}),
        )
        .await;
    assert!(s.is_client_error());

    let (_, st) = h
        .send(
            Method::PUT,
            "/api/platforms/fake/managed/selection",
            json!({"selected":["g1"," g1 ",""]}),
        )
        .await;
    assert_eq!(st["selected"], json!(["g1"]));
    assert!(st["next_run"].is_string());
    let (_, games) = h.get("/api/platforms/fake/managed/games").await;
    assert_eq!(games[0]["selected"], true);
    assert_eq!(games[1]["selected"], false);

    let (s, r) = h
        .send(Method::POST, "/api/platforms/fake/managed/run", Value::Null)
        .await;
    assert_eq!(s, StatusCode::ACCEPTED);
    assert_eq!(r["started"], true);
    let (s, _) = h
        .send(Method::POST, "/api/platforms/fake/managed/run", Value::Null)
        .await;
    assert_eq!(s, StatusCode::CONFLICT);

    let st = wait_for_run(&h).await;
    let run = &st["last_run"];
    assert_eq!(run["status"], "ok");
    assert_eq!(run["platform"], "fake");
    assert_eq!(run["phase"], Value::Null);
    assert_eq!(run["bytes_to_download"], 100 * 1024);
    assert_eq!(run["bytes_downloaded"], 100 * 1024);
    let report = &run["games"][0];
    assert_eq!(report["game_id"], "g1");
    assert_eq!(report["status"], "done");
    assert_eq!(report["updated"], true);
    assert_eq!(report["bytes_downloaded"], 100 * 1024);
    assert_eq!(report["bytes_already_cached"], 0);
    assert_eq!(report["bytes_to_download"], 100 * 1024);
    assert_eq!(
        (report["objects"].clone(), report["objects_done"].clone()),
        (json!(1), json!(1))
    );
    assert!(!h.storage.list("cache/fake/").await.unwrap().is_empty());
    // Content warmed by the run is a HIT for LAN clients.
    let r = h.cache_get("fake.example.com", "/obj/g1").await;
    assert_eq!(r.headers()["x-gamecacher-status"], "HIT");

    // Second run: everything already cached.
    h.send(Method::POST, "/api/platforms/fake/managed/run", Value::Null)
        .await;
    let st = wait_for_run(&h).await;
    assert_eq!(
        st["last_run"]["games"][0]["bytes_already_cached"],
        100 * 1024
    );
    assert_eq!(st["last_run"]["games"][0]["updated"], false);
    assert_eq!(st["last_run"]["bytes_to_download"], 0);
    let (_, runs) = h.get("/api/runs").await;
    let fake: Vec<&Value> = runs
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["platform"] == "fake")
        .collect();
    assert_eq!(fake.len(), 2);
    assert_eq!(fake[0]["id"], st["last_run"]["id"]);
    assert!(fake[0]["started"].as_str() > fake[1]["started"].as_str());
    let metrics = h
        .client
        .get(h.url("/metrics"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(
        metrics.contains(r#"gamecacher_managed_update_runs_total{platform="fake",result="ok"} 2"#),
        "{metrics}"
    );

    let (s, _) = h
        .send(
            Method::POST,
            "/api/platforms/fake/managed/logout",
            Value::Null,
        )
        .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    assert_eq!(
        h.get("/api/platforms/fake/managed").await.1["auth"]["logged_in"],
        false
    );
    h.app.shutdown().await;
}

#[tokio::test]
async fn runs_can_be_cancelled() {
    let h = Harness::new(&[], Duration::from_secs(3600)).await;
    use reqwest::Method;
    let cancel = "/api/platforms/fake/managed/run/cancel";
    let (s, _) = h.send(Method::POST, cancel, Value::Null).await;
    assert_eq!(s, StatusCode::CONFLICT);
    h.send(
        Method::PUT,
        "/api/platforms/fake/managed/selection",
        json!({"selected":["g1", "g2"]}),
    )
    .await;
    h.send(Method::POST, "/api/platforms/fake/managed/run", Value::Null)
        .await;
    // Still checking g1 (planning takes 300 ms): the pending list is visible.
    let (_, st) = h.get("/api/platforms/fake/managed").await;
    assert_eq!(st["running"], true);
    assert_eq!(st["last_run"]["status"], "running");
    assert_eq!(st["last_run"]["games"].as_array().unwrap().len(), 2);
    assert_eq!(st["last_run"]["games"][1]["status"], "pending");
    let (s, r) = h.send(Method::POST, cancel, Value::Null).await;
    assert_eq!(s, StatusCode::ACCEPTED);
    assert_eq!(r["cancelling"], true);
    let st = wait_for_run(&h).await;
    assert_eq!(st["last_run"]["status"], "cancelled");
    assert_eq!(st["last_run"]["games"][1]["status"], "cancelled");
    assert!(h.storage.list("cache/fake/").await.unwrap().is_empty());
    h.app.shutdown().await;
}

#[tokio::test]
async fn interrupted_runs_are_incomplete_and_retried() {
    let storage: DynStorage = Arc::new(MemoryStorage::new());
    let dead = json!({"runs": [{
        "id": "dead", "platform": "fake", "started": chrono::Utc::now(), "finished": null,
        "status": "running", "phase": "downloading", "bytes_to_download": 10,
        "bytes_downloaded": 5, "games": []
    }]});
    storage
        .put(
            "state/fake.runs.json",
            serde_json::to_vec(&dead).unwrap().into(),
        )
        .await
        .unwrap();
    let h = Harness::with_storage(&[], Duration::from_millis(100), storage).await;
    use reqwest::Method;
    let (_, runs) = h.get("/api/runs").await;
    assert_eq!(runs[0]["id"], "dead");
    assert_eq!(runs[0]["status"], "incomplete");
    h.send(
        Method::POST,
        "/api/platforms/fake/managed/login",
        json!({"method":"auth_code","code":"good"}),
    )
    .await;
    h.send(
        Method::PUT,
        "/api/platforms/fake/managed/selection",
        json!({"selected":["g1"]}),
    )
    .await;
    // The schedule's last trigger predates the dead run, but it is retried anyway.
    let st = wait_for_run(&h).await;
    assert_ne!(st["last_run"]["id"], "dead");
    assert_eq!(st["last_run"]["status"], "ok");
    let (_, runs) = h.get("/api/runs").await;
    assert_eq!(runs.as_array().unwrap().len(), 2);
    assert_eq!(runs[1]["status"], "incomplete");
    h.app.shutdown().await;
}

async fn wait_for_run(h: &Harness) -> Value {
    for _ in 0..100 {
        let (_, st) = h.get("/api/platforms/fake/managed").await;
        if st["running"] == false
            && st["last_run"]["finished"].is_string()
            && st["last_run"]["id"] != "dead"
        {
            return st;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("run did not finish");
}

#[tokio::test]
async fn scheduler_runs_selected_games() {
    let h = Harness::new(&[], Duration::from_millis(100)).await;
    use reqwest::Method;
    h.send(
        Method::POST,
        "/api/platforms/fake/managed/login",
        json!({"method":"auth_code","code":"good"}),
    )
    .await;
    h.send(
        Method::PUT,
        "/api/platforms/fake/managed/selection",
        json!({"selected":["g2"]}),
    )
    .await;
    let st = wait_for_run(&h).await;
    assert_eq!(st["last_run"]["games"][0]["game_id"], "g2");
    let _ = h.upstream;
    h.app.shutdown().await;
}

#[tokio::test]
async fn windows_update_selection_is_validated() {
    let h = Harness::new(&[], Duration::from_secs(3600)).await;
    let path = "/api/platforms/windowsupdate/managed/selection";
    let good = "http://download.windowsupdate.com/d/msdownload/update/x.cab";
    let (s, st) = h
        .send(reqwest::Method::PUT, path, json!({ "selected": [good] }))
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(st["selected"], json!([good]));
    for bad in [
        "http://example.com/x.cab",
        "ftp://download.windowsupdate.com/x",
        "not a url",
    ] {
        let (s, body) = h
            .send(
                reqwest::Method::PUT,
                path,
                json!({ "selected": [good, bad] }),
            )
            .await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{bad}");
        assert!(body["error"].is_string(), "{body}");
    }
    // The previous valid selection is unchanged.
    let (_, st) = h.get("/api/platforms/windowsupdate/managed").await;
    assert_eq!(st["selected"], json!([good]));
    h.app.shutdown().await;
}

#[tokio::test]
async fn settings_and_ui_fallback() {
    let ui = tempfile::tempdir().unwrap();
    std::fs::write(ui.path().join("index.html"), "<html>gc</html>").unwrap();
    std::fs::write(ui.path().join("app.js"), "console.log(1)").unwrap();
    let ui_path = ui.path().to_str().unwrap().to_string();
    let h = Harness::new(&["--ui-path", &ui_path], Duration::from_secs(3600)).await;
    use reqwest::Method;
    assert_eq!(
        h.get("/api/settings").await.1,
        json!({"update_schedule": {"timezone": "UTC", "times": ["03:00"]}})
    );
    // Times are sorted and de-duplicated.
    let (s, v) = h
        .send(
            Method::PUT,
            "/api/settings",
            json!({"update_schedule": {"timezone": "Europe/London", "times": ["16:00", "02:00", "16:00"]}}),
        )
        .await;
    let saved =
        json!({"update_schedule": {"timezone": "Europe/London", "times": ["02:00", "16:00"]}});
    assert_eq!((s, v), (StatusCode::OK, saved.clone()));
    assert_eq!(h.get("/api/settings").await.1, saved);
    for bad in [
        json!({"update_schedule": {"timezone": "Mars/Olympus", "times": ["02:00"]}}),
        json!({"update_schedule": {"timezone": "UTC", "times": ["25:00"]}}),
        json!({"update_schedule": {"timezone": "UTC", "times": ["3:00"]}}),
        json!({"update_schedule": {"timezone": "UTC", "times": ["02:00", "02:04"]}}),
    ] {
        let (s, v) = h.send(Method::PUT, "/api/settings", bad).await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        assert!(v["error"].is_string());
    }
    // An empty schedule means manual runs only.
    let (s, v) = h
        .send(
            Method::PUT,
            "/api/settings",
            json!({"update_schedule": {"timezone": "UTC", "times": []}}),
        )
        .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["update_schedule"]["times"], json!([]));

    let page = h
        .client
        .get(h.url("/platforms/steam"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert_eq!(page, "<html>gc</html>");
    let js = h
        .client
        .get(h.url("/app.js"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert_eq!(js, "console.log(1)");
    h.app.shutdown().await;
}
