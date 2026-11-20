//! `index/lru.json` encoding, written straight from the in-memory map and read
//! entry by entry, so neither direction holds a second full copy of the index.
//! Format: `{"version":1,"objects":{"<prefix>":{"identifier","bytes","meta_bytes","last_access"}}}`.
use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use serde::de::{self, DeserializeSeed, IgnoredAny, MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::Entry;
use super::compact::{Ids, Key, name};

/// Rough JSON size of one object, to size the output buffer up front.
const BYTES_PER_OBJECT: usize = 170;

#[derive(Serialize, Deserialize)]
struct SnapEntry<S> {
    identifier: S,
    bytes: u64,
    #[serde(default)]
    meta_bytes: u64,
    last_access: u64,
}

struct Objects<'a>(&'a HashMap<Key, Entry>, &'a [Arc<str>]);

impl Serialize for Objects<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let Objects(objects, names) = self;
        s.collect_map(objects.iter().map(|(k, e)| {
            let v = SnapEntry {
                identifier: name(names, e.id),
                bytes: e.bytes,
                meta_bytes: e.meta_bytes,
                last_access: e.last_access,
            };
            (k.prefix(names), v)
        }))
    }
}

pub(super) fn encode(objects: &HashMap<Key, Entry>, ids: &Ids) -> serde_json::Result<Vec<u8>> {
    #[derive(Serialize)]
    struct Snapshot<'a> {
        version: u32,
        objects: Objects<'a>,
    }
    let names = ids.names();
    let mut out = Vec::with_capacity(64 + objects.len() * BYTES_PER_OBJECT);
    serde_json::to_writer(
        &mut out,
        &Snapshot {
            version: 1,
            objects: Objects(objects, &names),
        },
    )?;
    Ok(out)
}

pub(super) fn decode(bytes: &[u8], ids: &Ids) -> serde_json::Result<HashMap<Key, Entry>> {
    let mut de = serde_json::Deserializer::from_slice(bytes);
    let objects = SnapshotSeed(ids).deserialize(&mut de)?;
    de.end()?;
    Ok(objects)
}

/// The top-level object: picks out `objects`, ignores everything else.
struct SnapshotSeed<'a>(&'a Ids);

impl<'de> DeserializeSeed<'de> for SnapshotSeed<'_> {
    type Value = HashMap<Key, Entry>;
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
        d.deserialize_map(self)
    }
}

impl<'de> Visitor<'de> for SnapshotSeed<'_> {
    type Value = HashMap<Key, Entry>;
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("an LRU index snapshot")
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut objects = None;
        while let Some(k) = map.next_key::<String>()? {
            if k == "objects" {
                objects = Some(map.next_value_seed(ObjectsSeed(self.0))?);
            } else {
                map.next_value::<IgnoredAny>()?;
            }
        }
        objects.ok_or_else(|| de::Error::missing_field("objects"))
    }
}

/// The `objects` map, converted to compact entries as it is read.
struct ObjectsSeed<'a>(&'a Ids);

impl<'de> DeserializeSeed<'de> for ObjectsSeed<'_> {
    type Value = HashMap<Key, Entry>;
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
        d.deserialize_map(self)
    }
}

impl<'de> Visitor<'de> for ObjectsSeed<'_> {
    type Value = HashMap<Key, Entry>;
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a map of object prefix to entry")
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let ids = self.0;
        let mut out = HashMap::with_capacity(map.size_hint().unwrap_or(0));
        while let Some((prefix, e)) = map.next_entry::<String, SnapEntry<String>>()? {
            let key = Key::parse(&prefix, |n| Some(ids.intern(n)))
                .unwrap_or_else(|| Key::Other(prefix.into()));
            let entry = Entry {
                id: ids.intern(&e.identifier),
                bytes: e.bytes,
                meta_bytes: e.meta_bytes,
                last_access: e.last_access,
                ..Default::default()
            };
            out.insert(key, entry);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::key::ObjectKey;

    #[test]
    fn reads_the_existing_format() {
        let p = ObjectKey::new("steam", "/a").prefix();
        let json = format!(
            r#"{{"version":1,"objects":{{
                "{p}":{{"identifier":"steam","bytes":100,"meta_bytes":2,"last_access":7}},
                "legacy/odd":{{"identifier":"epic","bytes":5,"last_access":9}}
            }},"future":[1,2]}}"#
        );
        let ids = Ids::default();
        let objects = decode(json.as_bytes(), &ids).unwrap();
        assert_eq!(objects.len(), 2);
        let k = Key::parse(&p, |n| ids.get(n)).unwrap();
        let e = &objects[&k];
        assert_eq!((e.bytes, e.meta_bytes, e.last_access), (100, 2, 7));
        assert_eq!(name(&ids.names(), e.id), "steam");
        let odd = &objects[&Key::Other("legacy/odd".into())];
        assert_eq!((odd.bytes, odd.meta_bytes), (5, 0));
        assert_eq!(name(&ids.names(), odd.id), "epic");
    }

    #[test]
    fn round_trip() {
        let ids = Ids::default();
        let p = ObjectKey::new("epic", "/b").prefix();
        let mut objects = HashMap::new();
        let entry = Entry {
            id: ids.intern("epic"),
            bytes: 42,
            meta_bytes: 3,
            last_access: 11,
            ..Default::default()
        };
        objects.insert(Key::parse(&p, |n| Some(ids.intern(n))).unwrap(), entry);
        let json = encode(&objects, &ids).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&json).unwrap();
        assert_eq!(v["version"], 1);
        assert_eq!(v["objects"][&p]["identifier"], "epic");
        assert_eq!(v["objects"][&p]["bytes"], 42);
        let back = decode(&json, &Ids::default()).unwrap();
        let e = back.values().next().unwrap();
        assert_eq!((e.bytes, e.meta_bytes, e.last_access), (42, 3, 11));
    }

    #[test]
    fn rejects_garbage() {
        let ids = Ids::default();
        assert!(decode(b"nope", &ids).is_err());
        assert!(decode(br#"{"version":1}"#, &ids).is_err());
        assert!(decode(br#"{"objects":{}} trailing"#, &ids).is_err());
        assert!(decode(br#"{"objects":{"p":{"bytes":1}}}"#, &ids).is_err());
    }
}
