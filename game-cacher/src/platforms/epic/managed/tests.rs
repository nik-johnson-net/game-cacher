use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use base64::Engine;
use bytes::Bytes;
use wiremock::matchers::{body_string_contains, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;
use crate::platforms::epic::Endpoints;
use crate::platforms::epic::manifest::{Chunk, testutil::build_manifest};
use crate::platforms::{Prefill, WarmResult};
use crate::secrets::SecretStore;
use crate::settings::SettingsStore;
use crate::storage::{DynStorage, MemoryStorage};

#[derive(Default)]
struct MemSecrets(Mutex<HashMap<String, String>>);

#[async_trait]
impl SecretStore for MemSecrets {
    fn name(&self) -> &'static str {
        "mem"
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

#[derive(Default)]
struct MockPrefill {
    manifest: Bytes,
    fail_containing: Option<String>,
    /// When set, warm reports everything as already cached.
    cached: std::sync::atomic::AtomicBool,
    warms: Mutex<Vec<(String, String, String)>>,
    gets: Mutex<Vec<(String, String, String)>>,
}

#[async_trait]
impl Prefill for MockPrefill {
    async fn warm(&self, id: &str, host: &str, pq: &str) -> Result<WarmResult> {
        self.warms
            .lock()
            .unwrap()
            .push((id.into(), host.into(), pq.into()));
        if self
            .fail_containing
            .as_deref()
            .is_some_and(|f| pq.contains(f))
        {
            bail!("boom");
        }
        let cached = self.cached.load(std::sync::atomic::Ordering::SeqCst);
        Ok(if cached {
            WarmResult {
                bytes_downloaded: 0,
                bytes_already_cached: 10,
            }
        } else {
            WarmResult {
                bytes_downloaded: 10,
                bytes_already_cached: 1,
            }
        })
    }
    async fn get(&self, id: &str, host: &str, pq: &str) -> Result<Bytes> {
        self.gets
            .lock()
            .unwrap()
            .push((id.into(), host.into(), pq.into()));
        Ok(self.manifest.clone())
    }
    async fn get_range(&self, _: &str, _: &str, _: &str, _: u64, _: u64) -> Result<Bytes> {
        bail!("unused")
    }
}

struct Fixture {
    server: MockServer,
    epic: Epic,
    ctx: PlatformCtx,
    prefill: Arc<MockPrefill>,
    secrets: Arc<MemSecrets>,
}

async fn fixture(prefill: MockPrefill) -> Fixture {
    let server = MockServer::start().await;
    let uri = server.uri();
    let epic = Epic::with_endpoints(Endpoints {
        oauth: uri.clone(),
        launcher: uri.clone(),
        catalog: uri,
    });
    let storage: DynStorage = Arc::new(MemoryStorage::new());
    let settings = Arc::new(SettingsStore::load(storage).await.unwrap());
    let secrets = Arc::new(MemSecrets::default());
    let prefill = Arc::new(prefill);
    let ctx = PlatformCtx {
        secrets: secrets.clone(),
        state: settings.state_store("epic"),
        settings,
        http: reqwest::Client::new(),
        prefill: prefill.clone(),
    };
    Fixture {
        server,
        epic,
        ctx,
        prefill,
        secrets,
    }
}

fn token_json(access: &str, refresh: &str) -> serde_json::Value {
    json!({"access_token": access, "refresh_token": refresh, "expires_in": 7200,
           "account_id": "acc1", "displayName": "Player One"})
}

fn basic_header() -> String {
    let raw = format!("{}:daafbccc737745039dffe53d94fc76cf", client::CLIENT_ID);
    format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(raw)
    )
}

#[test]
fn code_extraction() {
    assert_eq!(extract_code(" abc123 ").as_deref(), Some("abc123"));
    assert_eq!(extract_code("\"abc123\"").as_deref(), Some("abc123"));
    let blob = r#"{"redirectUrl":"https://x","authorizationCode":"deadbeef01","exchangeCode":null,"sid":"s"}"#;
    assert_eq!(extract_code(blob).as_deref(), Some("deadbeef01"));
    assert_eq!(extract_code(r#"{"authorizationCode":null}"#), None);
    assert_eq!(extract_code("{not json"), None);
    assert_eq!(extract_code(""), None);
    assert_eq!(extract_code("two words"), None);
}

#[tokio::test]
async fn login_exchanges_code_and_stores_state() {
    let f = fixture(MockPrefill::default()).await;
    Mock::given(method("POST"))
        .and(path("/account/api/oauth/token"))
        .and(header("authorization", basic_header().as_str()))
        .and(body_string_contains("grant_type=authorization_code"))
        .and(body_string_contains("code=abc123"))
        .and(body_string_contains("token_type=eg1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(token_json("at1", "rt1")))
        .expect(1)
        .mount(&f.server)
        .await;

    let blob = r#"{"authorizationCode":"abc123","exchangeCode":null}"#.to_string();
    let r = f
        .epic
        .login(&f.ctx, LoginRequest::AuthCode { code: blob })
        .await
        .unwrap();
    assert_eq!(
        r,
        LoginResponse::LoggedIn {
            account: "Player One".into()
        }
    );
    assert_eq!(
        f.secrets.get(REFRESH_SECRET).await.unwrap().as_deref(),
        Some("rt1")
    );
    let st = f.epic.auth_status(&f.ctx).await.unwrap();
    assert!(st.logged_in && st.required);
    assert_eq!(st.account.as_deref(), Some("Player One"));
    let url = st.auth_url.unwrap();
    assert!(url.starts_with("https://www.epicgames.com/id/login?redirectUrl="));
    assert!(url.contains("clientId%3D34a02cf8f4414e29b15921876da36f9a%26responseType%3Dcode"));
    // Access token is cached after login: no refresh request needed.
    assert_eq!(f.epic.access_token(&f.ctx).await.unwrap(), "at1");
}

#[tokio::test]
async fn login_failures() {
    let f = fixture(MockPrefill::default()).await;
    Mock::given(method("POST"))
        .and(path("/account/api/oauth/token"))
        .respond_with(ResponseTemplate::new(400).set_body_json(
            json!({"errorCode": "x", "errorMessage": "Sorry the authorization code you supplied was not found"}),
        ))
        .mount(&f.server)
        .await;
    let r = f
        .epic
        .login(&f.ctx, LoginRequest::AuthCode { code: "bad".into() })
        .await
        .unwrap();
    assert!(matches!(&r, LoginResponse::Failed { message } if message.contains("not found")));
    assert!(f.secrets.get(REFRESH_SECRET).await.unwrap().is_none());

    let r = f.epic.login(&f.ctx, LoginRequest::Qr).await.unwrap();
    assert!(matches!(r, LoginResponse::Failed { .. }));
    let r = f
        .epic
        .login(&f.ctx, LoginRequest::AuthCode { code: "{}".into() })
        .await
        .unwrap();
    assert!(matches!(r, LoginResponse::Failed { .. }));
}

#[tokio::test]
async fn refresh_rotates_and_caches() {
    let f = fixture(MockPrefill::default()).await;
    f.secrets.put(REFRESH_SECRET, "rt-old").await.unwrap();
    Mock::given(method("POST"))
        .and(path("/account/api/oauth/token"))
        .and(body_string_contains("grant_type=refresh_token"))
        .and(body_string_contains("refresh_token=rt-old"))
        .respond_with(ResponseTemplate::new(200).set_body_json(token_json("at2", "rt-new")))
        .expect(1)
        .mount(&f.server)
        .await;
    assert_eq!(f.epic.access_token(&f.ctx).await.unwrap(), "at2");
    assert_eq!(f.epic.access_token(&f.ctx).await.unwrap(), "at2");
    assert_eq!(
        f.secrets.get(REFRESH_SECRET).await.unwrap().as_deref(),
        Some("rt-new")
    );
}

#[tokio::test]
async fn refresh_rejected_logs_out() {
    let f = fixture(MockPrefill::default()).await;
    f.secrets.put(REFRESH_SECRET, "rt-dead").await.unwrap();
    Mock::given(method("POST"))
        .and(path("/account/api/oauth/token"))
        .respond_with(ResponseTemplate::new(400).set_body_json(
            json!({"errorCode": "invalid_grant", "errorMessage": "refresh token expired"}),
        ))
        .mount(&f.server)
        .await;
    let err = f.epic.access_token(&f.ctx).await.unwrap_err();
    assert!(format!("{err}").contains("log in again"));
    assert!(!f.epic.auth_status(&f.ctx).await.unwrap().logged_in);

    let r = f.epic.update_game(&f.ctx, "Sugar").await.unwrap();
    assert!(r.error.unwrap().contains("not logged in"));
}

#[tokio::test]
async fn logout_clears_secret_and_account() {
    let f = fixture(MockPrefill::default()).await;
    Mock::given(method("POST"))
        .and(path("/account/api/oauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(token_json("at1", "rt1")))
        .mount(&f.server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/account/api/oauth/sessions/kill/at1"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&f.server)
        .await;
    f.epic
        .login(&f.ctx, LoginRequest::AuthCode { code: "abc".into() })
        .await
        .unwrap();
    f.epic.logout(&f.ctx).await.unwrap();
    let st = f.epic.auth_status(&f.ctx).await.unwrap();
    assert!(!st.logged_in && st.account.is_none());
    assert!(f.ctx.state.load().await.unwrap().get("account").is_none());
}

async fn login_with_library(f: &Fixture, build: &str) {
    f.secrets.put(REFRESH_SECRET, "rt").await.unwrap();
    Mock::given(method("POST"))
        .and(path("/account/api/oauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(token_json("at", "rt")))
        .mount(&f.server)
        .await;
    Mock::given(method("GET"))
        .and(path("/launcher/api/public/assets/Windows"))
        .and(query_param("label", "Live"))
        .and(header("authorization", "Bearer at"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"appName": "Sugar", "labelName": "Live-Windows", "buildVersion": build,
             "catalogItemId": "cat1", "namespace": "ns1"},
            {"appName": "UE_4.27", "labelName": "Live-Windows", "buildVersion": "4.27",
             "catalogItemId": "cat2", "namespace": "ue"},
            {"appName": "Alpha", "buildVersion": "1", "catalogItemId": "cat3", "namespace": "ns1"}
        ])))
        .mount(&f.server)
        .await;
    Mock::given(method("GET"))
        .and(path("/catalog/api/shared/namespace/ns1/bulk/items"))
        .and(query_param("id", "cat1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "cat1": {"title": "Sugar Rush"}, "cat3": {"title": "alpha game"}
        })))
        .mount(&f.server)
        .await;
}

#[tokio::test]
async fn lists_games_with_catalog_titles() {
    let f = fixture(MockPrefill::default()).await;
    login_with_library(&f, "1.0.0").await;
    let games = f.epic.list_games(&f.ctx).await.unwrap();
    let got: Vec<_> = games
        .iter()
        .map(|g| (g.id.as_str(), g.name.as_str(), g.version.as_deref()))
        .collect();
    assert_eq!(
        got,
        [
            ("Alpha", "alpha game", Some("1")),
            ("Sugar", "Sugar Rush", Some("1.0.0"))
        ]
    );
    let st = f.ctx.state.load().await.unwrap();
    assert_eq!(st["assets"]["Sugar"]["namespace"], "ns1");
    assert_eq!(st["assets"]["Sugar"]["catalog_item_id"], "cat1");
    assert_eq!(st["assets"]["Sugar"]["title"], "Sugar Rush");
    assert!(st["assets"].get("UE_4.27").is_none());
}

fn chunks() -> Vec<Chunk> {
    (1..=3u32)
        .map(|i| Chunk {
            guid: [i, 0, 0, 0xFF],
            hash: i as u64 * 16,
            group: i as u8,
            window_size: 100,
            file_size: 50,
        })
        .collect()
}

async fn mount_manifest_list(f: &Fixture) {
    Mock::given(method("GET"))
        .and(path(
            "/launcher/api/public/assets/v2/platform/Windows/namespace/ns1/catalogItem/cat1/app/Sugar/label/Live",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"elements": [{
            "appName": "Sugar",
            "manifests": [
                {"uri": "https://evil.example.com/b/Sugar.manifest", "queryParams": []},
                {"uri": "https://cloudflare.epicgamescdn.com/Builds/o/x/default/a.manifest", "queryParams": []},
                {"uri": "https://download.epicgames.com/Builds/Org/o-1/abc/default/m.manifest",
                 "queryParams": [{"name": "f_token", "value": "a b"}]}
            ]
        }]})))
        .expect(1..)
        .mount(&f.server)
        .await;
}

#[tokio::test]
async fn concurrent_list_and_update_lose_no_state() {
    let prefill = MockPrefill {
        manifest: build_manifest("Sugar", "1.0.0", 18, &chunks(), true).into(),
        ..Default::default()
    };
    let f = fixture(prefill).await;
    login_with_library(&f, "1.0.0").await;
    mount_manifest_list(&f).await;
    for _ in 0..5 {
        let (games, report) = tokio::join!(
            f.epic.list_games(&f.ctx),
            f.epic.update_game(&f.ctx, "Sugar"),
        );
        games.unwrap();
        assert_eq!(report.unwrap().error, None);
        let st = f.ctx.state.load().await.unwrap();
        assert_eq!(st["games"]["Sugar"]["build_version"], "1.0.0");
        assert_eq!(st["assets"]["Sugar"]["title"], "Sugar Rush");
    }
}

#[tokio::test]
async fn update_game_warms_every_chunk_then_noops() {
    let prefill = MockPrefill {
        manifest: build_manifest("Sugar", "1.0.0", 18, &chunks(), true).into(),
        ..Default::default()
    };
    let f = fixture(prefill).await;
    login_with_library(&f, "1.0.0").await;
    mount_manifest_list(&f).await;

    let r = f.epic.update_game(&f.ctx, "Sugar").await.unwrap();
    assert_eq!(r.error, None);
    assert!(r.updated);
    assert_eq!(
        (r.files, r.bytes_downloaded, r.bytes_already_cached),
        (3, 30, 3)
    );
    assert_eq!(r.current_version.as_deref(), Some("1.0.0"));
    assert_eq!(r.previous_version, None);

    let gets = f.prefill.gets.lock().unwrap().clone();
    assert_eq!(
        gets,
        [(
            "epic".to_string(),
            "download.epicgames.com".to_string(),
            "/Builds/Org/o-1/abc/default/m.manifest?f_token=a+b".to_string()
        )]
    );
    let mut warms: Vec<_> = f
        .prefill
        .warms
        .lock()
        .unwrap()
        .iter()
        .map(|w| w.2.clone())
        .collect();
    warms.sort();
    let base = "/Builds/Org/o-1/abc/default/ChunksV4";
    let expected: Vec<String> = (1..=3)
        .map(|i| {
            format!(
                "{base}/{i:02}/{:016X}_{i:08X}0000000000000000000000FF.chunk",
                i as u64 * 16
            )
        })
        .collect();
    assert_eq!(warms, expected);
    assert!(
        f.prefill
            .warms
            .lock()
            .unwrap()
            .iter()
            .all(|w| w.0 == "epic" && w.1 == "download.epicgames.com")
    );

    // Same buildVersion again, content cached: re-verified, not an update.
    f.prefill
        .cached
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let r2 = f.epic.update_game(&f.ctx, "Sugar").await.unwrap();
    assert!(r2.error.is_none());
    assert_eq!(r2.previous_version.as_deref(), Some("1.0.0"));
    assert!(!r2.updated);
    assert_eq!(
        (r2.files, r2.bytes_downloaded, r2.bytes_already_cached),
        (3, 0, 30)
    );
    assert_eq!(f.prefill.warms.lock().unwrap().len(), 6);
    assert_eq!(f.prefill.gets.lock().unwrap().len(), 2);

    // Chunks evicted since: same build, but bytes had to be re-downloaded (repair).
    f.prefill
        .cached
        .store(false, std::sync::atomic::Ordering::SeqCst);
    let r3 = f.epic.update_game(&f.ctx, "Sugar").await.unwrap();
    assert!(r3.updated && r3.error.is_none());
    assert_eq!(r3.bytes_downloaded, 30);
}

#[tokio::test]
async fn chunk_errors_are_recorded_but_do_not_stop_the_run() {
    let prefill = MockPrefill {
        manifest: build_manifest("Sugar", "1.0.0", 18, &chunks(), false).into(),
        fail_containing: Some("/02/".into()),
        ..Default::default()
    };
    let f = fixture(prefill).await;
    login_with_library(&f, "1.0.0").await;
    mount_manifest_list(&f).await;

    let r = f.epic.update_game(&f.ctx, "Sugar").await.unwrap();
    assert!(r.updated);
    assert_eq!(f.prefill.warms.lock().unwrap().len(), 3);
    assert_eq!(r.bytes_downloaded, 20);
    assert!(r.error.as_deref().unwrap().contains("boom"));
    // Version not recorded, so the next run retries.
    let st = f.ctx.state.load().await.unwrap();
    assert!(st["games"].get("Sugar").is_none());
}

#[tokio::test]
async fn unknown_game_reports_error() {
    let f = fixture(MockPrefill::default()).await;
    login_with_library(&f, "1.0.0").await;
    let r = f.epic.update_game(&f.ctx, "Nope").await.unwrap();
    assert!(r.error.unwrap().contains("not found"));
}

#[test]
fn manifest_host_selection() {
    let entry = |u: &str| ManifestEntry {
        uri: u.into(),
        query_params: vec![],
    };
    let (host, pq, base) = pick_manifest(&[
        entry("https://nope.example.com/a/m.manifest"),
        entry("https://epicgames-download1.akamaized.net/Builds/x/m.manifest"),
        entry("https://fastly-download.epicgames.com/Builds/y/m.manifest"),
    ])
    .unwrap();
    assert_eq!(host, "fastly-download.epicgames.com");
    assert_eq!(pq, "/Builds/y/m.manifest");
    assert_eq!(base, "/Builds/y");
    assert!(pick_manifest(&[entry("https://nope.example.com/m")]).is_err());
}
