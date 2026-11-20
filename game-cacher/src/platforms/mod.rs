//! Platform abstraction. A platform declares which hosts it owns (DNS overrides +
//! cache identifier), per-path cache policy, and optionally managed updates.

pub mod blizzard;
pub mod domains;
pub mod epic;
pub mod origin;
pub mod steam;
pub mod windows_update;

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use bytes::Bytes;
use serde::{Deserialize, Serialize};

use crate::secrets::DynSecrets;
use crate::settings::SettingsStore;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CachePolicy {
    Cache,
    Bypass,
}

pub trait Platform: Send + Sync + 'static {
    /// Stable id, also the cache identifier and settings key (`steam`, `epic`, ...).
    fn id(&self) -> &'static str;
    fn name(&self) -> &'static str;
    fn description(&self) -> &'static str;
    /// Host patterns: exact (`cdn.blizzard.com`) or leading wildcard (`*.cdn.blizzard.com`,
    /// matching subdomains only). Used for DNS overrides and identifier matching.
    fn domains(&self) -> &'static [&'static str];
    /// Some clients are identified by user agent regardless of host (Steam).
    fn matches_user_agent(&self, _user_agent: &str) -> bool {
        false
    }
    fn cache_policy(&self, _path: &str) -> CachePolicy {
        CachePolicy::Cache
    }
    /// Strip upstream ETag headers (Blizzard ETags break range caching).
    fn hide_etag(&self) -> bool {
        false
    }
    fn managed(&self) -> Option<&dyn ManagedUpdates> {
        None
    }
}

pub type DynPlatform = Arc<dyn Platform>;

/// Identifiers of all supported platforms (same order as `all()`).
pub const IDENTIFIERS: &[&str] = &[
    steam::Steam::ID,
    epic::Epic::ID,
    blizzard::Blizzard::ID,
    origin::Origin::ID,
    windows_update::WindowsUpdate::ID,
];

/// All supported platforms in matching-priority order.
pub fn all() -> Vec<DynPlatform> {
    vec![
        Arc::new(steam::Steam::new()),
        Arc::new(epic::Epic::new()),
        Arc::new(blizzard::Blizzard::new()),
        Arc::new(origin::Origin),
        Arc::new(windows_update::WindowsUpdate::new()),
    ]
}

// ---------------------------------------------------------------------------
// Managed updates
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoginMethod {
    None,
    Qr,
    Password,
    AuthCode,
    ManualList,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthStatus {
    pub logged_in: bool,
    pub account: Option<String>,
    /// False for platforms that need no login (Blizzard, Windows Update).
    pub required: bool,
    /// Where the user should go to obtain an auth code (Epic).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "snake_case")]
pub enum LoginRequest {
    Qr,
    QrPoll { challenge_id: String },
    Password { username: String, password: String },
    Guard { challenge_id: String, code: String },
    AuthCode { code: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GuardType {
    Email,
    Device,
    DeviceConfirmation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum LoginResponse {
    LoggedIn {
        account: String,
    },
    Qr {
        challenge_id: String,
        url: String,
    },
    Pending {
        challenge_id: String,
    },
    GuardRequired {
        challenge_id: String,
        guard_type: GuardType,
    },
    Failed {
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Game {
    pub id: String,
    pub name: String,
    pub version: Option<String>,
    pub size_bytes: Option<u64>,
    /// Filled in by the API layer from settings; platforms leave it false.
    #[serde(default)]
    pub selected: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateReport {
    pub game_id: String,
    pub name: String,
    pub previous_version: Option<String>,
    pub current_version: Option<String>,
    /// True when new content was found and prefetched.
    pub updated: bool,
    pub bytes_downloaded: u64,
    pub bytes_already_cached: u64,
    pub files: u64,
    pub error: Option<String>,
}

/// Result of pulling one URL through the cache.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WarmResult {
    pub bytes_downloaded: u64,
    pub bytes_already_cached: u64,
}

/// Fetches content through the cache engine in-process, as if a LAN client had
/// requested `http://<host><path>`, storing it under `identifier`.
#[async_trait]
pub trait Prefill: Send + Sync {
    /// Ensures every slice of the object is cached; does not return the body.
    async fn warm(&self, identifier: &str, host: &str, path_and_query: &str) -> Result<WarmResult>;
    /// Returns the full body (served from cache or fetched and cached).
    /// Use for small metadata objects (manifests, configs) the platform must parse.
    async fn get(&self, identifier: &str, host: &str, path_and_query: &str) -> Result<Bytes>;
    /// Returns a byte range `[start, end]` inclusive (served from cache or fetched).
    async fn get_range(
        &self,
        identifier: &str,
        host: &str,
        path_and_query: &str,
        start: u64,
        end: u64,
    ) -> Result<Bytes>;
}

/// Per-platform persistent state document at `state/<platform>.json`.
#[async_trait]
pub trait StateStore: Send + Sync {
    async fn load(&self) -> Result<serde_json::Value>;
    async fn save(&self, value: &serde_json::Value) -> Result<()>;
}

/// Everything a managed-update implementation may use.
#[derive(Clone)]
pub struct PlatformCtx {
    pub secrets: DynSecrets,
    pub state: Arc<dyn StateStore>,
    pub settings: Arc<SettingsStore>,
    /// Direct HTTP client for APIs (auth, catalog). Content goes through `prefill`.
    pub http: reqwest::Client,
    pub prefill: Arc<dyn Prefill>,
}

#[async_trait]
pub trait ManagedUpdates: Send + Sync {
    fn login_methods(&self) -> &'static [LoginMethod];
    async fn auth_status(&self, ctx: &PlatformCtx) -> Result<AuthStatus>;
    async fn login(&self, ctx: &PlatformCtx, req: LoginRequest) -> Result<LoginResponse>;
    async fn logout(&self, ctx: &PlatformCtx) -> Result<()>;
    async fn list_games(&self, ctx: &PlatformCtx) -> Result<Vec<Game>>;
    /// Checks for a new version of `game_id` and prefetches its content through
    /// `ctx.prefill`. Must be idempotent: re-running with nothing new is cheap.
    /// Errors for a single game should be reported in `UpdateReport::error`.
    async fn update_game(&self, ctx: &PlatformCtx, game_id: &str) -> Result<UpdateReport>;
    /// Rejects an invalid selection before it is saved (e.g. malformed URLs for
    /// manual-list platforms). The error message is shown to the user.
    fn validate_selection(&self, _ids: &[String]) -> Result<()> {
        Ok(())
    }
}

/// True if `host` matches the pattern (`*.x.com` matches subdomains, not the apex).
pub fn host_matches(pattern: &str, host: &str) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    let host = host.split(':').next().unwrap_or_default();
    match pattern.strip_prefix("*.") {
        Some(suffix) => host.len() > suffix.len() + 1 && host.ends_with(&format!(".{suffix}")),
        None => host == pattern,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_matching() {
        assert!(host_matches("cdn.blizzard.com", "cdn.blizzard.com"));
        assert!(host_matches("cdn.blizzard.com", "CDN.blizzard.com."));
        assert!(host_matches("cdn.blizzard.com", "cdn.blizzard.com:80"));
        assert!(host_matches("*.cdn.blizzard.com", "us.cdn.blizzard.com"));
        assert!(!host_matches("*.cdn.blizzard.com", "cdn.blizzard.com"));
        assert!(!host_matches("*.cdn.blizzard.com", "evilcdn.blizzard.com"));
        assert!(!host_matches("cdn.blizzard.com", "x.cdn.blizzard.com"));
    }

    #[test]
    fn login_json_shapes() {
        let r: LoginRequest =
            serde_json::from_str(r#"{"method":"guard","challenge_id":"c","code":"123"}"#).unwrap();
        assert_eq!(
            r,
            LoginRequest::Guard {
                challenge_id: "c".into(),
                code: "123".into()
            }
        );
        let s = serde_json::to_value(LoginResponse::Qr {
            challenge_id: "c".into(),
            url: "u".into(),
        })
        .unwrap();
        assert_eq!(
            s,
            serde_json::json!({"status":"qr","challenge_id":"c","url":"u"})
        );
    }
}

/// The platform set plus lookups used by DNS, the cache engine and the API.
pub struct Platforms {
    list: Vec<DynPlatform>,
}

impl Platforms {
    pub fn new(list: Vec<DynPlatform>) -> Self {
        Self { list }
    }

    pub fn all(&self) -> &[DynPlatform] {
        &self.list
    }

    pub fn get(&self, id: &str) -> Option<&DynPlatform> {
        self.list.iter().find(|p| p.id() == id)
    }

    /// Platform owning a request: user-agent match first (Steam), then host match.
    pub fn identify(&self, host: &str, user_agent: &str) -> Option<&DynPlatform> {
        self.list
            .iter()
            .find(|p| p.matches_user_agent(user_agent))
            .or_else(|| self.by_host(host))
    }

    pub fn by_host(&self, host: &str) -> Option<&DynPlatform> {
        self.list
            .iter()
            .find(|p| p.domains().iter().any(|d| host_matches(d, host)))
    }
}

#[cfg(test)]
mod registry_tests {
    use super::*;

    #[test]
    fn identify() {
        let p = Platforms::new(all());
        assert_eq!(
            p.identify(
                "cache5-fra1.steamcontent.com",
                "Valve/Steam HTTP Client 1.0 (0)"
            )
            .unwrap()
            .id(),
            "steam"
        );
        assert_eq!(
            p.identify("us.cdn.blizzard.com", "Battle.net")
                .unwrap()
                .id(),
            "blizzard"
        );
        assert_eq!(
            p.identify("x.dl.delivery.mp.microsoft.com", "")
                .unwrap()
                .id(),
            "windowsupdate"
        );
        assert!(p.identify("example.com", "curl").is_none());
        assert_eq!(p.get("epic").unwrap().name(), "Epic Games");
    }

    #[test]
    fn identifiers_match_registry() {
        let ids: Vec<&str> = all().iter().map(|p| p.id()).collect();
        assert_eq!(ids, IDENTIFIERS);
    }
}
