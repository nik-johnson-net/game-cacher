//! Secrets abstraction: named string secrets (platform refresh tokens etc.).

mod storage;
mod vault;

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;

pub use storage::StorageSecrets;
pub use vault::VaultSecrets;

use crate::config::{Config, EnvSecrets, SecretsKind};
use crate::storage::DynStorage;

#[async_trait]
pub trait SecretStore: Send + Sync + 'static {
    fn name(&self) -> &'static str;
    async fn get(&self, name: &str) -> Result<Option<String>>;
    async fn put(&self, name: &str, value: &str) -> Result<()>;
    async fn delete(&self, name: &str) -> Result<()>;
}

pub type DynSecrets = Arc<dyn SecretStore>;

/// Secret names are `[a-z0-9_.-]+` (e.g. `steam.refresh_token`).
pub fn validate_name(name: &str) -> Result<()> {
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "_.-".contains(c))
    {
        anyhow::bail!("invalid secret name: {name:?}");
    }
    Ok(())
}

pub async fn from_config(
    config: &Config,
    env: &EnvSecrets,
    storage: DynStorage,
) -> Result<DynSecrets> {
    Ok(match config.secrets {
        SecretsKind::Storage => Arc::new(StorageSecrets::new(
            storage,
            env.secrets_key.as_deref().unwrap_or_default(),
        )?),
        SecretsKind::Vault => Arc::new(VaultSecrets::from_config(config, env).await?),
    })
}
