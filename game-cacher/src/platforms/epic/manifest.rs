//! Epic build manifest parsing (binary and JSON), following legendary's
//! `models/manifest.py` / `models/json_manifest.py`. Only the chunk data list
//! is needed for cache warming; the file list is skipped.

use std::io::Read;

use anyhow::{Context, Result, ensure};

const MAGIC: u32 = 0x44BE_C00C;
const FLAG_COMPRESSED: u8 = 0x1;
const FLAG_ENCRYPTED: u8 = 0x2;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    /// GUID as four u32 parts, as stored in the manifest.
    pub guid: [u32; 4],
    pub hash: u64,
    pub group: u8,
    /// Uncompressed size.
    pub window_size: u32,
    /// Download (compressed) size.
    pub file_size: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub app_name: String,
    pub build_version: String,
    pub feature_level: u32,
    pub chunks: Vec<Chunk>,
}

/// Chunk directory for a manifest feature level (legendary `get_chunk_dir`).
pub fn chunk_dir(feature_level: u32) -> &'static str {
    match feature_level {
        22.. => "ChunksV5",
        15..=21 => "ChunksV4",
        6..=14 => "ChunksV3",
        3..=5 => "ChunksV2",
        _ => "Chunks",
    }
}

impl Chunk {
    /// `{dir}/{group:02}/{hash:016X}_{GUID}.chunk`, GUID = 4x `{:08X}` without hyphens.
    pub fn path(&self, feature_level: u32) -> String {
        format!(
            "{}/{:02}/{:016X}_{:08X}{:08X}{:08X}{:08X}.chunk",
            chunk_dir(feature_level),
            self.group,
            self.hash,
            self.guid[0],
            self.guid[1],
            self.guid[2],
            self.guid[3]
        )
    }
}

impl Manifest {
    /// Chunk paths relative to the manifest's base URL.
    pub fn chunk_paths(&self) -> Vec<String> {
        self.chunks
            .iter()
            .map(|c| c.path(self.feature_level))
            .collect()
    }
}

pub fn parse(data: &[u8]) -> Result<Manifest> {
    let manifest = if data.first() == Some(&b'{') {
        parse_json(data)?
    } else {
        parse_binary(data)?
    };
    ensure!(
        manifest.feature_level < 22,
        "manifest feature level {} (encrypted chunk paths) is not supported",
        manifest.feature_level
    );
    Ok(manifest)
}

struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self
            .pos
            .checked_add(n)
            .context("manifest offset overflow")?;
        let s = self
            .data
            .get(self.pos..end)
            .context("unexpected end of manifest")?;
        self.pos = end;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.bytes(1)?[0])
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.bytes(4)?.try_into()?))
    }
    fn i32(&mut self) -> Result<i32> {
        Ok(i32::from_le_bytes(self.bytes(4)?.try_into()?))
    }
    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.bytes(8)?.try_into()?))
    }
    fn i64(&mut self) -> Result<i64> {
        Ok(i64::from_le_bytes(self.bytes(8)?.try_into()?))
    }
    fn remaining(&self) -> usize {
        self.data.len() - self.pos
    }
    /// Unreal FString: i32 length; negative = UTF-16 (len chars incl. terminator).
    fn fstring(&mut self) -> Result<String> {
        let len = self.i32()?;
        if len == 0 {
            return Ok(String::new());
        }
        if len < 0 {
            let n = len.unsigned_abs() as usize;
            let raw = self.bytes(n * 2)?;
            let units: Vec<u16> = raw
                .as_chunks::<2>()
                .0
                .iter()
                .map(|b| u16::from_le_bytes(*b))
                .collect();
            let units = units.strip_suffix(&[0]).unwrap_or(&units);
            Ok(String::from_utf16_lossy(units))
        } else {
            let raw = self.bytes(len as usize)?;
            let raw = raw.strip_suffix(&[0]).unwrap_or(raw);
            Ok(String::from_utf8_lossy(raw).into_owned())
        }
    }
}

fn parse_binary(data: &[u8]) -> Result<Manifest> {
    let mut c = Cursor { data, pos: 0 };
    ensure!(
        c.u32().ok() == Some(MAGIC),
        "not an Epic binary manifest (bad magic)"
    );
    let header_size = c.u32()? as usize;
    let size_uncompressed = c.u32()? as usize;
    let _size_compressed = c.u32()?;
    c.bytes(20)?; // sha1 of the (uncompressed) body
    let stored_as = c.u8()?;
    let _version = c.u32()?;
    ensure!(
        stored_as & FLAG_ENCRYPTED == 0,
        "encrypted manifests are not supported"
    );
    let body = data
        .get(header_size..)
        .context("manifest header truncated")?;
    let body = if stored_as & FLAG_COMPRESSED != 0 {
        let mut out = Vec::with_capacity(size_uncompressed.min(64 << 20));
        flate2::read::ZlibDecoder::new(body)
            .read_to_end(&mut out)
            .context("inflating manifest body")?;
        ensure!(
            out.len() == size_uncompressed,
            "manifest body size mismatch ({} != {size_uncompressed})",
            out.len()
        );
        out
    } else {
        body.to_vec()
    };
    parse_body(&body)
}

fn parse_body(body: &[u8]) -> Result<Manifest> {
    let mut c = Cursor { data: body, pos: 0 };
    // Meta section.
    let meta_size = c.u32()? as usize;
    let _data_version = c.u8()?;
    let feature_level = c.u32()?;
    let _is_file_data = c.u8()?;
    let _app_id = c.u32()?;
    let app_name = c.fstring()?;
    let build_version = c.fstring()?;
    // Remaining meta fields (launch exe, prereqs, ...) are skipped via meta_size.
    c.pos = meta_size;
    // Chunk data list.
    let _cdl_size = c.u32()?;
    let _cdl_version = c.u8()?;
    let count = c.u32()? as usize;
    // Each chunk takes at least 16+8+20+1+4+8 bytes; guards against bogus counts.
    ensure!(
        count <= c.remaining() / 57,
        "chunk count {count} out of range"
    );
    let mut chunks = vec![
        Chunk {
            guid: [0; 4],
            hash: 0,
            group: 0,
            window_size: 0,
            file_size: 0,
        };
        count
    ];
    for ch in &mut chunks {
        ch.guid = [c.u32()?, c.u32()?, c.u32()?, c.u32()?];
    }
    for ch in &mut chunks {
        ch.hash = c.u64()?;
    }
    c.bytes(20 * count)?; // sha1 per chunk
    for ch in &mut chunks {
        ch.group = c.u8()?;
    }
    for ch in &mut chunks {
        ch.window_size = c.u32()?;
    }
    for ch in &mut chunks {
        ch.file_size = c.i64()?;
    }
    Ok(Manifest {
        app_name,
        build_version,
        feature_level,
        chunks,
    })
}

// --- JSON manifests (legacy) ----------------------------------------------

/// Epic's JSON "blob" numbers: decimal triplets, one per little-endian byte.
fn blob_to_num(s: &str) -> Result<u64> {
    ensure!(s.len().is_multiple_of(3) && s.len() <= 24, "bad blob {s:?}");
    let mut n = 0u64;
    for (i, part) in s.as_bytes().chunks(3).enumerate() {
        let b: u64 = std::str::from_utf8(part)?.parse()?;
        ensure!(b < 256, "bad blob {s:?}");
        n |= b << (8 * i);
    }
    Ok(n)
}

fn parse_json(data: &[u8]) -> Result<Manifest> {
    let v: serde_json::Value = serde_json::from_slice(data).context("parsing JSON manifest")?;
    let str_of = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or_default();
    let feature_level = match v.get("ManifestFileVersion").and_then(|x| x.as_str()) {
        Some(s) => blob_to_num(s)? as u32,
        None => 13,
    };
    let hashes = v
        .get("ChunkHashList")
        .and_then(|x| x.as_object())
        .context("JSON manifest has no ChunkHashList")?;
    let groups = v.get("DataGroupList").and_then(|x| x.as_object());
    let sizes = v.get("ChunkFilesizeList").and_then(|x| x.as_object());
    let mut chunks = Vec::with_capacity(hashes.len());
    for (guid_hex, hash) in hashes {
        ensure!(guid_hex.len() == 32, "bad chunk guid {guid_hex:?}");
        let mut guid = [0u32; 4];
        for (i, g) in guid.iter_mut().enumerate() {
            *g = u32::from_str_radix(&guid_hex[i * 8..i * 8 + 8], 16)
                .with_context(|| format!("bad chunk guid {guid_hex:?}"))?;
        }
        let group = groups
            .and_then(|g| g.get(guid_hex))
            .and_then(|g| g.as_u64().or_else(|| g.as_str()?.parse().ok()))
            .context("chunk without DataGroupList entry")?;
        let file_size = match sizes.and_then(|s| s.get(guid_hex)).and_then(|s| s.as_str()) {
            Some(s) => blob_to_num(s)? as i64,
            None => 0,
        };
        chunks.push(Chunk {
            guid,
            hash: blob_to_num(hash.as_str().context("chunk hash is not a string")?)?,
            group: group as u8,
            window_size: 0,
            file_size,
        });
    }
    Ok(Manifest {
        app_name: str_of("AppNameString").to_string(),
        build_version: str_of("BuildVersionString").to_string(),
        feature_level,
        chunks,
    })
}

/// Manifest writer producing the binary format `parse` reads (tests only).
#[doc(hidden)]
pub mod testutil {
    use super::*;
    use std::io::Write;

    fn fstring(out: &mut Vec<u8>, s: &str) {
        out.extend(((s.len() + 1) as i32).to_le_bytes());
        out.extend(s.as_bytes());
        out.push(0);
    }

    pub fn build_manifest(
        app: &str,
        build: &str,
        feature_level: u32,
        chunks: &[Chunk],
        compress: bool,
    ) -> Vec<u8> {
        let mut meta = Vec::new();
        meta.push(0u8); // data_version
        meta.extend(feature_level.to_le_bytes());
        meta.push(1);
        meta.extend(0u32.to_le_bytes());
        fstring(&mut meta, app);
        fstring(&mut meta, build);
        fstring(&mut meta, "game.exe");
        fstring(&mut meta, "");
        meta.extend(0u32.to_le_bytes()); // prereq ids
        for _ in 0..3 {
            fstring(&mut meta, "");
        }
        let mut body = ((meta.len() + 4) as u32).to_le_bytes().to_vec();
        body.extend(meta);

        let mut cdl = vec![0u8];
        cdl.extend((chunks.len() as u32).to_le_bytes());
        for c in chunks {
            c.guid.iter().for_each(|g| cdl.extend(g.to_le_bytes()));
        }
        chunks.iter().for_each(|c| cdl.extend(c.hash.to_le_bytes()));
        cdl.extend(vec![0u8; 20 * chunks.len()]);
        chunks.iter().for_each(|c| cdl.push(c.group));
        chunks
            .iter()
            .for_each(|c| cdl.extend(c.window_size.to_le_bytes()));
        chunks
            .iter()
            .for_each(|c| cdl.extend(c.file_size.to_le_bytes()));
        body.extend(((cdl.len() + 4) as u32).to_le_bytes());
        body.extend(cdl);
        // Empty file manifest list.
        body.extend(9u32.to_le_bytes());
        body.push(0);
        body.extend(0u32.to_le_bytes());

        let size_uncompressed = body.len() as u32;
        let (stored, payload) = if compress {
            let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
            e.write_all(&body).unwrap();
            (FLAG_COMPRESSED, e.finish().unwrap())
        } else {
            (0, body)
        };
        let mut out = MAGIC.to_le_bytes().to_vec();
        out.extend(41u32.to_le_bytes()); // header size
        out.extend(size_uncompressed.to_le_bytes());
        out.extend((payload.len() as u32).to_le_bytes());
        out.extend([0u8; 20]);
        out.push(stored);
        out.extend(feature_level.to_le_bytes());
        assert_eq!(out.len(), 41);
        out.extend(payload);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::testutil::build_manifest;
    use super::*;

    fn sample() -> Vec<Chunk> {
        vec![
            Chunk {
                guid: [0x1A2B3C4D, 0, 0xDEADBEEF, 1],
                hash: 0x1122_3344_5566_7788,
                group: 7,
                window_size: 1_048_576,
                file_size: 500_000,
            },
            Chunk {
                guid: [1, 2, 3, 4],
                hash: 0xAB,
                group: 42,
                window_size: 10,
                file_size: 9,
            },
        ]
    }

    #[test]
    fn chunk_path_format() {
        let c = &sample()[0];
        assert_eq!(
            c.path(18),
            "ChunksV4/07/1122334455667788_1A2B3C4D00000000DEADBEEF00000001.chunk"
        );
        assert_eq!(
            sample()[1].path(3),
            "ChunksV2/42/00000000000000AB_00000001000000020000000300000004.chunk"
        );
    }

    #[test]
    fn chunk_dirs_by_feature_level() {
        let dirs: Vec<_> = [0, 2, 3, 5, 6, 14, 15, 21, 22]
            .iter()
            .map(|l| chunk_dir(*l))
            .collect();
        assert_eq!(
            dirs,
            [
                "Chunks", "Chunks", "ChunksV2", "ChunksV2", "ChunksV3", "ChunksV3", "ChunksV4",
                "ChunksV4", "ChunksV5"
            ]
        );
    }

    #[test]
    fn binary_roundtrip_compressed_and_raw() {
        for compress in [true, false] {
            let data = build_manifest("Fortnite", "1.2.3", 18, &sample(), compress);
            let m = parse(&data).unwrap();
            assert_eq!(m.app_name, "Fortnite");
            assert_eq!(m.build_version, "1.2.3");
            assert_eq!(m.feature_level, 18);
            assert_eq!(m.chunks, sample());
        }
    }

    #[test]
    fn rejects_bad_input() {
        assert!(parse(b"garbage garbage garbage").is_err());
        let data = build_manifest("a", "b", 18, &sample(), true);
        assert!(parse(&data[..data.len() - 10]).is_err());
        let mut bad_magic = data.clone();
        bad_magic[0] ^= 0xFF;
        assert!(parse(&bad_magic).is_err());
        let enc = build_manifest("a", "b", 22, &sample(), true);
        assert!(parse(&enc).is_err());
    }

    #[test]
    fn json_manifest() {
        let json = serde_json::json!({
            "ManifestFileVersion": "013000000000",
            "AppNameString": "Old",
            "BuildVersionString": "9",
            "ChunkHashList": { "1A2B3C4D00000000DEADBEEF00000001": "136068051085017034017" },
            "DataGroupList": { "1A2B3C4D00000000DEADBEEF00000001": 7 },
            "ChunkFilesizeList": { "1A2B3C4D00000000DEADBEEF00000001": "232003000000" }
        });
        let m = parse(json.to_string().as_bytes()).unwrap();
        assert_eq!(m.feature_level, 13);
        assert_eq!(m.chunks[0].group, 7);
        assert_eq!(m.chunks[0].file_size, 1000);
        assert_eq!(m.chunks[0].hash, 0x0011_2211_5533_4488);
        assert_eq!(m.chunks[0].guid, [0x1A2B3C4D, 0, 0xDEADBEEF, 1]);
        assert_eq!(blob_to_num("013000000000").unwrap(), 13);
        assert!(blob_to_num("1234").is_err());
    }
}
