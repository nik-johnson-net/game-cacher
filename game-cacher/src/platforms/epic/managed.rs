//! Epic managed updates: login, game listing and chunk prefill.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use serde_json::{Value, json};

use super::Epic;
use super::client::{self, Asset, Client, ManifestEntry, Rejected};
use super::manifest;
use crate::platforms::plan::{GamePlan, Outcome};
use crate::platforms::{
    AuthStatus, Game, LoginMethod, LoginRequest, LoginResponse, ManagedUpdates, PlatformCtx,
    UpdateReport, domains, host_matches,
};

const REFRESH_SECRET: &str = "epic.refresh_token";
const IDENTIFIER: &str = Epic::ID;
const WARM_CONCURRENCY: usize = 8;
/// Engine/tooling namespaces that appear in launcher assets but are not games.
const NON_GAME_NAMESPACES: &[&str] = &["ue"];
/// Manifest hosts in order of preference (substring match on the host).
const HOST_PREFERENCE: &[&str] = &["download.epicgames.com", "fastly", "akamai", "cloudflare"];

pub struct CachedToken {
    access: String,
    expires: Instant,
}

/// Accepts a bare code or the JSON shown at the Epic redirect page.
fn extract_code(input: &str) -> Option<String> {
    let t = input.trim();
    let code = if t.starts_with('{') {
        let v: Value = serde_json::from_str(t).ok()?;
        v["authorizationCode"].as_str()?.trim().to_string()
    } else {
        t.trim_matches(|c: char| c == '"' || c == '\'' || c.is_whitespace())
            .to_string()
    };
    (!code.is_empty() && code.chars().all(|c| c.is_ascii_alphanumeric())).then_some(code)
}

fn is_game(a: &Asset) -> bool {
    !a.app_name.is_empty() && !NON_GAME_NAMESPACES.contains(&a.namespace.as_str())
}

/// Picks the best manifest URL on a cacheable host: (host, path_and_query, base_dir).
fn pick_manifest(entries: &[ManifestEntry]) -> Result<(String, String, String)> {
    let mut best: Option<(usize, url::Url, &ManifestEntry)> = None;
    for e in entries {
        let Ok(u) = url::Url::parse(&e.uri) else {
            continue;
        };
        let Some(host) = u.host_str() else { continue };
        if !domains::EPIC.iter().any(|d| host_matches(d, host)) {
            continue;
        }
        let score = HOST_PREFERENCE
            .iter()
            .position(|p| host.contains(p))
            .unwrap_or(HOST_PREFERENCE.len());
        if best.as_ref().is_none_or(|(s, ..)| score < *s) {
            best = Some((score, u.clone(), e));
        }
    }
    let (_, u, entry) = best.context("no manifest URL on a cacheable Epic host")?;
    let host = u.host_str().unwrap_or_default().to_string();
    let base = u.path().rsplit_once('/').map_or("", |(b, _)| b).to_string();
    let mut query =
        url::form_urlencoded::Serializer::new(u.query().unwrap_or_default().to_string());
    for p in &entry.query_params {
        query.append_pair(&p.name, &p.value);
    }
    let query = query.finish();
    let pq = if query.is_empty() {
        u.path().to_string()
    } else {
        format!("{}?{query}", u.path())
    };
    Ok((host, pq, base))
}

impl Epic {
    fn api<'a>(&'a self, ctx: &'a PlatformCtx) -> Client<'a> {
        Client {
            http: &ctx.http,
            ep: &self.endpoints,
        }
    }

    async fn store_login(&self, ctx: &PlatformCtx, t: &client::Token) -> Result<String> {
        ctx.secrets.put(REFRESH_SECRET, &t.refresh_token).await?;
        let guard = self.state_lock.lock().await;
        let mut state = ctx.state.load().await?;
        let name = match (&t.display_name, state["account"]["display_name"].as_str()) {
            (Some(n), _) => n.clone(),
            (None, Some(old)) => old.to_string(),
            (None, None) => t.account_id.clone(),
        };
        state["account"] = json!({"id": t.account_id, "display_name": name});
        ctx.state.save(&state).await?;
        drop(guard);
        *self.token.lock().await = Some(CachedToken {
            access: t.access_token.clone(),
            expires: Instant::now() + Duration::from_secs(t.expires_in),
        });
        Ok(name)
    }

    async fn clear_login(&self, ctx: &PlatformCtx) -> Result<()> {
        ctx.secrets.delete(REFRESH_SECRET).await?;
        *self.token.lock().await = None;
        let _guard = self.state_lock.lock().await;
        let mut state = ctx.state.load().await?;
        if let Some(o) = state.as_object_mut() {
            o.remove("account");
        }
        ctx.state.save(&state).await
    }

    /// Cached access token, refreshed through the stored refresh token when needed.
    async fn access_token(&self, ctx: &PlatformCtx) -> Result<String> {
        let mut guard = self.token.lock().await;
        if let Some(t) = guard.as_ref()
            && t.expires > Instant::now() + Duration::from_secs(60)
        {
            return Ok(t.access.clone());
        }
        let refresh = ctx
            .secrets
            .get(REFRESH_SECRET)
            .await?
            .context("not logged in to Epic")?;
        let params = [
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh.as_str()),
            ("token_type", "eg1"),
        ];
        match self.api(ctx).token(&params).await {
            Ok(t) => {
                drop(guard);
                self.store_login(ctx, &t).await?;
                Ok(t.access_token)
            }
            Err(e) if e.is::<Rejected>() => {
                *guard = None;
                drop(guard);
                self.clear_login(ctx).await?;
                bail!("Epic session expired or revoked ({e}); log in again")
            }
            Err(e) => Err(e),
        }
    }

    /// Fetches launcher assets and refreshes the metadata cached in state.
    async fn fresh_assets(&self, ctx: &PlatformCtx, token: &str) -> Result<Vec<Asset>> {
        let mut assets: Vec<Asset> = self
            .api(ctx)
            .assets(token)
            .await?
            .into_iter()
            .filter(is_game)
            .collect();
        assets.dedup_by(|a, b| a.app_name == b.app_name);
        let _guard = self.state_lock.lock().await;
        let mut state = ctx.state.load().await?;
        let old = state["assets"].clone();
        let mut cache = serde_json::Map::new();
        for a in &assets {
            let title = old[&a.app_name]["title"].clone();
            cache.insert(
                a.app_name.clone(),
                json!({"namespace": a.namespace, "catalog_item_id": a.catalog_item_id,
                       "build_version": a.build_version, "title": title}),
            );
        }
        state["assets"] = Value::Object(cache);
        ctx.state.save(&state).await?;
        Ok(assets)
    }

    async fn plan(&self, ctx: &PlatformCtx, app: &str) -> Result<GamePlan> {
        let token = self.access_token(ctx).await?;
        let assets = self.fresh_assets(ctx, &token).await?;
        let asset = assets
            .iter()
            .find(|a| a.app_name == app)
            .ok_or_else(|| anyhow!("game {app} not found in your Epic library"))?;
        let state = ctx.state.load().await?;
        let name = state["assets"][app]["title"].as_str().unwrap_or(app);
        let mut plan = GamePlan::new(IDENTIFIER, app, name);
        let previous = state["games"][app]["build_version"]
            .as_str()
            .map(String::from);
        plan.version_changed = previous.as_deref() != Some(asset.build_version.as_str());
        plan.previous_version = previous;
        plan.current_version = Some(asset.build_version.clone());
        let entries = self.api(ctx).manifests(&token, asset).await?;
        let (host, pq, base) = pick_manifest(&entries)?;
        let raw = ctx
            .prefill
            .get(IDENTIFIER, &host, &pq)
            .await
            .context("downloading manifest")?;
        let m = manifest::parse(&raw).context("parsing manifest")?;
        let g = plan.group("", true);
        for (chunk, path) in m.chunks.iter().zip(m.chunk_paths()) {
            let size = u64::try_from(chunk.file_size).ok().filter(|&s| s > 0);
            plan.add(format!("{base}/{path}"), size, g);
        }
        plan.host = host;
        plan.concurrency = WARM_CONCURRENCY;
        Ok(plan)
    }
}

#[async_trait]
impl ManagedUpdates for Epic {
    fn login_methods(&self) -> &'static [LoginMethod] {
        &[LoginMethod::AuthCode]
    }

    async fn auth_status(&self, ctx: &PlatformCtx) -> Result<AuthStatus> {
        let logged_in = ctx.secrets.get(REFRESH_SECRET).await?.is_some();
        let account = if logged_in {
            let state = ctx.state.load().await?;
            state["account"]["display_name"].as_str().map(String::from)
        } else {
            None
        };
        let redirect = format!(
            "https://www.epicgames.com/id/api/redirect?clientId={}&responseType=code",
            client::CLIENT_ID
        );
        let enc: String = url::form_urlencoded::byte_serialize(redirect.as_bytes()).collect();
        Ok(AuthStatus {
            logged_in,
            account,
            required: true,
            auth_url: Some(format!(
                "https://www.epicgames.com/id/login?redirectUrl={enc}"
            )),
        })
    }

    async fn login(&self, ctx: &PlatformCtx, req: LoginRequest) -> Result<LoginResponse> {
        let LoginRequest::AuthCode { code } = req else {
            return Ok(LoginResponse::Failed {
                message: "Epic only supports login with an authorization code".into(),
            });
        };
        let Some(code) = extract_code(&code) else {
            return Ok(LoginResponse::Failed {
                message: "Could not find an authorization code in the pasted text".into(),
            });
        };
        let params = [
            ("grant_type", "authorization_code"),
            ("code", code.as_str()),
            ("token_type", "eg1"),
        ];
        match self.api(ctx).token(&params).await {
            Ok(t) => Ok(LoginResponse::LoggedIn {
                account: self.store_login(ctx, &t).await?,
            }),
            Err(e) if e.is::<Rejected>() => Ok(LoginResponse::Failed {
                message: format!("Epic rejected the code: {e}"),
            }),
            Err(e) => Err(e),
        }
    }

    async fn logout(&self, ctx: &PlatformCtx) -> Result<()> {
        if let Some(t) = self.token.lock().await.take()
            && let Err(e) = self.api(ctx).kill_session(&t.access).await
        {
            log::debug!("epic: session kill failed (ignored): {e:#}");
        }
        self.clear_login(ctx).await
    }

    async fn list_games(&self, ctx: &PlatformCtx) -> Result<Vec<Game>> {
        let token = self.access_token(ctx).await?;
        let assets = self.fresh_assets(ctx, &token).await?;
        let mut by_ns: BTreeMap<&str, Vec<String>> = BTreeMap::new();
        for a in &assets {
            by_ns
                .entry(&a.namespace)
                .or_default()
                .push(a.catalog_item_id.clone());
        }
        let mut titles = std::collections::HashMap::new();
        for (ns, ids) in by_ns {
            match self.api(ctx).titles(&token, ns, &ids).await {
                Ok(t) => titles.extend(t),
                Err(e) => log::warn!("epic: catalog lookup for namespace {ns} failed: {e:#}"),
            }
        }
        // Load late: the catalog lookups above are slow and must not clobber other writes.
        let guard = self.state_lock.lock().await;
        let mut state = ctx.state.load().await?;
        let mut games = Vec::new();
        for a in &assets {
            let cached = state["assets"][&a.app_name]["title"]
                .as_str()
                .map(String::from);
            let title = titles.get(&a.catalog_item_id).cloned().or(cached);
            state["assets"][&a.app_name]["title"] = json!(title);
            games.push(Game {
                id: a.app_name.clone(),
                name: title.unwrap_or_else(|| a.app_name.clone()),
                version: Some(a.build_version.clone()).filter(|v| !v.is_empty()),
                size_bytes: None,
                selected: false,
            });
        }
        ctx.state.save(&state).await?;
        drop(guard);
        games.sort_by_key(|g| g.name.to_lowercase());
        Ok(games)
    }

    async fn game_name(&self, ctx: &PlatformCtx, game_id: &str) -> String {
        let state = ctx.state.load().await.unwrap_or_default();
        state["assets"][game_id]["title"]
            .as_str()
            .unwrap_or(game_id)
            .to_string()
    }

    async fn plan_game(&self, ctx: &PlatformCtx, game_id: &str) -> Result<GamePlan> {
        self.plan(ctx, game_id).await
    }

    async fn commit(
        &self,
        ctx: &PlatformCtx,
        plan: &GamePlan,
        outcome: &Outcome,
        _report: &mut UpdateReport,
    ) -> Result<()> {
        if !outcome.succeeded(plan) || !plan.version_changed {
            return Ok(());
        }
        // Reload: the warm can take long and other writers may have saved meanwhile.
        let _guard = self.state_lock.lock().await;
        let mut state = ctx.state.load().await?;
        state["games"][&plan.game_id] = json!({ "build_version": plan.current_version });
        ctx.state.save(&state).await
    }
}

#[cfg(test)]
mod tests;
