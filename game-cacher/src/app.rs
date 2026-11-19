//! Wiring: builds shared state from config and binds all listeners.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

use crate::api;
use crate::api::auth::Auth;
use crate::cache::engine::EngineDeps;
use crate::cache::{CacheEngine, LruIndex};
use crate::config::{Config, EnvSecrets};
use crate::dns::DnsServer;
use crate::metrics::Metrics;
use crate::platforms::{self, DynPlatform, Platforms};
use crate::secrets::{self, DynSecrets};
use crate::settings::SettingsStore;
use crate::sni;
use crate::state::AppState;
use crate::storage::{self, DynStorage};
use crate::updates::UpdateManager;
use crate::upstream::{DnsResolver, HostResolver, LoopGuard, Upstream};

/// Test seams: anything left `None` is built from config.
#[derive(Default)]
pub struct Overrides {
    pub resolver: Option<Arc<dyn HostResolver>>,
    pub storage: Option<DynStorage>,
    pub secrets: Option<DynSecrets>,
    pub platforms: Option<Vec<DynPlatform>>,
    /// Upstream port for SNI passthrough (443 in production).
    pub sni_upstream_port: Option<u16>,
    /// Scheduler tick period (60s in production).
    pub scheduler_period: Option<Duration>,
}

pub struct RunningApp {
    pub state: Arc<AppState>,
    pub http_addr: SocketAddr,
    pub admin_addr: SocketAddr,
    pub https_addr: Option<SocketAddr>,
    pub dns_addr: Option<SocketAddr>,
    tasks: Vec<JoinHandle<()>>,
}

impl RunningApp {
    /// Stops background tasks and persists the LRU index.
    pub async fn shutdown(self) {
        for t in &self.tasks {
            t.abort();
        }
        if let Err(e) = self.state.index.snapshot().await {
            log::warn!("saving LRU index on shutdown: {e:#}");
        }
    }
}

pub async fn start(config: Config, env: EnvSecrets, o: Overrides) -> Result<RunningApp> {
    let config = Arc::new(config);
    let metrics = Metrics::new();
    let storage = match o.storage {
        Some(s) => s,
        None => storage::from_config(&config)
            .await
            .context("initialising storage")?,
    };
    let secrets = match o.secrets {
        Some(s) => s,
        None => secrets::from_config(&config, &env, storage.clone())
            .await
            .context("initialising secrets")?,
    };
    let settings = Arc::new(SettingsStore::load(storage.clone()).await?);
    let platforms = Arc::new(Platforms::new(o.platforms.unwrap_or_else(platforms::all)));

    let base_resolver: Arc<dyn HostResolver> = match o.resolver {
        Some(r) => r,
        None => {
            let servers: Vec<SocketAddr> = config.upstream_dns.iter().map(|s| s.0).collect();
            Arc::new(DnsResolver::new(&servers)?)
        }
    };
    let resolver = Arc::new(LoopGuard::new(base_resolver, config.cache_ip.clone()));
    let upstream = Upstream::new(resolver, config.upstream_http_port, config.instance_id())?;

    let index =
        LruIndex::load_or_rebuild(storage.clone(), config.cache_max_size, metrics.clone()).await?;
    let engine = CacheEngine::new(EngineDeps {
        storage: storage.clone(),
        platforms: platforms.clone(),
        settings: settings.clone(),
        upstream: upstream.clone(),
        index: index.clone(),
        metrics: metrics.clone(),
        slice_size: config.slice_size,
    });
    let updates = UpdateManager::new(
        platforms.clone(),
        settings.clone(),
        secrets.clone(),
        upstream.client().clone(),
        engine.clone(),
        metrics.clone(),
    );
    let auth = Auth::new(if config.disable_auth {
        None
    } else {
        env.admin_password.clone()
    });
    if !auth.enabled() {
        log::warn!("admin authentication is DISABLED");
    }

    let state = Arc::new(AppState {
        config: config.clone(),
        storage,
        secrets,
        settings: settings.clone(),
        platforms: platforms.clone(),
        metrics: metrics.clone(),
        index: index.clone(),
        engine: engine.clone(),
        upstream: upstream.clone(),
        updates: updates.clone(),
        auth,
        started: Instant::now(),
    });

    let mut tasks = vec![
        index.spawn_background(),
        updates.spawn_scheduler(o.scheduler_period.unwrap_or(Duration::from_secs(60))),
    ];

    let (http_addr, t) = serve(config.http_listen, engine.router(), "cache HTTP").await?;
    tasks.push(t);
    let (admin_addr, t) = serve(config.admin_listen, api::router(state.clone()), "admin").await?;
    tasks.push(t);

    let https_addr = if config.disable_sni {
        None
    } else {
        let l = TcpListener::bind(config.https_listen)
            .await
            .with_context(|| format!("binding SNI listener {}", config.https_listen))?;
        let addr = l.local_addr()?;
        tasks.push(sni::spawn(
            l,
            upstream.clone(),
            o.sni_upstream_port.unwrap_or(443),
            platforms.clone(),
            metrics.clone(),
        ));
        log::info!("SNI passthrough listening on {addr}");
        Some(addr)
    };

    let dns_addr = if config.disable_dns {
        None
    } else {
        let forwarders = config.dns_forward.iter().map(|f| f.0).collect();
        let dns = DnsServer::new(
            config.cache_ip.clone(),
            forwarders,
            platforms,
            settings,
            metrics,
        );
        let addr = dns.bind(config.dns_listen).await.context("binding DNS")?;
        log::info!("DNS listening on {addr} (udp+tcp)");
        Some(addr)
    };

    Ok(RunningApp {
        state,
        http_addr,
        admin_addr,
        https_addr,
        dns_addr,
        tasks,
    })
}

async fn serve(
    addr: SocketAddr,
    router: axum::Router,
    what: &str,
) -> Result<(SocketAddr, JoinHandle<()>)> {
    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding {what} listener {addr}"))?;
    let bound = listener.local_addr()?;
    log::info!("{what} listening on {bound}");
    let what = what.to_string();
    let task = tokio::spawn(async move {
        if let Err(e) = axum::serve(
            listener,
            router.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        {
            log::error!("{what} server failed: {e}");
        }
    });
    Ok((bound, task))
}

pub async fn run(config: Config, env: EnvSecrets) -> Result<()> {
    let app = start(config, env, Overrides::default()).await?;
    shutdown_signal().await;
    log::info!("shutting down");
    app.shutdown().await;
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        if let Ok(mut s) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            s.recv().await;
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {},
        _ = term => {},
    }
}
