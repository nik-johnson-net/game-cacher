//! HTTP `Range` / `Content-Range` parsing (single byte ranges only).

/// How a client `Range` header applies to an object of known size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeRequest {
    /// No usable Range header (absent, multi-range or malformed): serve the whole object.
    Full,
    /// Inclusive byte range inside the object.
    Partial(u64, u64),
    /// Syntactically valid but outside the object: 416.
    Unsatisfiable,
}

/// Resolves a `Range` header value (`bytes=a-b`, `bytes=a-`, `bytes=-n`) against `total`.
pub fn resolve_range(header: Option<&str>, total: u64) -> RangeRequest {
    let Some(spec) = header.and_then(|h| h.trim().strip_prefix("bytes=")) else {
        return RangeRequest::Full;
    };
    if spec.contains(',') {
        return RangeRequest::Full;
    }
    let Some((start, end)) = spec.split_once('-') else {
        return RangeRequest::Full;
    };
    let (start, end) = (start.trim(), end.trim());
    if start.is_empty() {
        let Ok(n) = end.parse::<u64>() else {
            return RangeRequest::Full;
        };
        if n == 0 || total == 0 {
            return RangeRequest::Unsatisfiable;
        }
        return RangeRequest::Partial(total - n.min(total), total - 1);
    }
    let Ok(start) = start.parse::<u64>() else {
        return RangeRequest::Full;
    };
    let end = if end.is_empty() {
        u64::MAX
    } else {
        match end.parse::<u64>() {
            Ok(e) if e >= start => e,
            _ => return RangeRequest::Full,
        }
    };
    if start >= total {
        return RangeRequest::Unsatisfiable;
    }
    RangeRequest::Partial(start, end.min(total - 1))
}

/// A parsed `Content-Range: bytes start-end/total` (inclusive end).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContentRange {
    pub start: u64,
    pub end: u64,
    pub total: u64,
}

/// Parses `bytes a-b/total`; returns None for unknown totals (`*`) or malformed values.
pub fn parse_content_range(value: &str) -> Option<ContentRange> {
    let rest = value.trim().strip_prefix("bytes")?.trim_start();
    let (range, total) = rest.split_once('/')?;
    let (start, end) = range.split_once('-')?;
    let cr = ContentRange {
        start: start.trim().parse().ok()?,
        end: end.trim().parse().ok()?,
        total: total.trim().parse().ok()?,
    };
    (cr.start <= cr.end && cr.end < cr.total).then_some(cr)
}

#[cfg(test)]
mod tests {
    use super::*;
    use RangeRequest::*;

    #[test]
    fn ranges() {
        assert_eq!(resolve_range(None, 100), Full);
        assert_eq!(resolve_range(Some("bytes=0-9"), 100), Partial(0, 9));
        assert_eq!(resolve_range(Some("bytes=90-200"), 100), Partial(90, 99));
        assert_eq!(resolve_range(Some("bytes=50-"), 100), Partial(50, 99));
        assert_eq!(resolve_range(Some("bytes=-10"), 100), Partial(90, 99));
        assert_eq!(resolve_range(Some("bytes=-500"), 100), Partial(0, 99));
        assert_eq!(resolve_range(Some(" bytes= 1 - 2 "), 100), Partial(1, 2));
    }

    #[test]
    fn ignored_and_unsatisfiable() {
        for h in [
            "bytes=0-1,5-6",
            "items=0-1",
            "bytes=abc",
            "bytes=5-1",
            "bytes=x-",
            "bytes=-y",
            "bytes=1-y",
        ] {
            assert_eq!(resolve_range(Some(h), 100), Full, "{h}");
        }
        assert_eq!(resolve_range(Some("bytes=100-"), 100), Unsatisfiable);
        assert_eq!(resolve_range(Some("bytes=200-300"), 100), Unsatisfiable);
        assert_eq!(resolve_range(Some("bytes=-0"), 100), Unsatisfiable);
        assert_eq!(resolve_range(Some("bytes=0-"), 0), Unsatisfiable);
    }

    #[test]
    fn content_range() {
        assert_eq!(
            parse_content_range("bytes 0-99/1000"),
            Some(ContentRange {
                start: 0,
                end: 99,
                total: 1000
            })
        );
        assert_eq!(parse_content_range("bytes 0-99/*"), None);
        assert_eq!(parse_content_range("bytes */1000"), None);
        assert_eq!(parse_content_range("bytes 10-5/1000"), None);
        assert_eq!(parse_content_range("bytes 0-1000/1000"), None);
        assert_eq!(parse_content_range("garbage"), None);
    }
}
