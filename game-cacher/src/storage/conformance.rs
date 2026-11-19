//! Backend-agnostic Storage test suite, run against every implementation.
use std::collections::BTreeSet;

use bytes::Bytes;

use super::{DiskStorage, MemoryStorage, S3Storage, Storage};

async fn keys(s: &dyn Storage, prefix: &str) -> BTreeSet<String> {
    s.list(prefix)
        .await
        .unwrap()
        .into_iter()
        .map(|o| o.key)
        .collect()
}

fn set(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|s| s.to_string()).collect()
}

pub async fn run(s: &dyn Storage) {
    // put / get / exists
    assert_eq!(s.get("a/b/c").await.unwrap(), None);
    assert!(!s.exists("a/b/c").await.unwrap());
    s.put("a/b/c", Bytes::from_static(b"hello")).await.unwrap();
    assert_eq!(s.get("a/b/c").await.unwrap().unwrap(), "hello");
    assert!(s.exists("a/b/c").await.unwrap());

    // overwrite
    s.put("a/b/c", Bytes::from_static(b"world!")).await.unwrap();
    assert_eq!(s.get("a/b/c").await.unwrap().unwrap(), "world!");

    // delete + delete missing
    s.delete("a/b/c").await.unwrap();
    assert_eq!(s.get("a/b/c").await.unwrap(), None);
    s.delete("a/b/c").await.unwrap();
    s.delete("nope/never").await.unwrap();

    // list + delete_prefix with partial component
    for (k, v) in [
        ("cache/steam/1", "aa"),
        ("cache/steam/sub/2", "bbb"),
        ("cache/stash/3", "c"),
        ("cache/epic/4", "dddd"),
        ("state/x.json", "e"),
    ] {
        s.put(k, Bytes::from(v)).await.unwrap();
    }
    assert_eq!(
        keys(s, "").await,
        set(&[
            "cache/steam/1",
            "cache/steam/sub/2",
            "cache/stash/3",
            "cache/epic/4",
            "state/x.json"
        ])
    );
    assert_eq!(
        keys(s, "cache/").await,
        set(&[
            "cache/steam/1",
            "cache/steam/sub/2",
            "cache/stash/3",
            "cache/epic/4"
        ])
    );
    let listed = s.list("cache/steam/sub/").await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].size, 3);

    assert_eq!(s.delete_prefix("cache/st").await.unwrap(), (3, 6));
    assert_eq!(keys(s, "").await, set(&["cache/epic/4", "state/x.json"]));
    assert_eq!(s.delete_prefix("cache/st").await.unwrap(), (0, 0));
    assert_eq!(s.delete_prefix("cache/epic/").await.unwrap(), (1, 4));
    assert_eq!(keys(s, "").await, set(&["state/x.json"]));
    s.delete("state/x.json").await.unwrap();
    assert!(keys(s, "").await.is_empty());

    // invalid keys
    for bad in ["", "/abs", "a/../b", "a//b", "a/./b", "a\\b", "a/"] {
        assert!(s.get(bad).await.is_err(), "get {bad:?}");
        assert!(s.put(bad, Bytes::new()).await.is_err(), "put {bad:?}");
        assert!(s.delete(bad).await.is_err(), "delete {bad:?}");
        assert!(s.exists(bad).await.is_err(), "exists {bad:?}");
    }
}

#[tokio::test]
async fn memory() {
    run(&MemoryStorage::new()).await;
}

#[tokio::test]
async fn disk() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    let s = DiskStorage::new(&root).await.unwrap();
    run(&s).await;
    // Empty directories are pruned but the root and .tmp survive.
    assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1, "only .tmp");
    assert_eq!(std::fs::read_dir(root.join(".tmp")).unwrap().count(), 0);
}

#[tokio::test]
async fn disk_hides_tmp_and_rejects_it() {
    let dir = tempfile::tempdir().unwrap();
    let s = DiskStorage::new(dir.path()).await.unwrap();
    assert!(s.put(".tmp/x", Bytes::new()).await.is_err());
    std::fs::write(dir.path().join(".tmp/stray"), b"x").unwrap();
    assert!(s.list("").await.unwrap().is_empty());
    assert!(s.list("../").await.is_err());
}

#[tokio::test]
async fn s3() {
    let (Ok(endpoint), Ok(bucket)) = (
        std::env::var("GC_TEST_S3_ENDPOINT"),
        std::env::var("GC_TEST_S3_BUCKET"),
    ) else {
        eprintln!("skipping S3 conformance: GC_TEST_S3_ENDPOINT / GC_TEST_S3_BUCKET not set");
        return;
    };
    let prefix = format!("gc-test-{}", uuid::Uuid::new_v4());
    let s = S3Storage::new(&bucket, Some(&endpoint), "us-east-1", &prefix, true).unwrap();
    run(&s).await;
}
