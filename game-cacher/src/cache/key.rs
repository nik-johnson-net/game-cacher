//! Storage key layout for cached objects.

use sha2::{Digest, Sha256};

/// Identifies one cached object: `<identifier><path>` with the query string removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectKey {
    pub identifier: String,
    pub path: String,
    hash: String,
}

impl ObjectKey {
    pub fn new(identifier: &str, path_and_query: &str) -> Self {
        let path = path_and_query
            .split('?')
            .next()
            .unwrap_or_default()
            .to_string();
        let mut h = Sha256::new();
        h.update(identifier.as_bytes());
        h.update(path.as_bytes());
        Self {
            identifier: identifier.to_string(),
            path,
            hash: hex::encode(h.finalize()),
        }
    }

    /// `cache/<identifier>/<h0h1>/<h2h3>/<hash>` — the object's directory prefix (no trailing slash).
    pub fn prefix(&self) -> String {
        format!(
            "cache/{}/{}/{}/{}",
            self.identifier,
            &self.hash[0..2],
            &self.hash[2..4],
            self.hash
        )
    }

    pub fn meta_key(&self) -> String {
        format!("{}/meta.json", self.prefix())
    }

    pub fn slice_key(&self, index: u64) -> String {
        format!("{}/{index}", self.prefix())
    }
}

/// Parses an object prefix back out of any key below it
/// (`cache/<id>/<aa>/<bb>/<hash>/...` → (`<id>`, prefix)).
pub fn object_prefix_of(key: &str) -> Option<(&str, &str)> {
    let mut parts = key.splitn(6, '/');
    let (root, id, a, b, hash) = (
        parts.next()?,
        parts.next()?,
        parts.next()?,
        parts.next()?,
        parts.next()?,
    );
    if root != "cache" || a.len() != 2 || b.len() != 2 || hash.len() != 64 {
        return None;
    }
    let len = root.len() + id.len() + a.len() + b.len() + hash.len() + 4;
    Some((id, &key[..len]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout() {
        let k = ObjectKey::new("steam", "/depot/1/chunk/abc?token=1");
        assert_eq!(k, ObjectKey::new("steam", "/depot/1/chunk/abc?token=2"));
        assert_ne!(k, ObjectKey::new("epic", "/depot/1/chunk/abc"));
        let p = k.prefix();
        assert!(p.starts_with("cache/steam/"));
        assert_eq!(
            object_prefix_of(&k.slice_key(3)),
            Some(("steam", p.as_str()))
        );
        assert_eq!(object_prefix_of(&k.meta_key()), Some(("steam", p.as_str())));
        assert_eq!(object_prefix_of("settings.json"), None);
        assert_eq!(object_prefix_of("cache/steam/ab"), None);
    }
}
