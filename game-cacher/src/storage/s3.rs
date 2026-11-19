//! TODO(T1): implement with object_store (AmazonS3Builder).
use anyhow::Result;
use async_trait::async_trait;
use bytes::Bytes;

use super::{ObjectInfo, Storage};
use crate::config::Config;

pub struct S3Storage {}

impl S3Storage {
    pub fn from_config(_config: &Config) -> Result<Self> {
        todo!("T1")
    }
}

#[async_trait]
impl Storage for S3Storage {
    fn name(&self) -> &'static str {
        "s3"
    }
    async fn get(&self, _key: &str) -> Result<Option<Bytes>> {
        todo!("T1")
    }
    async fn put(&self, _key: &str, _data: Bytes) -> Result<()> {
        todo!("T1")
    }
    async fn delete(&self, _key: &str) -> Result<()> {
        todo!("T1")
    }
    async fn delete_prefix(&self, _prefix: &str) -> Result<(u64, u64)> {
        todo!("T1")
    }
    async fn list(&self, _prefix: &str) -> Result<Vec<ObjectInfo>> {
        todo!("T1")
    }
    async fn exists(&self, _key: &str) -> Result<bool> {
        todo!("T1")
    }
}
