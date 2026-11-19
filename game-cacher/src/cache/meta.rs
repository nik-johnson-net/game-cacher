//! Per-object metadata (`meta.json`) and slice arithmetic.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Meta {
    pub identifier: String,
    pub host: String,
    pub path: String,
    pub total_size: u64,
    pub slice_size: u64,
    pub content_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub etag: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_modified: Option<String>,
    pub created: String,
}

/// Largest object we cache (1 TiB). Bigger claims from an upstream are refused so a
/// hostile `Content-Range` total can't drive slice arithmetic or index memory.
pub const MAX_OBJECT_SIZE: u64 = 1 << 40;

impl Meta {
    /// Rejects metadata that cannot describe a sliced object.
    pub fn is_valid(&self) -> bool {
        self.total_size > 0 && self.total_size <= MAX_OBJECT_SIZE && self.slice_size > 0
    }

    pub fn slice_count(&self) -> u64 {
        self.total_size.div_ceil(self.slice_size)
    }

    /// Inclusive byte bounds of slice `index` (the last slice may be short).
    pub fn slice_bounds(&self, index: u64) -> (u64, u64) {
        let start = index.saturating_mul(self.slice_size);
        let end = start
            .saturating_add(self.slice_size)
            .min(self.total_size)
            .saturating_sub(1);
        (start, end)
    }

    pub fn slice_len(&self, index: u64) -> u64 {
        let (start, end) = self.slice_bounds(index);
        end - start + 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(total: u64, slice: u64) -> Meta {
        Meta {
            identifier: "epic".into(),
            host: "h".into(),
            path: "/p".into(),
            total_size: total,
            slice_size: slice,
            content_type: "application/octet-stream".into(),
            etag: None,
            last_modified: None,
            created: "2026-01-01T00:00:00Z".into(),
        }
    }

    #[test]
    fn slices() {
        let m = meta(250, 100);
        assert_eq!(m.slice_count(), 3);
        assert_eq!(m.slice_bounds(0), (0, 99));
        assert_eq!(m.slice_bounds(2), (200, 249));
        assert_eq!(m.slice_len(2), 50);
        assert_eq!(meta(200, 100).slice_count(), 2);
        assert!(!meta(0, 100).is_valid());
        assert!(!meta(10, 0).is_valid());
        assert!(!meta(MAX_OBJECT_SIZE + 1, 100).is_valid());
        let huge = meta(u64::MAX, 1 << 20);
        let (a, b) = huge.slice_bounds(huge.slice_count() - 1);
        assert!(a <= b);
    }

    #[test]
    fn json_shape() {
        let v = serde_json::to_value(meta(1, 1)).unwrap();
        assert!(v.get("etag").is_none());
        let back: Meta = serde_json::from_value(v).unwrap();
        assert_eq!(back, meta(1, 1));
    }
}
