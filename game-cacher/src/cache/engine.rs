//! Cache engine + proxy handler. TODO(T6).
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use bytes::Bytes;

use super::LruIndex;
use crate::metrics::Metrics;
use crate::platforms::{Platforms, Prefill, WarmResult};
use crate::settings::SettingsStore;
use crate::storage::DynStorage;
use crate::upstream::Upstream;

pub struct CacheEngine {}

pub struct EngineDeps {
    pub storage: DynStorage,
    pub platforms: Arc<Platforms>,
    pub settings: Arc<SettingsStore>,
    pub upstream: Upstream,
    pub index: Arc<LruIndex>,
    pub metrics: Arc<Metrics>,
    pub slice_size: u64,
}

impl CacheEngine {
    pub fn new(_deps: EngineDeps) -> Arc<Self> {
        todo!("T6")
    }

    /// Router for the cache HTTP listener: every request is proxied by Host header.
    pub fn router(self: &Arc<Self>) -> axum::Router {
        todo!("T6")
    }
}

#[async_trait]
impl Prefill for CacheEngine {
    async fn warm(
        &self,
        _identifier: &str,
        _host: &str,
        _path_and_query: &str,
    ) -> Result<WarmResult> {
        todo!("T6")
    }
    async fn get(&self, _identifier: &str, _host: &str, _path_and_query: &str) -> Result<Bytes> {
        todo!("T6")
    }
    async fn get_range(
        &self,
        _identifier: &str,
        _host: &str,
        _path_and_query: &str,
        _start: u64,
        _end: u64,
    ) -> Result<Bytes> {
        todo!("T6")
    }
}
