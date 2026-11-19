//! TODO(T2): ChaCha20-Poly1305 encrypted secrets at `secrets/<name>.enc`.
use anyhow::Result;
use async_trait::async_trait;

use super::SecretStore;
use crate::storage::DynStorage;

pub struct StorageSecrets {
    #[allow(dead_code)]
    storage: DynStorage,
}

impl StorageSecrets {
    pub fn new(storage: DynStorage, _key: &str) -> Result<Self> {
        Ok(Self { storage })
    }
}

#[async_trait]
impl SecretStore for StorageSecrets {
    fn name(&self) -> &'static str {
        "storage"
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
