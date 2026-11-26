//! `index/lru.json` encoding, written straight from the in-memory map and read
//! entry by entry, so neither direction holds a second full copy of the index.
//! Format: `{"version":1,"objects":{"<prefix>":{"identifier","bytes","meta_bytes","total"?,"last_access"}}}`
//! (`total`: the object's full size, written once known; older snapshots lack it).
use std::collections::HashMap;
use std::fmt;

use serde::de::{self, DeserializeSeed, IgnoredAny, MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::Entry;
use super::compact::Key;

/// The only snapshot format version; anything else is rebuilt from storage.
const VERSION: u32 = 1;
/// Rough JSON size of one object, to size the output buffer up front.
const BYTES_PER_OBJECT: usize = 170;

#[derive(Serialize, Deserialize)]
struct SnapEntry<S> {
    identifier: S,
    bytes: u64,
    #[serde(default)]
    meta_bytes: u64,
    #[serde(default, skip_serializing_if = "is_zero")]
    total: u64,
    last_access: u64,
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

struct Objects<'a>(&'a HashMap<Key, Entry>);

impl Serialize for Objects<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_map(self.0.iter().map(|(k, e)| {
            let v = SnapEntry {
                identifier: k.identifier(),
                bytes: e.bytes,
                meta_bytes: e.meta_bytes,
                total: e.total,
                last_access: e.last_access,
            };
            (k.prefix(), v)
        }))
    }
}

pub(super) fn encode(objects: &HashMap<Key, Entry>) -> serde_json::Result<Vec<u8>> {
    #[derive(Serialize)]
    struct Snapshot<'a> {
        version: u32,
        objects: Objects<'a>,
    }
    let mut out = Vec::with_capacity(64 + objects.len() * BYTES_PER_OBJECT);
    serde_json::to_writer(
        &mut out,
        &Snapshot {
            version: VERSION,
            objects: Objects(objects),
        },
    )?;
    Ok(out)
}

pub(super) fn decode(bytes: &[u8]) -> serde_json::Result<HashMap<Key, Entry>> {
    let mut de = serde_json::Deserializer::from_slice(bytes);
    let objects = SnapshotSeed.deserialize(&mut de)?;
    de.end()?;
    Ok(objects)
}

/// The top-level object: picks out `objects`, ignores everything else.
struct SnapshotSeed;

impl<'de> DeserializeSeed<'de> for SnapshotSeed {
    type Value = HashMap<Key, Entry>;
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
        d.deserialize_map(self)
    }
}

impl<'de> Visitor<'de> for SnapshotSeed {
    type Value = HashMap<Key, Entry>;
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("an LRU index snapshot")
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let (mut version, mut objects) = (None, None);
        while let Some(k) = map.next_key::<String>()? {
            match k.as_str() {
                "version" => version = Some(map.next_value::<u32>()?),
                "objects" => objects = Some(map.next_value_seed(ObjectsSeed)?),
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        match version {
            Some(VERSION) => {}
            Some(v) => return Err(de::Error::custom(format!("unsupported version {v}"))),
            None => return Err(de::Error::missing_field("version")),
        }
        objects.ok_or_else(|| de::Error::missing_field("objects"))
    }
}

/// The `objects` map, converted to compact entries as it is read.
struct ObjectsSeed;

impl<'de> DeserializeSeed<'de> for ObjectsSeed {
    type Value = HashMap<Key, Entry>;
    fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
        d.deserialize_map(self)
    }
}

impl<'de> Visitor<'de> for ObjectsSeed {
    type Value = HashMap<Key, Entry>;
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a map of object prefix to entry")
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut out = HashMap::with_capacity(map.size_hint().unwrap_or(0));
        while let Some((prefix, e)) = map.next_entry::<String, SnapEntry<String>>()? {
            let key = Key::new(&prefix, &e.identifier);
            let entry = Entry {
                bytes: e.bytes,
                meta_bytes: e.meta_bytes,
                total: e.total,
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
                "legacy/odd":{{"identifier":"gone","bytes":5,"last_access":9}}
            }},"future":[1,2]}}"#
        );
        let objects = decode(json.as_bytes()).unwrap();
        assert_eq!(objects.len(), 2);
        let (k, e) = objects.get_key_value(&Key::find(&p)).unwrap();
        assert_eq!((e.bytes, e.meta_bytes, e.last_access), (100, 2, 7));
        assert_eq!(e.total, 0, "older snapshots have no sizes");
        assert!(matches!(k, Key::Object { .. }));
        assert_eq!(k.identifier(), "steam");
        let (k, odd) = objects.get_key_value(&Key::find("legacy/odd")).unwrap();
        assert_eq!((odd.bytes, odd.meta_bytes), (5, 0));
        assert_eq!(k.identifier(), "gone");
    }

    #[test]
    fn round_trip() {
        let p = ObjectKey::new("epic", "/b").prefix();
        let mut objects = HashMap::new();
        let entry = Entry {
            bytes: 42,
            meta_bytes: 3,
            total: 39,
            last_access: 11,
            ..Default::default()
        };
        objects.insert(Key::new(&p, "epic"), entry.clone());
        objects.insert(Key::new("odd", "x"), Entry { total: 0, ..entry });
        let json = encode(&objects).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&json).unwrap();
        assert_eq!(v["version"], 1);
        assert_eq!(v["objects"][&p]["identifier"], "epic");
        assert_eq!(v["objects"][&p]["bytes"], 42);
        assert_eq!(v["objects"][&p]["total"], 39);
        assert_eq!(v["objects"]["odd"]["identifier"], "x");
        assert!(
            v["objects"]["odd"].get("total").is_none(),
            "unknown: omitted"
        );
        let back = decode(&json).unwrap();
        let e = &back[&Key::find(&p)];
        assert_eq!((e.bytes, e.meta_bytes, e.last_access), (42, 3, 11));
        assert_eq!(e.total, 39);
        assert_eq!(back.len(), 2);
    }

    #[test]
    fn rejects_garbage() {
        assert!(decode(b"nope").is_err());
        assert!(decode(br#"{"version":1}"#).is_err());
        assert!(decode(br#"{"version":1,"objects":{}} trailing"#).is_err());
        assert!(decode(br#"{"version":1,"objects":{"p":{"bytes":1}}}"#).is_err());
        // Version must be present and supported (it may come after objects).
        assert!(decode(br#"{"objects":{}}"#).is_err());
        assert!(decode(br#"{"version":2,"objects":{}}"#).is_err());
        assert!(decode(br#"{"objects":{},"version":1}"#).unwrap().is_empty());
    }
}
