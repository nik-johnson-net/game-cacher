//! Windows Update: passive caching + managed prefill of a user-entered URL list.
use anyhow::Result;
use async_trait::async_trait;
use serde_json::json;
use url::Url;

use super::{
    AuthStatus, CachePolicy, Game, LoginMethod, LoginRequest, LoginResponse, ManagedUpdates,
    Platform, PlatformCtx, UpdateReport, domains, host_matches,
};

pub struct WindowsUpdate {}

impl WindowsUpdate {
    pub fn new() -> Self {
        Self {}
    }
}

impl Default for WindowsUpdate {
    fn default() -> Self {
        Self::new()
    }
}

impl Platform for WindowsUpdate {
    fn id(&self) -> &'static str {
        "windowsupdate"
    }
    fn name(&self) -> &'static str {
        "Windows Update"
    }
    fn description(&self) -> &'static str {
        "Caches Windows Update and Office CDN downloads. Specific update files can be prefetched by URL."
    }
    fn domains(&self) -> &'static [&'static str] {
        domains::WINDOWS_UPDATE
    }
    fn cache_policy(&self, path: &str) -> CachePolicy {
        // Certificate trust lists change often and must never be served stale.
        let file = path
            .rsplit('/')
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        let file = file.split('?').next().unwrap_or_default();
        if [
            "authrootstl.cab",
            "pinrulesstl.cab",
            "disallowedcertstl.cab",
        ]
        .contains(&file)
        {
            CachePolicy::Bypass
        } else {
            CachePolicy::Cache
        }
    }
    fn managed(&self) -> Option<&dyn ManagedUpdates> {
        Some(self)
    }
}

fn file_name(url: &str) -> String {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    path.trim_end_matches('/')
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(url)
        .to_string()
}

/// Validates a user URL and returns `(host, path_and_query)` for the HTTP-only cache.
fn parse_target(raw: &str) -> Result<(String, String), String> {
    let url = Url::parse(raw).map_err(|e| format!("invalid URL: {e}"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(format!("unsupported scheme {:?}", url.scheme()));
    }
    let host = url.host_str().unwrap_or_default().to_string();
    if !domains::WINDOWS_UPDATE
        .iter()
        .any(|d| host_matches(d, &host))
    {
        return Err(format!("host {host:?} is not a Windows Update domain"));
    }
    let mut pq = url.path().to_string();
    if let Some(q) = url.query() {
        pq.push('?');
        pq.push_str(q);
    }
    Ok((host, pq))
}

#[async_trait]
impl ManagedUpdates for WindowsUpdate {
    fn login_methods(&self) -> &'static [LoginMethod] {
        &[LoginMethod::ManualList]
    }

    async fn auth_status(&self, _ctx: &PlatformCtx) -> Result<AuthStatus> {
        Ok(AuthStatus {
            logged_in: false,
            account: None,
            required: false,
            auth_url: None,
        })
    }

    async fn login(&self, _ctx: &PlatformCtx, _req: LoginRequest) -> Result<LoginResponse> {
        Ok(LoginResponse::Failed {
            message: "not required".into(),
        })
    }

    async fn logout(&self, _ctx: &PlatformCtx) -> Result<()> {
        Ok(())
    }

    async fn list_games(&self, ctx: &PlatformCtx) -> Result<Vec<Game>> {
        let state = ctx.state.load().await.unwrap_or_default();
        let urls = ctx.settings.get().await.selected(self.id());
        Ok(urls
            .into_iter()
            .map(|u| {
                let size = state[&u]["size"].as_u64();
                Game {
                    name: file_name(&u),
                    version: size.map(|s| s.to_string()),
                    size_bytes: size,
                    selected: false,
                    id: u,
                }
            })
            .collect())
    }

    fn validate_selection(&self, ids: &[String]) -> Result<()> {
        for id in ids {
            parse_target(id).map_err(|e| anyhow::anyhow!("{id}: {e}"))?;
        }
        Ok(())
    }

    async fn update_game(&self, ctx: &PlatformCtx, game_id: &str) -> Result<UpdateReport> {
        let mut report = UpdateReport {
            game_id: game_id.to_string(),
            name: file_name(game_id),
            ..Default::default()
        };
        let mut state = ctx.state.load().await?;
        if !state.is_object() {
            state = json!({});
        }
        report.previous_version = state[game_id]["size"].as_u64().map(|s| s.to_string());
        let (host, pq) = match parse_target(game_id) {
            Ok(t) => t,
            Err(e) => {
                report.error = Some(e);
                return Ok(report);
            }
        };
        match ctx.prefill.warm(self.id(), &host, &pq).await {
            Ok(w) => {
                report.bytes_downloaded = w.bytes_downloaded;
                report.bytes_already_cached = w.bytes_already_cached;
                report.files = 1;
                report.updated = w.bytes_downloaded > 0;
                let size = w.bytes_downloaded + w.bytes_already_cached;
                report.current_version = Some(size.to_string());
                state[game_id] = json!({ "size": size });
                ctx.state.save(&state).await?;
            }
            Err(e) => report.error = Some(format!("{e:#}")),
        }
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use bytes::Bytes;

    use super::*;
    use crate::platforms::{Prefill, WarmResult};
    use crate::secrets::SecretStore;
    use crate::settings::SettingsStore;
    use crate::storage::MemoryStorage;

    struct NoSecrets;

    #[async_trait]
    impl SecretStore for NoSecrets {
        fn name(&self) -> &'static str {
            "test"
        }
        async fn get(&self, _: &str) -> Result<Option<String>> {
            Ok(None)
        }
        async fn put(&self, _: &str, _: &str) -> Result<()> {
            Ok(())
        }
        async fn delete(&self, _: &str) -> Result<()> {
            Ok(())
        }
    }

    #[derive(Default)]
    struct MockPrefill {
        calls: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl Prefill for MockPrefill {
        async fn warm(&self, id: &str, host: &str, p: &str) -> Result<WarmResult> {
            self.calls.lock().unwrap().push(format!("{id} {host}{p}"));
            Ok(WarmResult {
                bytes_downloaded: 500,
                bytes_already_cached: 0,
            })
        }
        async fn get(&self, _: &str, _: &str, _: &str) -> Result<Bytes> {
            anyhow::bail!("unused")
        }
        async fn get_range(&self, _: &str, _: &str, _: &str, _: u64, _: u64) -> Result<Bytes> {
            anyhow::bail!("unused")
        }
    }

    async fn ctx(prefill: Arc<MockPrefill>) -> PlatformCtx {
        let settings = Arc::new(
            SettingsStore::load(Arc::new(MemoryStorage::new()))
                .await
                .unwrap(),
        );
        PlatformCtx {
            secrets: Arc::new(NoSecrets),
            state: settings.state_store("windowsupdate"),
            settings,
            http: reqwest::Client::new(),
            prefill,
        }
    }

    #[test]
    fn bypasses_trust_lists() {
        let w = WindowsUpdate::new();
        assert_eq!(
            w.cache_policy("/msdownload/update/v3/static/trustedr/en/authrootstl.cab"),
            CachePolicy::Bypass
        );
        assert_eq!(
            w.cache_policy("/d/msdownload/update/software/x.cab"),
            CachePolicy::Cache
        );
    }

    #[tokio::test]
    async fn warms_valid_url_and_https_becomes_http() {
        let prefill = Arc::new(MockPrefill::default());
        let ctx = ctx(prefill.clone()).await;
        let w = WindowsUpdate::new();
        let url = "https://download.windowsupdate.com/d/msdownload/x.cab?a=1";
        let r = w.update_game(&ctx, url).await.unwrap();
        assert_eq!(r.error, None);
        assert!(r.updated);
        assert_eq!(r.name, "x.cab");
        assert_eq!(r.bytes_downloaded, 500);
        assert_eq!(
            *prefill.calls.lock().unwrap(),
            vec!["windowsupdate download.windowsupdate.com/d/msdownload/x.cab?a=1"]
        );
        let again = w.update_game(&ctx, url).await.unwrap();
        assert_eq!(again.previous_version.as_deref(), Some("500"));
    }

    #[tokio::test]
    async fn rejects_foreign_hosts_and_schemes() {
        let prefill = Arc::new(MockPrefill::default());
        let ctx = ctx(prefill.clone()).await;
        let w = WindowsUpdate::new();
        for u in [
            "http://example.com/x.cab",
            "ftp://download.windowsupdate.com/x.cab",
            "not a url",
        ] {
            let r = w.update_game(&ctx, u).await.unwrap();
            assert!(r.error.is_some(), "{u}");
            assert!(!r.updated);
        }
        assert!(prefill.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn lists_selected_urls() {
        let ctx = ctx(Arc::new(MockPrefill::default())).await;
        ctx.settings
            .update(|s| {
                s.managed
                    .entry("windowsupdate".into())
                    .or_default()
                    .selected = vec!["http://dl.delivery.mp.microsoft.com/a/b/KB1.msu".into()];
            })
            .await
            .unwrap();
        let w = WindowsUpdate::new();
        let games = w.list_games(&ctx).await.unwrap();
        assert_eq!(games.len(), 1);
        assert_eq!(games[0].name, "KB1.msu");
        assert_eq!(games[0].version, None);
        assert_eq!(w.login_methods(), &[LoginMethod::ManualList]);
    }
}
