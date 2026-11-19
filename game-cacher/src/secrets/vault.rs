//! TODO(T2): HashiCorp Vault KV v2 over HTTP (token or AppRole auth).
use anyhow::Result;
use async_trait::async_trait;

use super::SecretStore;
use crate::config::{Config, EnvSecrets};

pub struct VaultSecrets {}

impl VaultSecrets {
    pub async fn from_config(_config: &Config, _env: &EnvSecrets) -> Result<Self> {
        todo!("T2")
    }
}

#[async_trait]
impl SecretStore for VaultSecrets {
    fn name(&self) -> &'static str {
        "vault"
    }
    async fn get(&self, _name: &str) -> Result<Option<String>> {
        todo!("T2")
    }
    async fn put(&self, _name: &str, _value: &str) -> Result<()> {
        todo!("T2")
    }
    async fn delete(&self, _name: &str) -> Result<()> {
        todo!("T2")
    }
}
