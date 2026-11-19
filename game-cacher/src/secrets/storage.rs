//! ChaCha20-Poly1305 encrypted secrets at `secrets/<name>.enc`.
use anyhow::{Context, Result};
use async_trait::async_trait;
use bytes::Bytes;
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use rand::RngCore;
use sha2::{Digest, Sha256};

use super::{SecretStore, validate_name};
use crate::storage::DynStorage;

const NONCE_LEN: usize = 12;

pub struct StorageSecrets {
    storage: DynStorage,
    cipher: ChaCha20Poly1305,
}

impl StorageSecrets {
    /// The cipher key is the SHA-256 digest of the `GC_SECRETS_KEY` string, so
    /// any non-empty passphrase yields a valid 32-byte key.
    pub fn new(storage: DynStorage, key: &str) -> Result<Self> {
        if key.is_empty() {
            anyhow::bail!("GC_SECRETS_KEY must not be empty");
        }
        let digest = Sha256::digest(key.as_bytes());
        Ok(Self {
            storage,
            cipher: ChaCha20Poly1305::new(Key::from_slice(&digest)),
        })
    }

    fn key(name: &str) -> Result<String> {
        validate_name(name)?;
        Ok(format!("secrets/{name}.enc"))
    }
}

#[async_trait]
impl SecretStore for StorageSecrets {
    fn name(&self) -> &'static str {
        "storage"
    }

    async fn get(&self, name: &str) -> Result<Option<String>> {
        let Some(blob) = self.storage.get(&Self::key(name)?).await? else {
            return Ok(None);
        };
        if blob.len() < NONCE_LEN {
            anyhow::bail!("secret {name} is corrupt (too short)");
        }
        let (nonce, ct) = blob.split_at(NONCE_LEN);
        let plain = self
            .cipher
            .decrypt(Nonce::from_slice(nonce), ct)
            .map_err(|_| {
                anyhow::anyhow!(
                    "cannot decrypt secret {name}: GC_SECRETS_KEY is wrong or the data is corrupt"
                )
            })?;
        Ok(Some(
            String::from_utf8(plain).context("decrypted secret is not UTF-8")?,
        ))
    }

    async fn put(&self, name: &str, value: &str) -> Result<()> {
        let key = Self::key(name)?;
        let mut nonce = [0u8; NONCE_LEN];
        rand::rng().fill_bytes(&mut nonce);
        let ct = self
            .cipher
            .encrypt(Nonce::from_slice(&nonce), value.as_bytes())
            .map_err(|_| anyhow::anyhow!("encrypting secret {name} failed"))?;
        let mut blob = Vec::with_capacity(NONCE_LEN + ct.len());
        blob.extend_from_slice(&nonce);
        blob.extend_from_slice(&ct);
        self.storage.put(&key, Bytes::from(blob)).await
    }

    async fn delete(&self, name: &str) -> Result<()> {
        self.storage.delete(&Self::key(name)?).await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::storage::{MemoryStorage, Storage};

    fn store(key: &str, mem: Arc<MemoryStorage>) -> StorageSecrets {
        StorageSecrets::new(mem, key).unwrap()
    }

    #[tokio::test]
    async fn roundtrip_delete_and_missing() {
        let mem = Arc::new(MemoryStorage::new());
        let s = store("k1", mem.clone());
        assert_eq!(s.get("steam.token").await.unwrap(), None);
        s.put("steam.token", "héllo").await.unwrap();
        assert_eq!(s.get("steam.token").await.unwrap().unwrap(), "héllo");
        let raw = mem.get("secrets/steam.token.enc").await.unwrap().unwrap();
        assert!(
            !raw.windows(5)
                .any(|w| w == "héllo".as_bytes()[..5].as_ref())
        );
        s.delete("steam.token").await.unwrap();
        assert_eq!(s.get("steam.token").await.unwrap(), None);
        s.delete("steam.token").await.unwrap();
    }

    #[tokio::test]
    async fn random_nonce_per_write() {
        let mem = Arc::new(MemoryStorage::new());
        let s = store("k1", mem.clone());
        s.put("a", "same").await.unwrap();
        let first = mem.get("secrets/a.enc").await.unwrap().unwrap();
        s.put("a", "same").await.unwrap();
        let second = mem.get("secrets/a.enc").await.unwrap().unwrap();
        assert_ne!(first, second);
    }

    #[tokio::test]
    async fn wrong_key_and_corruption_error() {
        let mem = Arc::new(MemoryStorage::new());
        store("k1", mem.clone()).put("a", "v").await.unwrap();
        let err = store("k2", mem.clone()).get("a").await.unwrap_err();
        assert!(err.to_string().contains("wrong"), "{err}");

        mem.put("secrets/short.enc", Bytes::from_static(b"abc"))
            .await
            .unwrap();
        assert!(store("k1", mem.clone()).get("short").await.is_err());
    }

    #[tokio::test]
    async fn rejects_empty_key_and_bad_names() {
        let mem = Arc::new(MemoryStorage::new());
        assert!(StorageSecrets::new(mem.clone(), "").is_err());
        let s = store("k", mem);
        assert!(s.put("../x", "v").await.is_err());
        assert!(s.get("Bad").await.is_err());
    }
}
