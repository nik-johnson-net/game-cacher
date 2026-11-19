//! Login state machine and update flow against a fake Steam backend.

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use async_trait::async_trait;
use base64::Engine;
use base64::prelude::BASE64_STANDARD;
use bytes::Bytes;
use rsa::traits::PublicKeyParts;
use rsa::{Pkcs1v15Encrypt, RsaPrivateKey};

use super::appinfo::{AppInfo, DepotInfo};
use super::backend::*;
use super::manifest::tests::build_manifest;
use super::update::pick_cdn_host;
use super::*;
use crate::platforms::{GuardType, Prefill, StateStore, WarmResult};
use crate::secrets::SecretStore;
use crate::settings::SettingsStore;
use crate::storage::MemoryStorage;

// ---------------------------------------------------------------------------
// Test doubles
// ---------------------------------------------------------------------------

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

struct MemState(Mutex<serde_json::Value>);

#[async_trait]
impl StateStore for MemState {
    async fn load(&self) -> Result<serde_json::Value> {
        Ok(self.0.lock().unwrap().clone())
    }
    async fn save(&self, value: &serde_json::Value) -> Result<()> {
        *self.0.lock().unwrap() = value.clone();
        Ok(())
    }
}

#[derive(Default)]
struct MockPrefill {
    /// path → body served by `get`.
    bodies: Mutex<HashMap<String, Bytes>>,
    gets: Mutex<Vec<(String, String)>>,
    warmed: Mutex<Vec<(String, String)>>,
    fail_warm: Mutex<HashSet<String>>,
}

impl MockPrefill {
    fn warmed_paths(&self) -> BTreeSet<String> {
        self.warmed
            .lock()
            .unwrap()
            .iter()
            .map(|(_, p)| p.clone())
            .collect()
    }
    fn get_paths(&self) -> Vec<String> {
        self.gets
            .lock()
            .unwrap()
            .iter()
            .map(|(_, p)| p.clone())
            .collect()
    }
}

#[async_trait]
impl Prefill for MockPrefill {
    async fn warm(&self, identifier: &str, host: &str, path: &str) -> Result<WarmResult> {
        assert_eq!(identifier, "steam");
        self.warmed.lock().unwrap().push((host.into(), path.into()));
        if self.fail_warm.lock().unwrap().contains(path) {
            bail!("upstream 503");
        }
        Ok(WarmResult {
            bytes_downloaded: 1000,
            bytes_already_cached: 10,
        })
    }
    async fn get(&self, identifier: &str, host: &str, path: &str) -> Result<Bytes> {
        assert_eq!(identifier, "steam");
        self.gets.lock().unwrap().push((host.into(), path.into()));
        self.bodies
            .lock()
            .unwrap()
            .get(path)
            .cloned()
            .ok_or_else(|| anyhow!("upstream 404 for {path}"))
    }
    async fn get_range(&self, _: &str, _: &str, _: &str, _: u64, _: u64) -> Result<Bytes> {
        unimplemented!()
    }
}

/// Content the fake CM session serves.
#[derive(Default)]
struct FakeContent {
    apps: HashMap<u32, AppInfo>,
    ownership: Ownership,
    servers: Vec<CdnServer>,
    renewed_token: Option<String>,
    /// Makes the next `app_info` call fail (simulates a dropped CM connection).
    fail_next_app_info: bool,
}

#[derive(Default)]
struct FakeBackend {
    rsa: Option<RsaPrivateKey>,
    qr_start: Option<AuthSessionStart>,
    creds_start: Option<AuthSessionStart>,
    polls: Mutex<VecDeque<Result<PollResult, String>>>,
    accepted_code: String,
    content: Arc<Mutex<FakeContent>>,
    // recorded calls
    decrypted_passwords: Mutex<Vec<String>>,
    submitted_codes: Mutex<Vec<(u64, u64, String, GuardKind)>>,
    polled_client_ids: Mutex<Vec<u64>>,
    connects: Mutex<Vec<(String, String)>>,
    code_requests: Arc<Mutex<Vec<(u32, u32, u64)>>>,
}

#[async_trait]
impl SteamAuth for FakeBackend {
    async fn password_rsa_key(&self, _account: &str) -> Result<RsaKey> {
        let key = self.rsa.as_ref().expect("rsa key configured");
        Ok(RsaKey {
            modulus_hex: key.n().to_str_radix(16),
            exponent_hex: key.e().to_str_radix(16),
            timestamp: 1234,
        })
    }
    async fn begin_qr(&self) -> Result<AuthSessionStart> {
        self.qr_start.clone().ok_or_else(|| anyhow!("qr disabled"))
    }
    async fn begin_credentials(
        &self,
        _account: &str,
        encrypted: &str,
        timestamp: u64,
    ) -> Result<AuthSessionStart> {
        assert_eq!(timestamp, 1234);
        let key = self.rsa.as_ref().unwrap();
        let plain = key.decrypt(Pkcs1v15Encrypt, &BASE64_STANDARD.decode(encrypted)?)?;
        self.decrypted_passwords
            .lock()
            .unwrap()
            .push(String::from_utf8(plain)?);
        self.creds_start
            .clone()
            .ok_or_else(|| anyhow!("invalid password (EResult 5)"))
    }
    async fn submit_guard_code(
        &self,
        client_id: u64,
        steam_id: u64,
        code: &str,
        kind: GuardKind,
    ) -> Result<()> {
        self.submitted_codes
            .lock()
            .unwrap()
            .push((client_id, steam_id, code.into(), kind));
        if code != self.accepted_code {
            bail!("invalid Steam Guard code (EResult 65)");
        }
        Ok(())
    }
    async fn poll(&self, client_id: u64, _request_id: &[u8]) -> Result<PollResult> {
        self.polled_client_ids.lock().unwrap().push(client_id);
        match self.polls.lock().unwrap().pop_front() {
            Some(Ok(p)) => Ok(p),
            Some(Err(e)) => Err(anyhow!(e)),
            None => Ok(PollResult::default()),
        }
    }
}

#[async_trait]
impl SteamBackend for FakeBackend {
    async fn connect(&self, account: &str, refresh_token: &str) -> Result<Box<dyn SteamSession>> {
        self.connects
            .lock()
            .unwrap()
            .push((account.into(), refresh_token.into()));
        Ok(Box::new(FakeSession {
            content: self.content.clone(),
            code_requests: self.code_requests.clone(),
        }))
    }
}

struct FakeSession {
    content: Arc<Mutex<FakeContent>>,
    code_requests: Arc<Mutex<Vec<(u32, u32, u64)>>>,
}

#[async_trait]
impl SteamSession for FakeSession {
    fn renewed_refresh_token(&self) -> Option<String> {
        self.content.lock().unwrap().renewed_token.clone()
    }
    async fn ownership(&self) -> Result<Ownership> {
        Ok(self.content.lock().unwrap().ownership.clone())
    }
    async fn app_info(&self, app_ids: &[u32]) -> Result<Vec<AppInfo>> {
        let mut content = self.content.lock().unwrap();
        if std::mem::take(&mut content.fail_next_app_info) {
            bail!("connection closed");
        }
        Ok(app_ids
            .iter()
            .filter_map(|id| content.apps.get(id).cloned())
            .collect())
    }
    async fn manifest_request_code(
        &self,
        app_id: u32,
        depot_id: u32,
        manifest_gid: u64,
        branch: &str,
    ) -> Result<u64> {
        assert_eq!(branch, "public");
        self.code_requests
            .lock()
            .unwrap()
            .push((app_id, depot_id, manifest_gid));
        Ok(9000 + u64::from(depot_id))
    }
    async fn cdn_servers(&self) -> Result<Vec<CdnServer>> {
        Ok(self.content.lock().unwrap().servers.clone())
    }
}

struct Harness {
    managed: SteamManaged,
    backend: Arc<FakeBackend>,
    ctx: PlatformCtx,
    secrets: Arc<MemSecrets>,
    state: Arc<MemState>,
    prefill: Arc<MockPrefill>,
}

async fn harness_with(backend: FakeBackend, login_ttl: Duration) -> Harness {
    let backend = Arc::new(backend);
    let b = backend.clone();
    let managed = SteamManaged::new(Box::new(move |_| b.clone()), login_ttl);
    let secrets = Arc::new(MemSecrets::default());
    let state = Arc::new(MemState(Mutex::new(serde_json::json!({}))));
    let prefill = Arc::new(MockPrefill::default());
    let settings = SettingsStore::load(Arc::new(MemoryStorage::new()))
        .await
        .unwrap();
    let ctx = PlatformCtx {
        secrets: secrets.clone(),
        state: state.clone(),
        settings: Arc::new(settings),
        http: reqwest::Client::new(),
        prefill: prefill.clone(),
    };
    Harness {
        managed,
        backend,
        ctx,
        secrets,
        state,
        prefill,
    }
}

async fn harness(backend: FakeBackend) -> Harness {
    harness_with(backend, auth::SESSION_TTL).await
}

fn start(client_id: u64, confirmations: Vec<GuardKind>, url: &str) -> AuthSessionStart {
    AuthSessionStart {
        client_id,
        request_id: vec![1, 2, 3],
        interval: 0.0,
        allowed_confirmations: confirmations,
        steam_id: 76561198000000000,
        challenge_url: url.into(),
    }
}

fn token_poll(account: &str) -> Result<PollResult, String> {
    Ok(PollResult {
        refresh_token: Some("refresh-abc".into()),
        account_name: Some(account.into()),
        ..Default::default()
    })
}

fn challenge(r: &LoginResponse) -> String {
    match r {
        LoginResponse::Qr { challenge_id, .. }
        | LoginResponse::Pending { challenge_id }
        | LoginResponse::GuardRequired { challenge_id, .. } => challenge_id.clone(),
        other => panic!("no challenge in {other:?}"),
    }
}

fn small_rsa() -> RsaPrivateKey {
    RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 512).unwrap()
}

// ---------------------------------------------------------------------------
// Login
// ---------------------------------------------------------------------------

#[tokio::test]
async fn qr_login_pending_rotation_then_success() {
    let h = harness(FakeBackend {
        qr_start: Some(start(5, vec![], "https://s.team/q/1/aaa")),
        polls: Mutex::new(VecDeque::from([
            Ok(PollResult::default()),
            Ok(PollResult {
                new_client_id: Some(77),
                new_challenge_url: Some("https://s.team/q/1/bbb".into()),
                ..Default::default()
            }),
            token_poll("alice"),
        ])),
        ..Default::default()
    })
    .await;
    let m = &h.managed;
    assert_eq!(m.login_methods(), &[LoginMethod::Qr, LoginMethod::Password]);
    assert!(!m.auth_status(&h.ctx).await.unwrap().logged_in);

    let r = m.login(&h.ctx, LoginRequest::Qr).await.unwrap();
    let id = challenge(&r);
    assert_eq!(
        r,
        LoginResponse::Qr {
            challenge_id: id.clone(),
            url: "https://s.team/q/1/aaa".into()
        }
    );
    let poll = || LoginRequest::QrPoll {
        challenge_id: id.clone(),
    };
    assert_eq!(
        m.login(&h.ctx, poll()).await.unwrap(),
        LoginResponse::Pending {
            challenge_id: id.clone()
        }
    );
    assert_eq!(
        m.login(&h.ctx, poll()).await.unwrap(),
        LoginResponse::Qr {
            challenge_id: id.clone(),
            url: "https://s.team/q/1/bbb".into()
        }
    );
    assert_eq!(
        m.login(&h.ctx, poll()).await.unwrap(),
        LoginResponse::LoggedIn {
            account: "alice".into()
        }
    );
    // the rotated client id is used for subsequent polls
    assert_eq!(*h.backend.polled_client_ids.lock().unwrap(), vec![5, 5, 77]);
    assert_eq!(
        h.secrets
            .get(REFRESH_TOKEN_SECRET)
            .await
            .unwrap()
            .as_deref(),
        Some("refresh-abc")
    );
    let status = m.auth_status(&h.ctx).await.unwrap();
    assert!(status.logged_in && status.required);
    assert_eq!(status.account.as_deref(), Some("alice"));
    // the challenge is consumed
    assert!(matches!(
        m.login(&h.ctx, poll()).await.unwrap(),
        LoginResponse::Failed { .. }
    ));
}

#[tokio::test]
async fn password_login_with_email_guard() {
    let h = harness(FakeBackend {
        rsa: Some(small_rsa()),
        creds_start: Some(start(9, vec![GuardKind::EmailCode], "")),
        accepted_code: "ABCDE".into(),
        polls: Mutex::new(VecDeque::from([
            Ok(PollResult::default()),
            token_poll("bob"),
        ])),
        ..Default::default()
    })
    .await;
    let m = &h.managed;
    let r = m
        .login(
            &h.ctx,
            LoginRequest::Password {
                username: " bob ".into(),
                password: "hunter2".into(),
            },
        )
        .await
        .unwrap();
    let id = challenge(&r);
    assert_eq!(
        r,
        LoginResponse::GuardRequired {
            challenge_id: id.clone(),
            guard_type: GuardType::Email
        }
    );
    assert_eq!(
        *h.backend.decrypted_passwords.lock().unwrap(),
        vec!["hunter2"]
    );

    // wrong code: failed, but the session survives for a retry
    let r = m
        .login(
            &h.ctx,
            LoginRequest::Guard {
                challenge_id: id.clone(),
                code: "WRONG".into(),
            },
        )
        .await
        .unwrap();
    assert!(matches!(r, LoginResponse::Failed { message } if message.contains("Guard")));

    let r = m
        .login(
            &h.ctx,
            LoginRequest::Guard {
                challenge_id: id.clone(),
                code: " ABCDE ".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        r,
        LoginResponse::LoggedIn {
            account: "bob".into()
        }
    );
    let codes = h.backend.submitted_codes.lock().unwrap().clone();
    assert_eq!(
        codes.last().unwrap(),
        &(9, 76561198000000000, "ABCDE".into(), GuardKind::EmailCode)
    );
    assert_eq!(h.backend.polled_client_ids.lock().unwrap().len(), 2);
    assert!(h.secrets.get(REFRESH_TOKEN_SECRET).await.unwrap().is_some());
    assert_eq!(h.state.load().await.unwrap()["account"], "bob");
}

#[tokio::test]
async fn password_login_with_device_confirmation_and_device_code() {
    let h = harness(FakeBackend {
        rsa: Some(small_rsa()),
        creds_start: Some(start(3, vec![GuardKind::DeviceConfirmation], "")),
        polls: Mutex::new(VecDeque::from([
            Ok(PollResult::default()),
            token_poll("carol"),
        ])),
        ..Default::default()
    })
    .await;
    let m = &h.managed;
    let password = |u: &str| LoginRequest::Password {
        username: u.into(),
        password: "pw".into(),
    };
    let r = m.login(&h.ctx, password("carol")).await.unwrap();
    let id = challenge(&r);
    assert!(matches!(
        r,
        LoginResponse::GuardRequired {
            guard_type: GuardType::DeviceConfirmation,
            ..
        }
    ));
    let poll = || LoginRequest::QrPoll {
        challenge_id: id.clone(),
    };
    assert!(matches!(
        m.login(&h.ctx, poll()).await.unwrap(),
        LoginResponse::Pending { .. }
    ));
    assert_eq!(
        m.login(&h.ctx, poll()).await.unwrap(),
        LoginResponse::LoggedIn {
            account: "carol".into()
        }
    );

    // when a code is also allowed, ask for the authenticator code
    let h = harness(FakeBackend {
        rsa: Some(small_rsa()),
        creds_start: Some(start(
            3,
            vec![GuardKind::DeviceConfirmation, GuardKind::DeviceCode],
            "",
        )),
        ..Default::default()
    })
    .await;
    assert!(matches!(
        h.managed.login(&h.ctx, password("dave")).await.unwrap(),
        LoginResponse::GuardRequired {
            guard_type: GuardType::Device,
            ..
        }
    ));
}

#[tokio::test]
async fn password_login_without_guard_and_bad_password() {
    let h = harness(FakeBackend {
        rsa: Some(small_rsa()),
        creds_start: Some(start(3, vec![GuardKind::None], "")),
        polls: Mutex::new(VecDeque::from([Ok(PollResult {
            refresh_token: Some("tok".into()),
            ..Default::default()
        })])),
        ..Default::default()
    })
    .await;
    let r = h
        .managed
        .login(
            &h.ctx,
            LoginRequest::Password {
                username: "erin".into(),
                password: "pw".into(),
            },
        )
        .await
        .unwrap();
    // account falls back to the username when the poll omits it
    assert_eq!(
        r,
        LoginResponse::LoggedIn {
            account: "erin".into()
        }
    );

    let h = harness(FakeBackend {
        rsa: Some(small_rsa()),
        ..Default::default()
    })
    .await;
    let r = h
        .managed
        .login(
            &h.ctx,
            LoginRequest::Password {
                username: "erin".into(),
                password: "bad".into(),
            },
        )
        .await
        .unwrap();
    assert!(matches!(r, LoginResponse::Failed { message } if message.contains("invalid password")));
}

#[tokio::test]
async fn expired_unknown_and_failed_challenges() {
    let h = harness_with(
        FakeBackend {
            qr_start: Some(start(1, vec![], "u")),
            ..Default::default()
        },
        Duration::ZERO,
    )
    .await;
    let id = challenge(&h.managed.login(&h.ctx, LoginRequest::Qr).await.unwrap());
    let r = h
        .managed
        .login(&h.ctx, LoginRequest::QrPoll { challenge_id: id })
        .await
        .unwrap();
    assert!(matches!(r, LoginResponse::Failed { message } if message.contains("expired")));
    assert!(h.backend.polled_client_ids.lock().unwrap().is_empty());

    let h = harness(FakeBackend {
        qr_start: Some(start(1, vec![], "u")),
        polls: Mutex::new(VecDeque::from([Err("login session expired".into())])),
        ..Default::default()
    })
    .await;
    let r = h
        .managed
        .login(
            &h.ctx,
            LoginRequest::Guard {
                challenge_id: "nope".into(),
                code: "1".into(),
            },
        )
        .await
        .unwrap();
    assert!(matches!(r, LoginResponse::Failed { .. }));
    let id = challenge(&h.managed.login(&h.ctx, LoginRequest::Qr).await.unwrap());
    let poll = LoginRequest::QrPoll { challenge_id: id };
    assert!(matches!(
        h.managed.login(&h.ctx, poll.clone()).await.unwrap(),
        LoginResponse::Failed { message } if message.contains("not approved")
    ));
    // a failed poll ends the session
    assert!(matches!(
        h.managed.login(&h.ctx, poll).await.unwrap(),
        LoginResponse::Failed { message } if message.contains("expired")
    ));
    assert!(h.secrets.get(REFRESH_TOKEN_SECRET).await.unwrap().is_none());
}

#[tokio::test]
async fn logout_clears_token_and_account() {
    let h = harness(FakeBackend::default()).await;
    h.secrets.put(REFRESH_TOKEN_SECRET, "t").await.unwrap();
    h.state
        .save(&serde_json::json!({"account": "a", "apps": {"1": {"buildid": "2"}}}))
        .await
        .unwrap();
    h.managed.logout(&h.ctx).await.unwrap();
    assert!(h.secrets.get(REFRESH_TOKEN_SECRET).await.unwrap().is_none());
    let st = h.state.load().await.unwrap();
    assert!(st.get("account").is_none());
    assert_eq!(st["apps"]["1"]["buildid"], "2");
    let status = h.managed.auth_status(&h.ctx).await.unwrap();
    assert_eq!(
        status,
        AuthStatus {
            logged_in: false,
            account: None,
            required: true,
            auth_url: None
        }
    );
}

// ---------------------------------------------------------------------------
// Games and updates
// ---------------------------------------------------------------------------

const SHA_A: [u8; 20] = [0xaa; 20];
const SHA_B: [u8; 20] = [0xbb; 20];
const SHA_C: [u8; 20] = [0xcc; 20];

fn depot(id: u32, oslist: Option<&str>, gid: u64, size: u64) -> DepotInfo {
    DepotInfo {
        depot_id: id,
        oslist: oslist.map(str::to_string),
        has_manifests: true,
        public_manifest: Some(gid),
        size: Some(size),
        ..Default::default()
    }
}

fn game(buildid: &str, gid_101: u64, gid_103: u64) -> AppInfo {
    AppInfo {
        app_id: 100,
        name: "Test Game".into(),
        app_type: "game".into(),
        public_buildid: Some(buildid.into()),
        depots: vec![
            depot(101, Some("windows"), gid_101, 1000),
            depot(102, Some("macos"), 1002, 2000),
            depot(103, None, gid_103, 500),
        ],
        ..Default::default()
    }
}

fn servers() -> Vec<CdnServer> {
    let s = |kind: &str, vhost: &str, load: f32| CdnServer {
        kind: kind.into(),
        host: format!("ip-{vhost}"),
        vhost: vhost.into(),
        https_support: "optional".into(),
        weighted_load: load,
        ..Default::default()
    };
    vec![
        s("CDN", "busy.steamcontent.com", 90.0),
        s("SteamCache", "cache1.steamcontent.com", 10.0),
        s("OpenCache", "other.example", 1.0),
    ]
}

/// A logged-in harness whose fake serves app 100 (+ DLC 200, game 300).
async fn content_harness(app: AppInfo) -> Harness {
    let content = FakeContent {
        apps: HashMap::from([
            (100, app),
            (
                200,
                AppInfo {
                    app_id: 200,
                    name: "Some DLC".into(),
                    app_type: "dlc".into(),
                    ..Default::default()
                },
            ),
            (
                300,
                AppInfo {
                    app_id: 300,
                    name: "another game".into(),
                    app_type: "game".into(),
                    public_buildid: Some("7".into()),
                    ..Default::default()
                },
            ),
        ]),
        ownership: Ownership {
            app_ids: [100, 200, 300].into(),
            depot_ids: [101, 102, 103].into(),
        },
        servers: servers(),
        renewed_token: Some("refresh-new".into()),
        fail_next_app_info: false,
    };
    let h = harness(FakeBackend {
        content: Arc::new(Mutex::new(content)),
        ..Default::default()
    })
    .await;
    h.secrets
        .put(REFRESH_TOKEN_SECRET, "refresh-old")
        .await
        .unwrap();
    h.state
        .save(&serde_json::json!({"account": "alice"}))
        .await
        .unwrap();
    h
}

fn serve_manifest(h: &Harness, depot: u32, gid: u64, files: &[&[[u8; 20]]]) {
    h.prefill.bodies.lock().unwrap().insert(
        format!("/depot/{depot}/manifest/{gid}/5/{}", 9000 + depot),
        Bytes::from(build_manifest(depot, gid, files)),
    );
}

#[tokio::test]
async fn list_games_returns_owned_games_and_renews_token() {
    let h = content_harness(game("555", 1001, 1003)).await;
    let games = h.managed.list_games(&h.ctx).await.unwrap();
    let summary: Vec<(&str, &str, Option<&str>, Option<u64>)> = games
        .iter()
        .map(|g| {
            (
                g.id.as_str(),
                g.name.as_str(),
                g.version.as_deref(),
                g.size_bytes,
            )
        })
        .collect();
    assert_eq!(
        summary,
        vec![
            ("300", "another game", Some("7"), None),
            ("100", "Test Game", Some("555"), Some(1500)),
        ]
    );
    assert_eq!(
        *h.backend.connects.lock().unwrap(),
        vec![("alice".to_string(), "refresh-old".to_string())]
    );
    assert_eq!(
        h.secrets
            .get(REFRESH_TOKEN_SECRET)
            .await
            .unwrap()
            .as_deref(),
        Some("refresh-new")
    );
    // the session is reused
    h.managed.list_games(&h.ctx).await.unwrap();
    assert_eq!(h.backend.connects.lock().unwrap().len(), 1);
    // a failing reused session is replaced once
    h.backend.content.lock().unwrap().fail_next_app_info = true;
    assert_eq!(h.managed.list_games(&h.ctx).await.unwrap().len(), 2);
    assert_eq!(h.backend.connects.lock().unwrap().len(), 2);
    // ... but a fresh session's failure is returned
    h.managed.drop_session().await;
    h.backend.content.lock().unwrap().fail_next_app_info = true;
    assert!(h.managed.list_games(&h.ctx).await.is_err());
}

#[tokio::test]
async fn full_update_prefetches_unique_chunks_of_selected_depots() {
    let h = content_harness(game("555", 1001, 1003)).await;
    serve_manifest(&h, 101, 1001, &[&[SHA_A, SHA_B], &[SHA_B, SHA_C], &[SHA_A]]);
    serve_manifest(&h, 103, 1003, &[&[SHA_A]]);

    let r = h.managed.update_game(&h.ctx, "100").await.unwrap();
    assert_eq!(r.error, None);
    assert!(r.updated);
    assert_eq!(r.name, "Test Game");
    assert_eq!(r.previous_version, None);
    assert_eq!(r.current_version.as_deref(), Some("555"));
    assert_eq!(r.files, 4);
    assert_eq!(r.bytes_downloaded, 4000);
    assert_eq!(r.bytes_already_cached, 40);

    let a = hex::encode(SHA_A);
    let b = hex::encode(SHA_B);
    let c = hex::encode(SHA_C);
    let expected: BTreeSet<String> = [
        format!("/depot/101/chunk/{a}"),
        format!("/depot/101/chunk/{b}"),
        format!("/depot/101/chunk/{c}"),
        format!("/depot/103/chunk/{a}"),
    ]
    .into();
    assert_eq!(h.prefill.warmed_paths(), expected);
    assert_eq!(h.prefill.warmed.lock().unwrap().len(), 4); // no duplicates
    assert!(
        h.prefill
            .warmed
            .lock()
            .unwrap()
            .iter()
            .all(|(host, _)| host == "cache1.steamcontent.com")
    );
    assert_eq!(
        h.prefill.get_paths(),
        vec![
            "/depot/101/manifest/1001/5/9101".to_string(),
            "/depot/103/manifest/1003/5/9103".to_string()
        ]
    );
    assert_eq!(
        *h.backend.code_requests.lock().unwrap(),
        vec![(100, 101, 1001), (100, 103, 1003)]
    );
    let st = h.state.load().await.unwrap();
    assert_eq!(st["account"], "alice");
    assert_eq!(
        st["apps"]["100"],
        serde_json::json!({"buildid": "555", "depots": {"101": "1001", "103": "1003"}})
    );
}

#[tokio::test]
async fn unchanged_buildid_is_a_noop() {
    let h = content_harness(game("555", 1001, 1003)).await;
    h.state
        .save(&serde_json::json!({
            "account": "alice",
            "apps": {"100": {"buildid": "555", "depots": {"101": "1001", "103": "1003"}}}
        }))
        .await
        .unwrap();
    let r = h.managed.update_game(&h.ctx, "100").await.unwrap();
    assert!(!r.updated);
    assert_eq!(r.error, None);
    assert_eq!(r.previous_version.as_deref(), Some("555"));
    assert_eq!(r.current_version.as_deref(), Some("555"));
    assert!(h.prefill.warmed.lock().unwrap().is_empty());
    assert!(h.prefill.get_paths().is_empty());
    assert!(h.backend.code_requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn partial_change_only_fetches_changed_depots() {
    let h = content_harness(game("556", 1001, 2003)).await;
    h.state
        .save(&serde_json::json!({
            "account": "alice",
            "apps": {"100": {"buildid": "555", "depots": {"101": "1001", "103": "1003"}}}
        }))
        .await
        .unwrap();
    serve_manifest(&h, 103, 2003, &[&[SHA_B, SHA_C]]);
    let r = h.managed.update_game(&h.ctx, "100").await.unwrap();
    assert_eq!(r.error, None);
    assert!(r.updated);
    assert_eq!(r.files, 2);
    assert_eq!(r.previous_version.as_deref(), Some("555"));
    assert_eq!(
        *h.backend.code_requests.lock().unwrap(),
        vec![(100, 103, 2003)]
    );
    assert!(
        h.prefill
            .warmed_paths()
            .iter()
            .all(|p| p.starts_with("/depot/103/"))
    );
    assert_eq!(
        h.state.load().await.unwrap()["apps"]["100"],
        serde_json::json!({"buildid": "556", "depots": {"101": "1001", "103": "2003"}})
    );
}

#[tokio::test]
async fn depot_errors_are_reported_and_retried_later() {
    let h = content_harness(game("556", 2001, 2003)).await;
    h.state
        .save(&serde_json::json!({
            "account": "alice",
            "apps": {"100": {"buildid": "555", "depots": {"101": "1001", "103": "1003"}}}
        }))
        .await
        .unwrap();
    // depot 101's manifest is missing upstream; one chunk of 103 fails
    serve_manifest(&h, 103, 2003, &[&[SHA_B, SHA_C]]);
    h.prefill
        .fail_warm
        .lock()
        .unwrap()
        .insert(format!("/depot/103/chunk/{}", hex::encode(SHA_C)));
    let r = h.managed.update_game(&h.ctx, "100").await.unwrap();
    let err = r.error.unwrap();
    assert!(err.starts_with("depot 101:"), "{err}");
    // both depots failed, the old gids and build id are kept so the next run retries
    assert_eq!(
        h.state.load().await.unwrap()["apps"]["100"],
        serde_json::json!({"buildid": "555", "depots": {"101": "1001", "103": "1003"}})
    );

    // next run succeeds and only then records the build id
    serve_manifest(&h, 101, 2001, &[&[SHA_A]]);
    h.prefill.fail_warm.lock().unwrap().clear();
    let r = h.managed.update_game(&h.ctx, "100").await.unwrap();
    assert_eq!(r.error, None);
    assert_eq!(r.previous_version.as_deref(), Some("555"));
    assert_eq!(r.files, 3); // both previously failed depots: 1 + 2 chunks
    assert_eq!(
        h.state.load().await.unwrap()["apps"]["100"]["buildid"],
        "556"
    );
}

#[tokio::test]
async fn update_errors_become_reports() {
    let h = harness(FakeBackend::default()).await;
    let r = h.managed.update_game(&h.ctx, "100").await.unwrap();
    assert!(r.error.unwrap().contains("not logged in"));
    let r = h.managed.update_game(&h.ctx, "abc").await.unwrap();
    assert!(r.error.unwrap().contains("invalid Steam app id"));
    assert!(h.managed.list_games(&h.ctx).await.is_err());

    let h = content_harness(game("1", 1, 1)).await;
    let r = h.managed.update_game(&h.ctx, "999").await.unwrap();
    assert_eq!(r.game_id, "999");
    assert!(r.error.unwrap().contains("no PICS app info"));
}

#[test]
fn cdn_host_selection() {
    let mut list = servers();
    assert_eq!(
        pick_cdn_host(&list, 100).as_deref(),
        Some("cache1.steamcontent.com")
    );
    list[1].allowed_app_ids = vec![5];
    assert_eq!(
        pick_cdn_host(&list, 100).as_deref(),
        Some("busy.steamcontent.com")
    );
    // HTTPS-mandatory servers still serve plain HTTP; vhost falls back to host
    list[0].https_support = "mandatory".into();
    list[0].vhost.clear();
    assert_eq!(
        pick_cdn_host(&list, 100).as_deref(),
        Some("ip-busy.steamcontent.com")
    );
    list[0].use_as_proxy = true;
    assert_eq!(pick_cdn_host(&list, 100), None);
}
