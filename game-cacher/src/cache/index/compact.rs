//! Compact in-memory form of LRU index entries. The index holds one entry per
//! cached object (millions on a large cache), so keys are stored as an interned
//! identifier plus the 32-byte object hash instead of the prefix string, and the
//! slice bitset keeps its first word inline.
use std::sync::{Arc, RwLock};

/// Interned identifier strings (one per platform; only grows).
#[derive(Default)]
pub(super) struct Ids(RwLock<Vec<Arc<str>>>);

impl Ids {
    pub fn get(&self, name: &str) -> Option<u32> {
        let names = self.0.read().unwrap_or_else(|e| e.into_inner());
        names.iter().position(|n| &**n == name).map(|i| i as u32)
    }

    pub fn intern(&self, name: &str) -> u32 {
        if let Some(id) = self.get(name) {
            return id;
        }
        let mut names = self.0.write().unwrap_or_else(|e| e.into_inner());
        if let Some(i) = names.iter().position(|n| &**n == name) {
            return i as u32;
        }
        names.push(name.into());
        (names.len() - 1) as u32
    }

    /// All names, indexed by id.
    pub fn names(&self) -> Vec<Arc<str>> {
        self.0.read().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

/// Name of `id` in a `names()` table.
pub(super) fn name(names: &[Arc<str>], id: u32) -> &str {
    names.get(id as usize).map_or("", |n| n)
}

/// An object prefix as stored in the index.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) enum Key {
    /// `cache/<id>/<h0h1>/<h2h3>/<hex(hash)>`, the layout of `ObjectKey::prefix`.
    Object { id: u32, hash: [u8; 32] },
    /// Any other prefix, kept verbatim.
    Other(Box<str>),
}

impl Key {
    /// Parses a prefix. `id` resolves the identifier of a canonical prefix; None
    /// from it means no such object can be indexed (the identifier is unknown).
    pub fn parse(prefix: &str, id: impl FnOnce(&str) -> Option<u32>) -> Option<Key> {
        match canonical(prefix) {
            Some((name, hash)) => id(name).map(|id| Key::Object { id, hash }),
            None => Some(Key::Other(prefix.into())),
        }
    }

    pub fn prefix(&self, names: &[Arc<str>]) -> String {
        match self {
            Key::Object { id, hash } => {
                let h = hex::encode(hash);
                format!("cache/{}/{}/{}/{h}", name(names, *id), &h[0..2], &h[2..4])
            }
            Key::Other(p) => p.to_string(),
        }
    }
}

/// Splits a canonical prefix into (identifier, hash); None if it isn't one
/// (so that formatting the parsed key gives back exactly the same string).
fn canonical(prefix: &str) -> Option<(&str, [u8; 32])> {
    let (id, rest) = prefix.strip_prefix("cache/")?.split_once('/')?;
    let (a, rest) = rest.split_once('/')?;
    let (b, h) = rest.split_once('/')?;
    let lower_hex = h.len() == 64 && h.bytes().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f'));
    if id.is_empty() || !lower_hex || a != &h[0..2] || b != &h[2..4] {
        return None;
    }
    let mut hash = [0u8; 32];
    hex::decode_to_slice(h, &mut hash).ok()?;
    Some((id, hash))
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
        let ids = Ids::default();
        let p = ObjectKey::new("steam", "/depot/1/chunk/abc").prefix();
        let k = Key::parse(&p, |n| Some(ids.intern(n))).unwrap();
        assert!(matches!(k, Key::Object { .. }));
        assert_eq!(k.prefix(&ids.names()), p);
        // Same prefix, same key; unknown identifier on a read-only lookup → None.
        assert_eq!(Key::parse(&p, |n| ids.get(n)), Some(k));
        let epic = ObjectKey::new("epic", "/x").prefix();
        assert_eq!(Key::parse(&epic, |n| ids.get(n)), None);
    }

    #[test]
    fn other_prefixes_kept_verbatim() {
        let ids = Ids::default();
        let h = "ab".to_string() + &"0".repeat(62);
        for p in [
            "p1".to_string(),
            "cache/x/aa/bb/c".to_string(),
            format!("cache/s/ab/00/{}", h.to_uppercase()), // not lowercase
            format!("cache/s/ac/00/{h}"),                  // wrong fan-out dir
            format!("cache//ab/00/{h}"),                   // empty identifier
        ] {
            let k = Key::parse(&p, |n| Some(ids.intern(n))).unwrap();
            assert_eq!(k, Key::Other(p.clone().into()), "{p}");
            assert_eq!(k.prefix(&ids.names()), p);
        }
    }

    #[test]
    fn interning() {
        let ids = Ids::default();
        assert_eq!(ids.get("steam"), None);
        let s = ids.intern("steam");
        assert_eq!(ids.intern("epic"), s + 1);
        assert_eq!(ids.intern("steam"), s);
        assert_eq!(name(&ids.names(), s + 1), "epic");
        assert_eq!(name(&ids.names(), 99), "");
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
