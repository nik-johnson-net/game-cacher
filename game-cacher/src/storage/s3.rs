use anyhow::{Context, Result};
use async_trait::async_trait;
use bytes::Bytes;
use futures::{StreamExt, TryStreamExt};
use object_store::aws::AmazonS3Builder;
use object_store::path::Path as ObjPath;
use object_store::{ObjectStore, ObjectStoreExt, PutPayload};

use super::{ObjectInfo, Storage, validate_key};
use crate::config::Config;

/// S3-compatible storage; every key lives under an optional `prefix/`.
pub struct S3Storage {
    store: Box<dyn ObjectStore>,
    /// Empty, or ends with `/`.
    prefix: String,
}

impl S3Storage {
    /// Credentials come from `AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY`.
    pub fn new(
        bucket: &str,
        endpoint: Option<&str>,
        region: &str,
        prefix: &str,
        path_style: bool,
    ) -> Result<Self> {
        let mut b = AmazonS3Builder::from_env()
            .with_bucket_name(bucket)
            .with_region(region)
            .with_virtual_hosted_style_request(!path_style);
        if let Some(ep) = endpoint {
            b = b
                .with_endpoint(ep)
                .with_allow_http(ep.starts_with("http://"));
        }
        let store = b.build().context("building S3 client")?;
        let p = prefix.trim_matches('/');
        Ok(Self {
            store: Box::new(store),
            prefix: if p.is_empty() {
                String::new()
            } else {
                format!("{p}/")
            },
        })
    }

    pub fn from_config(config: &Config) -> Result<Self> {
        let bucket = config
            .s3_bucket
            .as_deref()
            .context("--s3-bucket is required with --storage s3")?;
        Self::new(
            bucket,
            config.s3_endpoint.as_deref(),
            &config.s3_region,
            &config.s3_prefix,
            config.s3_path_style,
        )
    }

    fn path(&self, key: &str) -> Result<ObjPath> {
        validate_key(key)?;
        Ok(ObjPath::parse(format!("{}{key}", self.prefix))?)
    }

    /// Lists objects whose full key starts with `self.prefix + prefix`.
    async fn list_raw(&self, prefix: &str) -> Result<Vec<(ObjPath, u64)>> {
        if prefix.starts_with('/') || prefix.split('/').any(|c| c == "." || c == "..") {
            anyhow::bail!("invalid storage prefix: {prefix:?}");
        }
        let full = format!("{}{prefix}", self.prefix);
        let dir = full.rfind('/').map(|i| &full[..i]).unwrap_or("");
        let dir_path = if dir.is_empty() {
            None
        } else {
            Some(ObjPath::parse(dir)?)
        };
        let mut stream = self.store.list(dir_path.as_ref());
        let mut out = Vec::new();
        while let Some(meta) = stream.try_next().await.context("listing S3 objects")? {
            if meta.location.as_ref().starts_with(&full) {
                out.push((meta.location, meta.size));
            }
        }
        Ok(out)
    }

    fn strip(&self, p: &ObjPath) -> String {
        p.as_ref()
            .strip_prefix(&self.prefix)
            .unwrap_or(p.as_ref())
            .to_string()
    }
}

#[async_trait]
impl Storage for S3Storage {
    fn name(&self) -> &'static str {
        "s3"
    }

    async fn get(&self, key: &str) -> Result<Option<Bytes>> {
        let path = self.path(key)?;
        match self.store.get(&path).await {
            Ok(r) => Ok(Some(
                r.bytes().await.with_context(|| format!("reading {key}"))?,
            )),
            Err(object_store::Error::NotFound { .. }) => Ok(None),
            Err(e) => Err(e).with_context(|| format!("S3 get {key}")),
        }
    }

    async fn put(&self, key: &str, data: Bytes) -> Result<()> {
        let path = self.path(key)?;
        self.store
            .put(&path, PutPayload::from_bytes(data))
            .await
            .with_context(|| format!("S3 put {key}"))?;
        Ok(())
    }

    async fn delete(&self, key: &str) -> Result<()> {
        let path = self.path(key)?;
        match self.store.delete(&path).await {
            Ok(()) | Err(object_store::Error::NotFound { .. }) => Ok(()),
            Err(e) => Err(e).with_context(|| format!("S3 delete {key}")),
        }
    }

    async fn delete_prefix(&self, prefix: &str) -> Result<(u64, u64)> {
        let objs = self.list_raw(prefix).await?;
        let bytes: u64 = objs.iter().map(|(_, s)| s).sum();
        let paths: Vec<_> = objs.into_iter().map(|(p, _)| Ok(p)).collect();
        let mut count = 0;
        let mut results = self
            .store
            .delete_stream(futures::stream::iter(paths).boxed());
        while let Some(r) = results.next().await {
            match r {
                Ok(_) | Err(object_store::Error::NotFound { .. }) => count += 1,
                Err(e) => return Err(e).context("S3 bulk delete"),
            }
        }
        Ok((count, bytes))
    }

    async fn list(&self, prefix: &str) -> Result<Vec<ObjectInfo>> {
        Ok(self
            .list_raw(prefix)
            .await?
            .into_iter()
            .map(|(p, size)| ObjectInfo {
                key: self.strip(&p),
                size,
            })
            .collect())
    }

    async fn exists(&self, key: &str) -> Result<bool> {
        let path = self.path(key)?;
        match self.store.head(&path).await {
            Ok(_) => Ok(true),
            Err(object_store::Error::NotFound { .. }) => Ok(false),
            Err(e) => Err(e).with_context(|| format!("S3 head {key}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_normalised() {
        for (input, want) in [("", ""), ("/", ""), ("a", "a/"), ("/a/b/", "a/b/")] {
            let s =
                S3Storage::new("b", Some("http://localhost:1"), "us-east-1", input, true).unwrap();
            assert_eq!(s.prefix, want, "{input}");
        }
    }

    #[test]
    fn key_mapping() {
        let s = S3Storage::new("b", Some("http://localhost:1"), "r", "pre", true).unwrap();
        let p = s.path("cache/x").unwrap();
        assert_eq!(p.as_ref(), "pre/cache/x");
        assert_eq!(s.strip(&p), "cache/x");
        assert!(s.path("../x").is_err());
    }
}
