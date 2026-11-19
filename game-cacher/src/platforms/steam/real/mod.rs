//! Production backend: auth over the Web API, everything else over a CM session.

mod cm;
mod webapi;

use anyhow::Result;
use async_trait::async_trait;

use super::backend::{
    AuthSessionStart, GuardKind, PollResult, RsaKey, SteamAuth, SteamBackend, SteamSession,
};

pub struct RealBackend {
    auth: webapi::WebApiAuth,
}

impl RealBackend {
    pub fn new(http: reqwest::Client) -> Self {
        Self {
            auth: webapi::WebApiAuth::new(http, webapi::DEFAULT_API_BASE),
        }
    }
}

#[async_trait]
impl SteamAuth for RealBackend {
    async fn password_rsa_key(&self, account: &str) -> Result<RsaKey> {
        self.auth.password_rsa_key(account).await
    }
    async fn begin_qr(&self) -> Result<AuthSessionStart> {
        self.auth.begin_qr().await
    }
    async fn begin_credentials(
        &self,
        account: &str,
        encrypted_password_b64: &str,
        timestamp: u64,
    ) -> Result<AuthSessionStart> {
        self.auth
            .begin_credentials(account, encrypted_password_b64, timestamp)
            .await
    }
    async fn submit_guard_code(
        &self,
        client_id: u64,
        steam_id: u64,
        code: &str,
        kind: GuardKind,
    ) -> Result<()> {
        self.auth
            .submit_guard_code(client_id, steam_id, code, kind)
            .await
    }
    async fn poll(&self, client_id: u64, request_id: &[u8]) -> Result<PollResult> {
        self.auth.poll(client_id, request_id).await
    }
}

#[async_trait]
impl SteamBackend for RealBackend {
    async fn connect(&self, account: &str, refresh_token: &str) -> Result<Box<dyn SteamSession>> {
        Ok(Box::new(cm::connect(account, refresh_token).await?))
    }
}
