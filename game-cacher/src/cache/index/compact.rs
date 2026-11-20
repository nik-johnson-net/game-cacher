//! Compact in-memory form of LRU index entries. The index holds one entry per
//! cached object (millions on a large cache), so an object of a known platform
//! is keyed by its platform number plus the 32-byte object hash instead of the
//! prefix string, and the slice bitset keeps its first word inline.
use std::hash::{Hash, Hasher};

use crate::platforms::IDENTIFIERS;

/// An object prefix as stored in the index.
#[derive(Debug, Clone)]
pub(super) enum Key {
    /// `cache/<IDENTIFIERS[platform]>/<h0h1>/<h2h3>/<hex(hash)>`, the layout of
    /// `ObjectKey::prefix`; the identifier is the one in the prefix.
    Object { platform: u8, hash: [u8; 32] },
    /// Any other prefix (unknown identifier, odd layout), kept verbatim.
    /// Identified by `prefix` alone; `identifier` is carried along for usage.
    Other {
        prefix: Box<str>,
        identifier: Box<str>,
    },
}

impl Key {
    /// Key for an object registered under `identifier` (used only when the
    /// prefix isn't a canonical one of a known platform).
    pub fn new(prefix: &str, identifier: &str) -> Key {
        match canonical(prefix) {
            Some((platform, hash)) => Key::Object { platform, hash },
            None => Key::Other {
                prefix: prefix.into(),
                identifier: identifier.into(),
            },
        }
    }

    /// Key for looking up an existing object by prefix.
    pub fn find(prefix: &str) -> Key {
        Key::new(prefix, "")
    }

    pub fn identifier(&self) -> &str {
        match self {
            Key::Object { platform, .. } => IDENTIFIERS[*platform as usize],
            Key::Other { identifier, .. } => identifier,
        }
    }

    pub fn prefix(&self) -> String {
        match self {
            Key::Object { platform, hash } => {
                let h = hex::encode(hash);
                let id = IDENTIFIERS[*platform as usize];
                format!("cache/{id}/{}/{}/{h}", &h[0..2], &h[2..4])
            }
            Key::Other { prefix, .. } => prefix.to_string(),
        }
    }
}

impl PartialEq for Key {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (
                Key::Object { platform, hash },
                Key::Object {
                    platform: p,
                    hash: h,
                },
            ) => platform == p && hash == h,
            (Key::Other { prefix, .. }, Key::Other { prefix: p, .. }) => prefix == p,
            _ => false,
        }
    }
}

impl Eq for Key {}

impl Hash for Key {
    fn hash<H: Hasher>(&self, state: &mut H) {
        match self {
            Key::Object { platform, hash } => (0u8, platform, hash).hash(state),
            Key::Other { prefix, .. } => (1u8, prefix).hash(state),
        }
    }
}

/// Splits a canonical prefix of a known platform into (platform, hash); None
/// otherwise, so that formatting the key gives back exactly the same string.
fn canonical(prefix: &str) -> Option<(u8, [u8; 32])> {
    let (id, rest) = prefix.strip_prefix("cache/")?.split_once('/')?;
    let (a, rest) = rest.split_once('/')?;
    let (b, h) = rest.split_once('/')?;
    let lower_hex = h.len() == 64 && h.bytes().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f'));
    if !lower_hex || a != &h[0..2] || b != &h[2..4] {
        return None;
    }
    let platform = IDENTIFIERS.iter().position(|i| *i == id)? as u8;
    let mut hash = [0u8; 32];
    hex::decode_to_slice(h, &mut hash).ok()?;
    Some((platform, hash))
}

/// Bitset of stored slice indices. Word 0 is inline: at the default 1 MiB slice
/// size it covers every object up to 64 MiB without a heap allocation.
#[derive(Debug, Clone, Default)]
pub(super) struct Slices {
    first: u64,
    rest: Option<Box<[u64]>>,
}

impl Slices {
    fn words(&self) -> usize {
        1 + self.rest.as_ref().map_or(0, |r| r.len())
    }

    fn word(&self, i: usize) -> u64 {
        match i {
            0 => self.first,
            _ => self
                .rest
                .as_ref()
                .and_then(|r| r.get(i - 1))
                .copied()
                .unwrap_or(0),
        }
    }

    fn word_mut(&mut self, i: usize) -> &mut u64 {
        if i == 0 {
            return &mut self.first;
        }
        if self.rest.as_ref().map_or(0, |r| r.len()) < i {
            let mut grown = self.rest.take().map(Vec::from).unwrap_or_default();
            grown.resize(i, 0);
            self.rest = Some(grown.into_boxed_slice());
        }
        let rest = self.rest.as_mut().expect("grown above");
        &mut rest[i - 1]
    }

    /// Sets bit `index`; returns true if it was not set before.
    pub fn set(&mut self, index: u64) -> bool {
        let (i, bit) = ((index / 64) as usize, 1u64 << (index % 64));
        let w = self.word_mut(i);
        let was = *w & bit != 0;
        *w |= bit;
        !was
    }

    /// Clears bit `index`; returns true if it was set.
    pub fn clear(&mut self, index: u64) -> bool {
        let (i, bit) = ((index / 64) as usize, 1u64 << (index % 64));
        if self.word(i) & bit == 0 {
            return false;
        }
        *self.word_mut(i) &= !bit;
        true
    }

    /// Number of set bits below `count`.
    pub fn count_below(&self, count: u64) -> u64 {
        let full = (count / 64) as usize;
        let mut n: u64 = (0..full.min(self.words()))
            .map(|i| self.word(i).count_ones() as u64)
            .sum();
        let rem = count % 64;
        if rem > 0 {
            n += (self.word(full) & ((1u64 << rem) - 1)).count_ones() as u64;
        }
        n
    }

    /// Sets every bit set in `other`.
    pub fn union(&mut self, other: &Slices) {
        for i in 0..other.words() {
            let w = other.word(i);
            if w != 0 {
                *self.word_mut(i) |= w;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::key::ObjectKey;

    #[test]
    fn canonical_prefixes_round_trip() {
        for id in IDENTIFIERS {
            let p = ObjectKey::new(id, "/depot/1/chunk/abc").prefix();
            let k = Key::new(&p, id);
            assert!(matches!(k, Key::Object { .. }), "{p}");
            assert_eq!(k.prefix(), p);
            assert_eq!(k.identifier(), *id);
            assert_eq!(Key::find(&p), k);
        }
    }

    #[test]
    fn other_prefixes_kept_verbatim() {
        let h = "ab".to_string() + &"0".repeat(62);
        for p in [
            "p1".to_string(),
            "cache/x/aa/bb/c".to_string(),
            format!("cache/unknown/ab/00/{h}"), // not a known platform
            format!("cache/steam/ab/00/{}", h.to_uppercase()), // not lowercase
            format!("cache/steam/ac/00/{h}"),   // wrong fan-out dir
        ] {
            let k = Key::new(&p, "someid");
            assert!(matches!(k, Key::Other { .. }), "{p}");
            assert_eq!(k.prefix(), p);
            assert_eq!(k.identifier(), "someid");
            // Looked up by prefix alone.
            assert_eq!(Key::find(&p), k);
        }
        assert_ne!(Key::find("p1"), Key::find("p2"));
    }

    #[test]
    fn slice_bits() {
        let mut s = Slices::default();
        assert!(s.set(3));
        assert!(!s.set(3));
        assert!(s.rest.is_none(), "first word stays inline");
        assert!(s.set(130));
        assert_eq!(s.count_below(4), 1);
        assert_eq!(s.count_below(131), 2);
        assert!(!s.clear(500), "clearing beyond the end doesn't grow");
        assert_eq!(s.words(), 3);
        assert!(s.clear(130));
        assert!(!s.clear(130));
        let mut t = Slices::default();
        t.set(0);
        t.union(&s);
        assert_eq!(t.count_below(64), 2);
    }
}
