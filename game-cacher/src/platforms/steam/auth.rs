//! Login state machine over IAuthenticationService: QR and password + Steam
//! Guard. In-progress sessions live in memory keyed by a random challenge id.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use base64::Engine;
use base64::prelude::BASE64_STANDARD;
use rsa::{BigUint, Pkcs1v15Encrypt, RsaPublicKey};
use tokio::sync::Mutex;

use super::backend::{GuardKind, PollResult, RsaKey, SteamAuth};
use crate::platforms::{GuardType, LoginResponse};

pub const SESSION_TTL: Duration = Duration::from_secs(5 * 60);
/// Polls after submitting a guard code before answering `pending`.
const GUARD_POLLS: usize = 5;
const MAX_POLL_DELAY: Duration = Duration::from_secs(2);

/// Result of a login step: either a response for the UI, or fresh credentials
/// the caller must persist before answering `logged_in`.
#[derive(Debug, PartialEq, Eq)]
pub enum Step {
    Respond(LoginResponse),
    Success {
        account: String,
        refresh_token: String,
    },
}

struct Pending {
    client_id: u64,
    request_id: Vec<u8>,
    steam_id: u64,
    /// Username for credential logins (fallback if the poll omits it).
    account: Option<String>,
    code_type: Option<GuardKind>,
    interval: f32,
    created: Instant,
}

pub struct LoginFlow {
    sessions: Mutex<HashMap<String, Pending>>,
    ttl: Duration,
}

impl LoginFlow {
    pub fn new(ttl: Duration) -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            ttl,
        }
    }

    pub async fn start_qr(&self, auth: &dyn SteamAuth) -> LoginResponse {
        let start = match auth.begin_qr().await {
            Ok(s) => s,
            Err(e) => return failed(e.context("starting QR login")),
        };
        let url = start.challenge_url.clone();
        let challenge_id = self
            .insert(Pending {
                client_id: start.client_id,
                request_id: start.request_id,
                steam_id: 0,
                account: None,
                code_type: None,
                interval: start.interval,
                created: Instant::now(),
            })
            .await;
        LoginResponse::Qr { challenge_id, url }
    }

    pub async fn start_password(
        &self,
        auth: &dyn SteamAuth,
        username: &str,
        password: &str,
    ) -> Step {
        let start = async {
            let key = auth.password_rsa_key(username).await?;
            let encrypted = encrypt_password(&key, password)?;
            auth.begin_credentials(username, &encrypted, key.timestamp)
                .await
        };
        let start = match start.await {
            Ok(s) => s,
            Err(e) => return Step::Respond(failed(e.context("starting password login"))),
        };
        let code_type = start
            .allowed_confirmations
            .iter()
            .copied()
            .find(|c| matches!(c, GuardKind::EmailCode | GuardKind::DeviceCode));
        let needs_confirmation = start.allowed_confirmations.iter().any(|c| {
            matches!(
                c,
                GuardKind::DeviceConfirmation | GuardKind::EmailConfirmation
            )
        });
        let challenge_id = self
            .insert(Pending {
                client_id: start.client_id,
                request_id: start.request_id,
                steam_id: start.steam_id,
                account: Some(username.to_string()),
                code_type,
                interval: start.interval,
                created: Instant::now(),
            })
            .await;
        let guard = |guard_type| {
            Step::Respond(LoginResponse::GuardRequired {
                challenge_id: challenge_id.clone(),
                guard_type,
            })
        };
        match code_type {
            Some(GuardKind::EmailCode) => guard(GuardType::Email),
            Some(_) => guard(GuardType::Device),
            None if needs_confirmation => guard(GuardType::DeviceConfirmation),
            None => self.poll(auth, &challenge_id).await,
        }
    }

    pub async fn submit_guard(&self, auth: &dyn SteamAuth, challenge_id: &str, code: &str) -> Step {
        let Some((client_id, steam_id, code_type, interval)) = self
            .with_session(challenge_id, |p| {
                (p.client_id, p.steam_id, p.code_type, p.interval)
            })
            .await
        else {
            return Step::Respond(expired());
        };
        let kind = code_type.unwrap_or(GuardKind::DeviceCode);
        if let Err(e) = auth
            .submit_guard_code(client_id, steam_id, code.trim(), kind)
            .await
        {
            // Keep the session so the user can retry with another code.
            return Step::Respond(failed(e.context("Steam Guard code rejected")));
        }
        let delay = Duration::from_secs_f32(interval.max(0.0)).min(MAX_POLL_DELAY);
        for attempt in 0..GUARD_POLLS {
            if attempt > 0 {
                tokio::time::sleep(delay).await;
            }
            match self.poll(auth, challenge_id).await {
                Step::Respond(LoginResponse::Pending { .. }) => continue,
                other => return other,
            }
        }
        Step::Respond(LoginResponse::Pending {
            challenge_id: challenge_id.to_string(),
        })
    }

    /// One poll of the auth session (QR, device confirmation, after a guard code).
    pub async fn poll(&self, auth: &dyn SteamAuth, challenge_id: &str) -> Step {
        let Some((client_id, request_id)) = self
            .with_session(challenge_id, |p| (p.client_id, p.request_id.clone()))
            .await
        else {
            return Step::Respond(expired());
        };
        let result = match auth.poll(client_id, &request_id).await {
            Ok(r) => r,
            Err(e) => {
                self.sessions.lock().await.remove(challenge_id);
                return Step::Respond(failed(e.context("login was not approved")));
            }
        };
        self.apply_poll(challenge_id, result).await
    }

    async fn apply_poll(&self, challenge_id: &str, result: PollResult) -> Step {
        let mut sessions = self.sessions.lock().await;
        if let Some(token) = result.refresh_token.filter(|t| !t.is_empty()) {
            let pending = sessions.remove(challenge_id);
            let account = result
                .account_name
                .filter(|a| !a.is_empty())
                .or_else(|| pending.and_then(|p| p.account))
                .unwrap_or_default();
            return Step::Success {
                account,
                refresh_token: token,
            };
        }
        let Some(pending) = sessions.get_mut(challenge_id) else {
            return Step::Respond(expired());
        };
        if let Some(id) = result.new_client_id.filter(|&id| id != 0) {
            pending.client_id = id;
        }
        match result.new_challenge_url.filter(|u| !u.is_empty()) {
            Some(url) => Step::Respond(LoginResponse::Qr {
                challenge_id: challenge_id.to_string(),
                url,
            }),
            None => Step::Respond(LoginResponse::Pending {
                challenge_id: challenge_id.to_string(),
            }),
        }
    }

    async fn insert(&self, pending: Pending) -> String {
        let id = uuid::Uuid::new_v4().simple().to_string();
        let mut sessions = self.sessions.lock().await;
        let ttl = self.ttl;
        sessions.retain(|_, p| p.created.elapsed() < ttl);
        sessions.insert(id.clone(), pending);
        id
    }

    async fn with_session<T>(&self, id: &str, f: impl FnOnce(&Pending) -> T) -> Option<T> {
        let mut sessions = self.sessions.lock().await;
        let ttl = self.ttl;
        sessions.retain(|_, p| p.created.elapsed() < ttl);
        sessions.get(id).map(f)
    }
}

/// RSA PKCS#1 v1.5 encryption with the key from GetPasswordRSAPublicKey, base64-encoded.
pub fn encrypt_password(key: &RsaKey, password: &str) -> Result<String> {
    let n = BigUint::parse_bytes(key.modulus_hex.as_bytes(), 16).context("bad RSA modulus")?;
    let e = BigUint::parse_bytes(key.exponent_hex.as_bytes(), 16).context("bad RSA exponent")?;
    let public = RsaPublicKey::new(n, e).context("invalid RSA key")?;
    let encrypted = public
        .encrypt(
            &mut rsa::rand_core::OsRng,
            Pkcs1v15Encrypt,
            password.as_bytes(),
        )
        .context("encrypting password")?;
    Ok(BASE64_STANDARD.encode(encrypted))
}

fn failed(e: anyhow::Error) -> LoginResponse {
    LoginResponse::Failed {
        message: format!("{e:#}"),
    }
}

fn expired() -> LoginResponse {
    LoginResponse::Failed {
        message: "login session expired or unknown; start again".into(),
    }
}
