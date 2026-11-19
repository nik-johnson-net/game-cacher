//! Network boundary for Steam. Everything that talks to Steam goes through
//! these traits so the login state machine and update logic can be tested
//! against a fake.

use std::collections::BTreeSet;

use anyhow::Result;
use async_trait::async_trait;

/// `EAuthSessionGuardType` values from steammessages_auth.steamclient.proto.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuardKind {
    None,
    EmailCode,
    DeviceCode,
    DeviceConfirmation,
    EmailConfirmation,
    Other(i32),
}

impl GuardKind {
    pub fn from_proto(v: i32) -> Self {
        match v {
            1 => Self::None,
            2 => Self::EmailCode,
            3 => Self::DeviceCode,
            4 => Self::DeviceConfirmation,
            5 => Self::EmailConfirmation,
            other => Self::Other(other),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RsaKey {
    pub modulus_hex: String,
    pub exponent_hex: String,
    pub timestamp: u64,
}

/// Common part of BeginAuthSessionViaQR / ViaCredentials responses.
#[derive(Debug, Clone, PartialEq)]
pub struct AuthSessionStart {
    pub client_id: u64,
    pub request_id: Vec<u8>,
    /// Suggested poll interval in seconds.
    pub interval: f32,
    pub allowed_confirmations: Vec<GuardKind>,
    /// Credentials only.
    pub steam_id: u64,
    /// QR only.
    pub challenge_url: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PollResult {
    pub new_client_id: Option<u64>,
    pub new_challenge_url: Option<String>,
    pub refresh_token: Option<String>,
    pub account_name: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct CdnServer {
    /// `CDN`, `SteamCache`, ...
    pub kind: String,
    pub host: String,
    pub vhost: String,
    /// `none` | `optional` | `mandatory`
    pub https_support: String,
    pub allowed_app_ids: Vec<u32>,
    pub weighted_load: f32,
    pub use_as_proxy: bool,
}

/// App/depot ids granted by the account's licenses (package `appids` + `depotids`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Ownership {
    pub app_ids: BTreeSet<u32>,
    pub depot_ids: BTreeSet<u32>,
}

impl Ownership {
    /// DepotDownloader's `AccountHasAccess`: depot listed in a package's appids or depotids.
    pub fn has(&self, id: u32) -> bool {
        self.app_ids.contains(&id) || self.depot_ids.contains(&id)
    }
}

/// Steam IAuthenticationService (anonymous; Web API or CM).
#[async_trait]
pub trait SteamAuth: Send + Sync {
    async fn password_rsa_key(&self, account: &str) -> Result<RsaKey>;
    async fn begin_qr(&self) -> Result<AuthSessionStart>;
    async fn begin_credentials(
        &self,
        account: &str,
        encrypted_password_b64: &str,
        timestamp: u64,
    ) -> Result<AuthSessionStart>;
    async fn submit_guard_code(
        &self,
        client_id: u64,
        steam_id: u64,
        code: &str,
        kind: GuardKind,
    ) -> Result<()>;
    async fn poll(&self, client_id: u64, request_id: &[u8]) -> Result<PollResult>;
}

/// A logged-on CM session.
#[async_trait]
pub trait SteamSession: Send + Sync {
    /// A rotated refresh token obtained during logon, to be persisted.
    fn renewed_refresh_token(&self) -> Option<String>;
    async fn ownership(&self) -> Result<Ownership>;
    /// PICS product info for `app_ids` (unknown apps are omitted).
    async fn app_info(&self, app_ids: &[u32]) -> Result<Vec<super::appinfo::AppInfo>>;
    async fn manifest_request_code(
        &self,
        app_id: u32,
        depot_id: u32,
        manifest_gid: u64,
        branch: &str,
    ) -> Result<u64>;
    async fn cdn_servers(&self) -> Result<Vec<CdnServer>>;
}

#[async_trait]
pub trait SteamBackend: SteamAuth {
    /// Connects to a CM and logs on with a refresh token.
    async fn connect(&self, account: &str, refresh_token: &str) -> Result<Box<dyn SteamSession>>;
}
