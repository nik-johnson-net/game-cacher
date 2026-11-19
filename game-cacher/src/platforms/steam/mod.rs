//! Steam: passive caching keyed by the Steam client user agent, plus managed
//! updates (QR / password + Steam Guard login, PICS update checks, depot
//! manifest → chunk prefill).

mod appinfo;
mod auth;
mod backend;
mod kv;
mod manifest;
mod real;
mod state;
mod update;

#[cfg(test)]
mod tests;

use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use async_trait::async_trait;
use tokio::sync::Mutex;

use self::auth::{LoginFlow, Step};
use self::backend::{SteamBackend, SteamSession};
use super::{
    AuthStatus, Game, LoginMethod, LoginRequest, LoginResponse, ManagedUpdates, Platform,
    PlatformCtx, UpdateReport, domains,
};

pub const REFRESH_TOKEN_SECRET: &str = "steam.refresh_token";
/// How long a logged-on CM session is reused across calls.
const SESSION_REUSE: Duration = Duration::from_secs(10 * 60);

type BackendFactory = Box<dyn Fn(&PlatformCtx) -> Arc<dyn SteamBackend> + Send + Sync>;

pub struct Steam {
    managed: SteamManaged,
}

impl Steam {
    pub fn new() -> Self {
        Self {
            managed: SteamManaged::new(
                Box::new(|ctx| Arc::new(real::RealBackend::new(ctx.http.clone()))),
                auth::SESSION_TTL,
            ),
        }
    }
}

impl Default for Steam {
    fn default() -> Self {
        Self::new()
    }
}

impl Platform for Steam {
    fn id(&self) -> &'static str {
        "steam"
    }
    fn name(&self) -> &'static str {
        "Steam"
    }
    fn description(&self) -> &'static str {
        "Caches Steam depot downloads. The Steam client switches to HTTP when lancache.steamcontent.com resolves to the cache."
    }
    fn domains(&self) -> &'static [&'static str] {
        domains::STEAM
    }
    fn matches_user_agent(&self, user_agent: &str) -> bool {
        user_agent.starts_with("Valve/Steam HTTP Client 1.0")
    }
    fn managed(&self) -> Option<&dyn ManagedUpdates> {
        Some(&self.managed)
    }
}

pub struct SteamManaged {
    factory: BackendFactory,
    backend: OnceLock<Arc<dyn SteamBackend>>,
    login: LoginFlow,
    session: Mutex<Option<(Instant, Arc<dyn SteamSession>)>>,
    /// Serializes read-modify-write of `state/steam.json`.
    state_lock: Mutex<()>,
}

impl SteamManaged {
    fn new(factory: BackendFactory, login_ttl: Duration) -> Self {
        Self {
            factory,
            backend: OnceLock::new(),
            login: LoginFlow::new(login_ttl),
            session: Mutex::new(None),
            state_lock: Mutex::new(()),
        }
    }

    fn backend(&self, ctx: &PlatformCtx) -> &dyn SteamBackend {
        self.backend.get_or_init(|| (self.factory)(ctx)).as_ref()
    }

    /// A logged-on session, reused for [`SESSION_REUSE`] (the flag says whether
    /// it was reused). Persists rotated refresh tokens.
    async fn session(&self, ctx: &PlatformCtx) -> Result<(Arc<dyn SteamSession>, bool)> {
        let mut cached = self.session.lock().await;
        if let Some((created, s)) = cached.as_ref()
            && created.elapsed() < SESSION_REUSE
        {
            return Ok((s.clone(), true));
        }
        let token = ctx
            .secrets
            .get(REFRESH_TOKEN_SECRET)
            .await?
            .context("not logged in to Steam")?;
        let account = state::load(ctx.state.as_ref())
            .await?
            .account
            .context("Steam account name unknown; log in again")?;
        let session: Arc<dyn SteamSession> = self
            .backend(ctx)
            .connect(&account, &token)
            .await
            .context("Steam logon failed")?
            .into();
        if let Some(new) = session.renewed_refresh_token().filter(|t| *t != token) {
            ctx.secrets.put(REFRESH_TOKEN_SECRET, &new).await?;
            log::info!("steam refresh token renewed");
        }
        *cached = Some((Instant::now(), session.clone()));
        Ok((session, false))
    }

    /// Drops the session after a failure; true if a retry with a fresh one makes sense.
    async fn should_retry(&self, reused: bool, e: &anyhow::Error) -> bool {
        self.drop_session().await;
        if reused {
            log::info!("steam: retrying with a new session after: {e:#}");
        }
        reused
    }

    async fn drop_session(&self) {
        self.session.lock().await.take();
    }

    async fn finish_login(&self, ctx: &PlatformCtx, step: Step) -> Result<LoginResponse> {
        match step {
            Step::Respond(r) => Ok(r),
            Step::Success {
                account,
                refresh_token,
            } => {
                ctx.secrets
                    .put(REFRESH_TOKEN_SECRET, &refresh_token)
                    .await?;
                self.update_state(ctx, |s| s.account = Some(account.clone()))
                    .await?;
                self.drop_session().await;
                log::info!("steam login succeeded for {account}");
                Ok(LoginResponse::LoggedIn { account })
            }
        }
    }

    async fn update_state(
        &self,
        ctx: &PlatformCtx,
        f: impl FnOnce(&mut state::SteamState),
    ) -> Result<()> {
        let _guard = self.state_lock.lock().await;
        let mut st = state::load(ctx.state.as_ref()).await?;
        f(&mut st);
        state::save(ctx.state.as_ref(), &st).await
    }

    async fn games(&self, session: &dyn SteamSession) -> Result<Vec<Game>> {
        let owned = session.ownership().await?;
        let ids: Vec<u32> = owned.app_ids.iter().copied().collect();
        let mut games: Vec<Game> = session
            .app_info(&ids)
            .await?
            .into_iter()
            .filter(|a| a.app_type == "game")
            .map(|a| {
                let sizes: Vec<u64> = appinfo::filter_depots(&a, &owned)
                    .iter()
                    .filter(|d| d.public_manifest.is_some())
                    .filter_map(|d| d.size)
                    .collect();
                Game {
                    id: a.app_id.to_string(),
                    name: a.name.clone(),
                    version: a.public_buildid.clone(),
                    size_bytes: (!sizes.is_empty()).then(|| sizes.iter().sum()),
                    selected: false,
                }
            })
            .collect();
        games.sort_by_key(|g| g.name.to_lowercase());
        Ok(games)
    }

    async fn update(&self, ctx: &PlatformCtx, game_id: &str) -> Result<UpdateReport> {
        let app_id: u32 = game_id
            .parse()
            .with_context(|| format!("invalid Steam app id {game_id:?}"))?;
        let previous = state::load(ctx.state.as_ref())
            .await?
            .apps
            .remove(game_id)
            .unwrap_or_default();
        let (report, next) = loop {
            let (session, reused) = self.session(ctx).await?;
            match update::update_app(session.as_ref(), ctx.prefill.as_ref(), app_id, &previous)
                .await
            {
                Ok(r) => break r,
                Err(e) if self.should_retry(reused, &e).await => continue,
                Err(e) => return Err(e),
            }
        };
        if next != previous {
            self.update_state(ctx, |s| {
                s.apps.insert(game_id.to_string(), next);
            })
            .await?;
        }
        Ok(report)
    }
}

#[async_trait]
impl ManagedUpdates for SteamManaged {
    fn login_methods(&self) -> &'static [LoginMethod] {
        &[LoginMethod::Qr, LoginMethod::Password]
    }

    async fn auth_status(&self, ctx: &PlatformCtx) -> Result<AuthStatus> {
        let logged_in = ctx.secrets.get(REFRESH_TOKEN_SECRET).await?.is_some();
        let account = state::load(ctx.state.as_ref()).await?.account;
        Ok(AuthStatus {
            logged_in,
            account: account.filter(|_| logged_in),
            required: true,
            auth_url: None,
        })
    }

    async fn login(&self, ctx: &PlatformCtx, req: LoginRequest) -> Result<LoginResponse> {
        let auth = self.backend(ctx);
        let step = match req {
            LoginRequest::Qr => Step::Respond(self.login.start_qr(auth).await),
            LoginRequest::QrPoll { challenge_id } => self.login.poll(auth, &challenge_id).await,
            LoginRequest::Password { username, password } => {
                self.login
                    .start_password(auth, username.trim(), &password)
                    .await
            }
            LoginRequest::Guard { challenge_id, code } => {
                self.login.submit_guard(auth, &challenge_id, &code).await
            }
            LoginRequest::AuthCode { .. } => Step::Respond(LoginResponse::Failed {
                message: "Steam does not support auth-code login".into(),
            }),
        };
        self.finish_login(ctx, step).await
    }

    async fn logout(&self, ctx: &PlatformCtx) -> Result<()> {
        ctx.secrets.delete(REFRESH_TOKEN_SECRET).await?;
        self.update_state(ctx, |s| s.account = None).await?;
        self.drop_session().await;
        Ok(())
    }

    async fn list_games(&self, ctx: &PlatformCtx) -> Result<Vec<Game>> {
        loop {
            let (session, reused) = self.session(ctx).await?;
            match self.games(session.as_ref()).await {
                Ok(games) => return Ok(games),
                Err(e) if self.should_retry(reused, &e).await => continue,
                Err(e) => return Err(e),
            }
        }
    }

    async fn update_game(&self, ctx: &PlatformCtx, game_id: &str) -> Result<UpdateReport> {
        Ok(match self.update(ctx, game_id).await {
            Ok(report) => report,
            Err(e) => UpdateReport {
                game_id: game_id.to_string(),
                error: Some(format!("{e:#}")),
                ..Default::default()
            },
        })
    }
}
