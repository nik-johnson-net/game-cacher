//! Blizzard managed updates: poll the TACT patch server, then warm the build's CDN objects.

use std::collections::{HashMap, HashSet};

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use futures::stream::{self, StreamExt};
use serde_json::{Value, json};

use super::formats;
use super::tact::{self, cdn_path, parse_bpsv, parse_config, values};
use super::{Blizzard, domains};
use crate::platforms::plan::{GamePlan, Outcome};
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
    build: &HashMap<String, String>,
    cdn: &HashMap<String, String>,
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
    let mut seen = HashSet::new();
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

    /// `(BuildConfig, CDNConfig, VersionsName)` currently published for `REGION`.
    async fn current_version(
        &self,
        ctx: &PlatformCtx,
        product: &str,
    ) -> Result<(String, String, String)> {
        let versions = self.fetch_table(ctx, product, "versions").await?;
        let Some(v) = versions.region(REGION) else {
            bail!("no versions listed for {product}");
        };
        let field = |k: &str| v.get(k).cloned().unwrap_or_default();
        let (build, cdn) = (field("BuildConfig"), field("CDNConfig"));
        if build.is_empty() || cdn.is_empty() {
            bail!("versions for {product} lack BuildConfig/CDNConfig");
        }
        Ok((build, cdn, field("VersionsName")))
    }

    /// `(host, path)` of the CDN to fetch `product` from.
    async fn cdn(&self, ctx: &PlatformCtx, product: &str) -> Result<(String, String)> {
        let cdns = self.fetch_table(ctx, product, "cdns").await?;
        let Some(c) = cdns.region(REGION) else {
            bail!("no cdns listed for {product}");
        };
        let path = c.get("Path").cloned().unwrap_or_default();
        if path.is_empty() {
            bail!("cdns for {product} lack Path");
        }
        Ok((pick_host(c.get("Hosts").map_or("", String::as_str)), path))
    }

    /// Plans the build's full object set: when nothing changed these are cache hits,
    /// and anything evicted or purged is fetched again.
    async fn plan(&self, ctx: &PlatformCtx, product: &str) -> Result<GamePlan> {
        let (build_hash, cdn_hash, version) = self.current_version(ctx, product).await?;
        let mut plan = GamePlan::new(IDENTIFIER, product, &product_name(product));
        let state = ctx.state.load().await?;
        let prev = state.get(product).cloned().unwrap_or(Value::Null);
        plan.previous_version = prev["version"].as_str().map(str::to_string);
        plan.current_version = Some(version.clone());
        plan.version_changed =
            prev["build_config"] != build_hash.as_str() || prev["cdn_config"] != cdn_hash.as_str();

        let (host, path) = self.cdn(ctx, product).await?;
        let build_cfg = get_config(ctx, &host, &path, &build_hash).await?;
        let cdn_cfg = get_config(ctx, &host, &path, &cdn_hash).await?;
        let planned = plan_paths(&path, &build_cfg, &cdn_cfg);
        let contents = contents(ctx, &host, &path, &build_cfg, &cdn_cfg, &planned)
            .await
            .unwrap_or_else(|e| {
                plan.error = Some(format!("loose files: {e:#}"));
                Contents::default()
            });
        let (archives, loose) = (plan.group("", true), plan.group("loose files", false));
        for p in planned {
            let size = contents.archive_sizes.get(&p).copied();
            plan.add(p, size, archives);
        }
        for (p, size) in contents.loose {
            plan.add(p, size, loose);
        }
        plan.host = host;
        plan.concurrency = WARM_CONCURRENCY;
        plan.payload = Box::new(Payload {
            build_config: build_hash,
            cdn_config: cdn_hash,
            version,
        });
        Ok(plan)
    }
}

/// Archive sizes (from their indexes) and loose files: encoding-file ekeys not
/// in any archive index and not already planned, with their encoded sizes.
async fn contents(
    ctx: &PlatformCtx,
    host: &str,
    path: &str,
    build: &HashMap<String, String>,
    cdn: &HashMap<String, String>,
    planned: &[String],
) -> Result<Contents> {
    let (archive_sizes, archived) = archive_indexes(ctx, host, path, cdn).await?;
    let loose = loose_files(ctx, host, path, build, &archived, planned).await?;
    Ok(Contents {
        archive_sizes,
        loose,
    })
}

/// Parses every archive index: archive data path → size, and all archived ekeys.
async fn archive_indexes(
    ctx: &PlatformCtx,
    host: &str,
    path: &str,
    cdn: &HashMap<String, String>,
) -> Result<(HashMap<String, u64>, HashSet<Vec<u8>>)> {
    let archives: Vec<(String, String)> = values(cdn, "archives")
        .into_iter()
        .filter_map(|a| {
            Some((
                cdn_path(path, "data", a, "")?,
                cdn_path(path, "data", a, ".index")?,
            ))
        })
        .collect();
    let prefill = ctx.prefill.clone();
    let mut indices = stream::iter(archives)
        .map(|(data, idx)| {
            let prefill = prefill.clone();
            let host = host.to_string();
            async move {
                let body = prefill.get(IDENTIFIER, &host, &idx).await?;
                let parsed = formats::parse_index(&body).with_context(|| idx)?;
                Ok::<_, anyhow::Error>((data, parsed))
            }
        })
        .buffer_unordered(WARM_CONCURRENCY);
    let (mut sizes, mut archived) = (HashMap::new(), HashSet::new());
    while let Some(r) = indices.next().await {
        let (data, index) = r?;
        sizes.insert(data, index.archive_size);
        archived.extend(index.keys);
    }
    Ok((sizes, archived))
}

/// Data paths of encoding-file ekeys stored outside archives and not already
/// planned, with their encoded size when the encoding file lists it. Empty when
/// the build has no encoding file.
async fn loose_files(
    ctx: &PlatformCtx,
    host: &str,
    path: &str,
    build: &HashMap<String, String>,
    archived: &HashSet<Vec<u8>>,
    planned: &[String],
) -> Result<Vec<(String, Option<u64>)>> {
    let enc_key = values(build, "encoding").get(1).map(|k| k.to_string());
    let Some(enc) = enc_key.and_then(|k| cdn_path(path, "data", &k, "")) else {
        return Ok(Vec::new());
    };
    let body = ctx.prefill.get(IDENTIFIER, host, &enc).await?;
    let decoded = formats::decode_blte(&body).context(enc.clone())?;
    let ekeys = formats::encoding_ekeys(&decoded).context("parsing encoding file")?;
    let sizes = formats::encoding_sizes(&decoded).unwrap_or_else(|e| {
        log::warn!("blizzard: encoded sizes unavailable ({e:#}); loose files are probed");
        HashMap::new()
    });
    let planned: HashSet<&String> = planned.iter().collect();
    let mut seen = HashSet::new();
    Ok(ekeys
        .into_iter()
        .filter(|k| !archived.contains(k) && seen.insert(k.clone()))
        .filter_map(|k| {
            let p = cdn_path(path, "data", &hex::encode(&k), "")?;
            (!planned.contains(&p)).then(|| (p, sizes.get(&k).copied()))
        })
        .collect())
}

/// Fetches and parses a build or CDN config.
async fn get_config(
    ctx: &PlatformCtx,
    host: &str,
    path: &str,
    hash: &str,
) -> Result<HashMap<String, String>> {
    let p =
        cdn_path(path, "config", hash, "").with_context(|| format!("bad config hash {hash:?}"))?;
    let body = ctx
        .prefill
        .get(IDENTIFIER, host, &p)
        .await
        .with_context(|| p.clone())?;
    Ok(parse_config(&String::from_utf8_lossy(&body)))
}

/// State recorded once a build is fully warmed.
struct Payload {
    build_config: String,
    cdn_config: String,
    version: String,
}

#[derive(Default)]
struct Contents {
    /// Archive data path → size.
    archive_sizes: HashMap<String, u64>,
    /// Loose file data paths with their encoded size, when known.
    loose: Vec<(String, Option<u64>)>,
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

    async fn game_name(&self, _ctx: &PlatformCtx, game_id: &str) -> String {
        product_name(game_id)
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
        // Failed objects leave the state untouched so the next run retries.
        let Some(p) = plan.payload::<Payload>() else {
            return Ok(());
        };
        if !outcome.succeeded(plan) || !plan.version_changed {
            return Ok(());
        }
        let mut state = ctx.state.load().await?;
        state[&plan.game_id] = json!({
            "build_config": p.build_config,
            "cdn_config": p.cdn_config,
            "version": p.version,
            "updated_at": chrono::Utc::now().to_rfc3339(),
        });
        ctx.state.save(&state).await
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
