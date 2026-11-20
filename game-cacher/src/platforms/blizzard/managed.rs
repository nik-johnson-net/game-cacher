//! Blizzard managed updates: poll the TACT patch server, then warm the build's CDN objects.

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use futures::stream::{self, StreamExt};
use serde_json::{Value, json};

use super::formats;
use super::tact::{self, cdn_path, parse_bpsv, parse_config, values};
use super::{Blizzard, domains};
use crate::platforms::{
    AuthStatus, Game, LoginMethod, LoginRequest, LoginResponse, ManagedUpdates, PlatformCtx,
    UpdateReport, host_matches,
};

const IDENTIFIER: &str = Blizzard::ID;
const REGION: &str = "us";
const FALLBACK_HOST: &str = "level3.blizzard.com";
const WARM_CONCURRENCY: usize = 8;

/// TACT product code -> display name.
const CATALOG: &[(&str, &str)] = &[
    ("wow", "World of Warcraft"),
    ("wow_classic", "WoW Classic"),
    ("wow_classic_era", "WoW Classic Era"),
    ("pro", "Overwatch 2"),
    ("fenris", "Diablo IV"),
    ("d3", "Diablo III"),
    ("hs", "Hearthstone"),
    ("s2", "StarCraft II"),
    ("s1", "StarCraft: Remastered"),
    ("hero", "Heroes of the Storm"),
    ("w3", "Warcraft III: Reforged"),
    ("wlby", "Crash Bandicoot 4"),
    ("viper", "Call of Duty: Black Ops 4"),
    ("odin", "Call of Duty: Modern Warfare"),
    ("auks", "Call of Duty"),
];

fn product_name(product: &str) -> String {
    CATALOG
        .iter()
        .find(|(c, _)| *c == product)
        .map_or(product, |(_, n)| n)
        .to_string()
}

/// Picks the first CDN host that is a known Blizzard cache domain.
fn pick_host(hosts: &str) -> String {
    hosts
        .split_whitespace()
        .find(|h| domains::BLIZZARD.iter().any(|d| host_matches(d, h)))
        .unwrap_or(FALLBACK_HOST)
        .to_string()
}

/// CDN object paths (excluding the two configs) to warm for a build, deduplicated in order.
fn plan_paths(
    path: &str,
    build: &std::collections::HashMap<String, String>,
    cdn: &std::collections::HashMap<String, String>,
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut add = |p: Option<String>| out.extend(p);
    for a in values(cdn, "archives") {
        add(cdn_path(path, "data", a, ".index"));
        add(cdn_path(path, "data", a, ""));
    }
    for a in values(cdn, "patch-archives") {
        add(cdn_path(path, "patch", a, ".index"));
        add(cdn_path(path, "patch", a, ""));
    }
    for key in ["file-index", "archive-group"] {
        for h in values(cdn, key) {
            add(cdn_path(path, "data", h, ".index"));
        }
    }
    for h in values(cdn, "patch-file-index") {
        add(cdn_path(path, "patch", h, ".index"));
    }
    for key in ["encoding", "root", "install", "download", "size"] {
        if let Some(ekey) = values(build, key).get(1) {
            add(cdn_path(path, "data", ekey, ""));
        }
    }
    let mut seen = std::collections::HashSet::new();
    out.retain(|p| seen.insert(p.clone()));
    out
}

impl Blizzard {
    async fn fetch_table(
        &self,
        ctx: &PlatformCtx,
        product: &str,
        what: &str,
    ) -> Result<tact::Bpsv> {
        let url = format!("{}/{product}/{what}", self.patch_base);
        let resp = ctx
            .http
            .get(&url)
            .send()
            .await
            .with_context(|| format!("GET {url}"))?;
        let text = resp
            .error_for_status()
            .with_context(|| format!("GET {url}"))?
            .text()
            .await?;
        Ok(parse_bpsv(&text))
    }

    async fn run_update(
        &self,
        ctx: &PlatformCtx,
        product: &str,
        report: &mut UpdateReport,
    ) -> Result<()> {
        let versions = self.fetch_table(ctx, product, "versions").await?;
        let Some(v) = versions.region(REGION) else {
            bail!("no versions listed for {product}");
        };
        let field = |m: &std::collections::HashMap<String, String>, k: &str| {
            m.get(k).cloned().unwrap_or_default()
        };
        let (build_hash, cdn_hash, version) = (
            field(v, "BuildConfig"),
            field(v, "CDNConfig"),
            field(v, "VersionsName"),
        );
        if build_hash.is_empty() || cdn_hash.is_empty() {
            bail!("versions for {product} lack BuildConfig/CDNConfig");
        }
        report.current_version = Some(version.clone());

        let mut state = ctx.state.load().await?;
        let prev = state.get(product).cloned().unwrap_or(Value::Null);
        report.previous_version = prev["version"].as_str().map(str::to_string);
        let changed =
            prev["build_config"] != build_hash.as_str() || prev["cdn_config"] != cdn_hash.as_str();

        let cdns = self.fetch_table(ctx, product, "cdns").await?;
        let Some(c) = cdns.region(REGION) else {
            bail!("no cdns listed for {product}");
        };
        let (host, path) = (pick_host(&field(c, "Hosts")), field(c, "Path"));
        if path.is_empty() {
            bail!("cdns for {product} lack Path");
        }

        let get_cfg = |hash: String| {
            let host = host.clone();
            let path = path.clone();
            async move {
                let p = cdn_path(&path, "config", &hash, "")
                    .with_context(|| format!("bad config hash {hash:?}"))?;
                let body = ctx
                    .prefill
                    .get(IDENTIFIER, &host, &p)
                    .await
                    .with_context(|| p.clone())?;
                Ok::<_, anyhow::Error>(parse_config(&String::from_utf8_lossy(&body)))
            }
        };
        let build_cfg = get_cfg(build_hash.clone()).await?;
        let cdn_cfg = get_cfg(cdn_hash.clone()).await?;

        // Every run warms the full object set: when nothing changed these are cache hits
        // (`bytes_already_cached`), and anything evicted or purged is fetched again.
        let planned = plan_paths(&path, &build_cfg, &cdn_cfg);
        self.warm_all(ctx, &host, planned.clone(), report, true)
            .await;
        match self
            .loose_paths(ctx, &host, &path, &build_cfg, &cdn_cfg, &planned)
            .await
        {
            Ok(loose) => self.warm_all(ctx, &host, loose, report, false).await,
            Err(e) => {
                report
                    .error
                    .get_or_insert_with(|| format!("loose files: {e:#}"));
            }
        }
        if report.error.is_none() && changed {
            // Failed objects leave the state untouched so the next run retries.
            state[product] = json!({
                "build_config": build_hash,
                "cdn_config": cdn_hash,
                "version": version,
                "updated_at": chrono::Utc::now().to_rfc3339(),
            });
            ctx.state.save(&state).await?;
        }
        report.updated = changed || report.bytes_downloaded > 0;
        Ok(())
    }

    /// Data paths of files stored outside archives: encoding-file ekeys not in any archive
    /// index and not already planned. Empty when the build has no encoding file.
    async fn loose_paths(
        &self,
        ctx: &PlatformCtx,
        host: &str,
        path: &str,
        build: &std::collections::HashMap<String, String>,
        cdn: &std::collections::HashMap<String, String>,
        planned: &[String],
    ) -> Result<Vec<String>> {
        let enc_key = values(build, "encoding").get(1).map(|k| k.to_string());
        let Some(enc) = enc_key.and_then(|k| cdn_path(path, "data", &k, "")) else {
            return Ok(Vec::new());
        };
        let body = ctx.prefill.get(IDENTIFIER, host, &enc).await?;
        let ekeys = formats::encoding_ekeys(&formats::decode_blte(&body).context(enc.clone())?)
            .context("parsing encoding file")?;

        let mut index_paths: Vec<String> = Vec::new();
        for a in values(cdn, "archives") {
            index_paths.extend(cdn_path(path, "data", a, ".index"));
        }
        let mut archived = std::collections::HashSet::new();
        let prefill = ctx.prefill.clone();
        let mut indices = stream::iter(index_paths)
            .map(|p| {
                let prefill = prefill.clone();
                let host = host.to_string();
                async move {
                    let body = prefill.get(IDENTIFIER, &host, &p).await?;
                    formats::index_keys(&body).with_context(|| p)
                }
            })
            .buffer_unordered(WARM_CONCURRENCY);
        while let Some(keys) = indices.next().await {
            archived.extend(keys?);
        }

        let planned: std::collections::HashSet<&String> = planned.iter().collect();
        let mut seen = std::collections::HashSet::new();
        Ok(ekeys
            .into_iter()
            .filter(|k| !archived.contains(k) && seen.insert(k.clone()))
            .filter_map(|k| cdn_path(path, "data", &hex::encode(k), ""))
            .filter(|p| !planned.contains(p))
            .collect())
    }

    async fn warm_all(
        &self,
        ctx: &PlatformCtx,
        host: &str,
        paths: Vec<String>,
        report: &mut UpdateReport,
        fatal: bool,
    ) {
        let mut results = stream::iter(paths)
            .map(|p| async move {
                ctx.prefill
                    .warm(IDENTIFIER, host, &p)
                    .await
                    .map_err(|e| e.context(p))
            })
            .buffer_unordered(WARM_CONCURRENCY);
        while let Some(r) = results.next().await {
            match r {
                Ok(w) => {
                    report.files += 1;
                    report.bytes_downloaded += w.bytes_downloaded;
                    report.bytes_already_cached += w.bytes_already_cached;
                }
                Err(e) => {
                    log::warn!("blizzard warm failed: {e:#}");
                    if fatal {
                        report.error.get_or_insert_with(|| format!("{e:#}"));
                    }
                }
            }
        }
    }
}

#[async_trait]
impl ManagedUpdates for Blizzard {
    fn login_methods(&self) -> &'static [LoginMethod] {
        &[LoginMethod::None]
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
        Ok(CATALOG
            .iter()
            .map(|(code, name)| Game {
                id: code.to_string(),
                name: name.to_string(),
                version: state[*code]["version"].as_str().map(str::to_string),
                size_bytes: None,
                selected: false,
            })
            .collect())
    }

    async fn update_game(&self, ctx: &PlatformCtx, game_id: &str) -> Result<UpdateReport> {
        let mut report = UpdateReport {
            game_id: game_id.to_string(),
            name: product_name(game_id),
            ..Default::default()
        };
        if let Err(e) = self.run_update(ctx, game_id, &mut report).await {
            report.error = Some(format!("{e:#}"));
        }
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    use bytes::Bytes;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::super::formats::testing::{blte, encoding, index};
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
        fail: Option<&'static str>,
        fail_loose: bool,
        /// Report everything as already cached instead of downloaded.
        cached: AtomicBool,
    }

    impl MockPrefill {
        fn calls(&self, kind: &str) -> Vec<String> {
            let mut v: Vec<String> = self
                .calls
                .lock()
                .unwrap()
                .iter()
                .filter_map(|c| c.strip_prefix(&format!("{kind} ")).map(str::to_string))
                .collect();
            v.sort();
            v
        }
    }

    const BUILD: &str = "aaaa1111";
    const CDN: &str = "bbbb2222";

    #[async_trait]
    impl Prefill for MockPrefill {
        async fn warm(&self, id: &str, host: &str, p: &str) -> Result<WarmResult> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("warm {id} {host}{p}"));
            if self.fail.is_some_and(|f| p.contains(f)) || (self.fail_loose && p.contains("/a3a3"))
            {
                bail!("boom");
            }
            Ok(if self.cached.load(Ordering::SeqCst) {
                WarmResult {
                    bytes_downloaded: 0,
                    bytes_already_cached: 11,
                }
            } else {
                WarmResult {
                    bytes_downloaded: 10,
                    bytes_already_cached: 1,
                }
            })
        }
        async fn get(&self, id: &str, host: &str, p: &str) -> Result<Bytes> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("get {id} {host}{p}"));
            let k = |n: u8| [n; 16];
            Ok(match p {
                _ if p.ends_with(BUILD) => Bytes::from(
                    "root = rootc\nencoding = encc encekey1\ninstall = instc instekey\ndownload = dlc dlekey\nsize = sizec sizekey\n",
                ),
                _ if p.ends_with(CDN) => Bytes::from(
                    "archives = arch1111 arch2222\npatch-archives = parc3333\nfile-index = fidx4444\n",
                ),
                // a1 and a2 live in archives; a3 is a loose file.
                _ if p.ends_with("arch1111.index") => Bytes::from(index(&[k(0xa1)])),
                _ if p.ends_with("arch2222.index") => Bytes::from(index(&[k(0xa2)])),
                _ if p.ends_with("encekey1") => Bytes::from(blte(&[(
                    b'Z',
                    &encoding(&[vec![
                        (k(1), vec![k(0xa1)]),
                        (k(2), vec![k(0xa2)]),
                        (k(3), vec![k(0xa3), k(0xa1)]),
                    ]]),
                )])),
                _ => bail!("unexpected {p}"),
            })
        }
        async fn get_range(&self, _: &str, _: &str, _: &str, _: u64, _: u64) -> Result<Bytes> {
            bail!("unused")
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
            state: settings.state_store("blizzard"),
            settings,
            http: reqwest::Client::new(),
            prefill,
        }
    }

    async fn patch_server(build: &str) -> MockServer {
        let server = MockServer::start().await;
        let versions = format!(
            "Region!STRING:0|BuildConfig!HEX:16|CDNConfig!HEX:16|VersionsName!String:0\n## seqn = 1\neu|x|y|0.0.1\nus|{build}|{CDN}|1.2.3\n"
        );
        let cdns = "Name!STRING:0|Path!STRING:0|Hosts!STRING:0\n## seqn = 1\nus|tpr/wow|example.org level3.blizzard.com us.cdn.blizzard.com\n";
        for (p, body) in [("/wow/versions", versions.as_str()), ("/wow/cdns", cdns)] {
            Mock::given(method("GET"))
                .and(path(p))
                .respond_with(ResponseTemplate::new(200).set_body_string(body))
                .mount(&server)
                .await;
        }
        server
    }

    #[test]
    fn host_selection() {
        assert_eq!(
            pick_host("a.example.org us.cdn.blizzard.com"),
            "us.cdn.blizzard.com"
        );
        assert_eq!(pick_host("a.example.org"), FALLBACK_HOST);
        assert_eq!(pick_host(""), FALLBACK_HOST);
    }

    #[tokio::test]
    async fn full_update_then_verify_then_repair() {
        let server = patch_server(BUILD).await;
        let b = Blizzard::with_patch_base(&server.uri());
        let prefill = Arc::new(MockPrefill::default());
        let ctx = ctx(prefill.clone()).await;

        let r = b.update_game(&ctx, "wow").await.unwrap();
        assert_eq!(r.error, None);
        assert!(r.updated);
        assert_eq!(r.name, "World of Warcraft");
        assert_eq!(r.current_version.as_deref(), Some("1.2.3"));
        assert_eq!(r.files, 12);
        assert_eq!(r.bytes_downloaded, 120);
        assert_eq!(r.bytes_already_cached, 12);
        assert_eq!(
            prefill.calls("get"),
            vec![
                "blizzard level3.blizzard.com/tpr/wow/config/aa/aa/aaaa1111",
                "blizzard level3.blizzard.com/tpr/wow/config/bb/bb/bbbb2222",
                "blizzard level3.blizzard.com/tpr/wow/data/ar/ch/arch1111.index",
                "blizzard level3.blizzard.com/tpr/wow/data/ar/ch/arch2222.index",
                "blizzard level3.blizzard.com/tpr/wow/data/en/ce/encekey1",
            ]
        );
        let h = "blizzard level3.blizzard.com/tpr/wow";
        let mut want: Vec<String> = [
            "data/ar/ch/arch1111.index",
            "data/ar/ch/arch1111",
            "data/ar/ch/arch2222.index",
            "data/ar/ch/arch2222",
            "patch/pa/rc/parc3333.index",
            "patch/pa/rc/parc3333",
            "data/fi/dx/fidx4444.index",
            "data/en/ce/encekey1",
            "data/in/st/instekey",
            "data/dl/ek/dlekey",
            "data/si/ze/sizekey",
            "data/a3/a3/a3a3a3a3a3a3a3a3a3a3a3a3a3a3a3a3",
        ]
        .iter()
        .map(|s| format!("{h}/{s}"))
        .collect();
        want.sort();
        // Root has no ekey in the build config, so it is not warmed. Archived ekeys
        // (a1, a2) are not warmed as loose files; a3 is.
        assert_eq!(prefill.calls("warm").len(), 12);
        assert_eq!(prefill.calls("warm"), want);

        let games = b.list_games(&ctx).await.unwrap();
        assert_eq!(
            games
                .iter()
                .find(|g| g.id == "wow")
                .unwrap()
                .version
                .as_deref(),
            Some("1.2.3")
        );

        // Unchanged build with everything cached: verified, not updated.
        prefill.cached.store(true, Ordering::SeqCst);
        let r2 = b.update_game(&ctx, "wow").await.unwrap();
        assert!(!r2.updated);
        assert_eq!(r2.error, None);
        assert_eq!(r2.current_version.as_deref(), Some("1.2.3"));
        assert_eq!(r2.previous_version.as_deref(), Some("1.2.3"));
        assert_eq!(
            (r2.files, r2.bytes_downloaded, r2.bytes_already_cached),
            (12, 0, 132)
        );
        assert_eq!(prefill.calls("warm").len(), 24);

        // Same build but content had to be fetched again (purged/evicted): repaired.
        prefill.cached.store(false, Ordering::SeqCst);
        let r3 = b.update_game(&ctx, "wow").await.unwrap();
        assert!(r3.updated);
        assert_eq!(r3.bytes_downloaded, 120);
        assert_eq!(r3.previous_version.as_deref(), Some("1.2.3"));
    }

    #[tokio::test]
    async fn object_failure_is_reported_and_retried() {
        let server = patch_server(BUILD).await;
        let b = Blizzard::with_patch_base(&server.uri());
        let prefill = Arc::new(MockPrefill {
            fail: Some("fidx4444"),
            ..Default::default()
        });
        let ctx = ctx(prefill.clone()).await;
        let r = b.update_game(&ctx, "wow").await.unwrap();
        assert!(r.error.unwrap().contains("fidx4444"));
        assert_eq!(r.files, 11);
        // State not saved, so the next run is not a no-op.
        assert!(ctx.state.load().await.unwrap().get("wow").is_none());
    }

    #[tokio::test]
    async fn loose_file_failure_is_tolerated() {
        let server = patch_server(BUILD).await;
        let b = Blizzard::with_patch_base(&server.uri());
        let prefill = Arc::new(MockPrefill {
            fail_loose: true,
            ..Default::default()
        });
        let ctx = ctx(prefill.clone()).await;
        let r = b.update_game(&ctx, "wow").await.unwrap();
        assert_eq!(r.error, None);
        assert_eq!(r.files, 11);
        assert!(ctx.state.load().await.unwrap().get("wow").is_some());
    }

    #[tokio::test]
    async fn patch_server_error_reported() {
        let server = MockServer::start().await;
        let b = Blizzard::with_patch_base(&server.uri());
        let ctx = ctx(Arc::new(MockPrefill::default())).await;
        let r = b.update_game(&ctx, "nope").await.unwrap();
        assert!(!r.updated);
        assert!(r.error.unwrap().contains("404"));
    }

    #[tokio::test]
    async fn auth_and_catalog() {
        let b = Blizzard::new();
        let ctx = ctx(Arc::new(MockPrefill::default())).await;
        assert_eq!(b.login_methods(), &[LoginMethod::None]);
        let s = b.auth_status(&ctx).await.unwrap();
        assert!(!s.logged_in && !s.required);
        assert_eq!(
            b.login(&ctx, LoginRequest::Qr).await.unwrap(),
            LoginResponse::Failed {
                message: "not required".into()
            }
        );
        let games = b.list_games(&ctx).await.unwrap();
        assert!(
            games
                .iter()
                .any(|g| g.id == "pro" && g.name == "Overwatch 2")
        );
        assert!(games.iter().all(|g| g.version.is_none()));
    }
}
