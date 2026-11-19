//! Depot manifest parsing (format per SteamKit2 `DepotManifest`): the CDN
//! serves a zip with one entry holding magic-prefixed protobuf sections.

use std::collections::BTreeSet;
use std::io::{Cursor, Read};

use anyhow::{Context, Result, bail};
use steam_vent_proto_common::protobuf::Message;
use steam_vent_proto_steam::content_manifest::{ContentManifestMetadata, ContentManifestPayload};

pub const PAYLOAD_MAGIC: u32 = 0x71F6_17D0;
pub const METADATA_MAGIC: u32 = 0x1F48_12BE;
pub const SIGNATURE_MAGIC: u32 = 0x1B81_B817;
pub const END_MAGIC: u32 = 0x32C4_15AB;
/// Pre-protobuf (v4) binary manifest; never served for `/5/` requests.
const LEGACY_MAGIC: u32 = 0x1634_9781;

/// Upper bound for the decompressed manifest (the largest real ones are ~100 MB).
const MAX_MANIFEST_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub depot_id: u32,
    pub gid: u64,
    /// Unique chunk SHA-1 ids, lowercase hex, sorted.
    pub chunks: Vec<String>,
    /// Sum of compressed sizes of the unique chunks.
    pub compressed_bytes: u64,
}

/// Parses a manifest as downloaded from `/depot/<id>/manifest/<gid>/5/<code>`.
pub fn parse(zipped: &[u8]) -> Result<Manifest> {
    let raw = unzip_single(zipped)?;
    parse_raw(&raw)
}

fn unzip_single(data: &[u8]) -> Result<Vec<u8>> {
    let mut archive = zip::ZipArchive::new(Cursor::new(data)).context("manifest is not a zip")?;
    if archive.is_empty() {
        bail!("manifest zip is empty");
    }
    let entry = archive.by_index(0).context("reading manifest zip entry")?;
    let mut out = Vec::new();
    entry
        .take(MAX_MANIFEST_BYTES)
        .read_to_end(&mut out)
        .context("decompressing manifest")?;
    Ok(out)
}

/// Parses the unzipped section stream.
pub fn parse_raw(data: &[u8]) -> Result<Manifest> {
    let mut pos = 0usize;
    let mut payload = None;
    let mut metadata = None;
    loop {
        let magic = read_u32(data, &mut pos).context("truncated manifest")?;
        if magic == END_MAGIC {
            break;
        }
        if magic == LEGACY_MAGIC {
            bail!("legacy (v4) depot manifest format is not supported");
        }
        let len = read_u32(data, &mut pos).context("truncated manifest section header")? as usize;
        let section = data
            .get(pos..pos + len)
            .context("truncated manifest section")?;
        pos += len;
        match magic {
            PAYLOAD_MAGIC => payload = Some(ContentManifestPayload::parse_from_bytes(section)?),
            METADATA_MAGIC => metadata = Some(ContentManifestMetadata::parse_from_bytes(section)?),
            SIGNATURE_MAGIC => {}
            other => bail!("unrecognized depot manifest section magic {other:#x}"),
        }
    }
    let payload = payload.context("manifest has no payload section")?;
    let metadata = metadata.context("manifest has no metadata section")?;

    let mut seen = BTreeSet::new();
    let mut compressed_bytes = 0u64;
    for chunk in payload.mappings.iter().flat_map(|m| m.chunks.iter()) {
        let sha = chunk.sha();
        if sha.len() != 20 {
            bail!("chunk with invalid sha length {}", sha.len());
        }
        if seen.insert(hex::encode(sha)) {
            compressed_bytes += u64::from(chunk.cb_compressed());
        }
    }
    Ok(Manifest {
        depot_id: metadata.depot_id(),
        gid: metadata.gid_manifest(),
        chunks: seen.into_iter().collect(),
        compressed_bytes,
    })
}

fn read_u32(data: &[u8], pos: &mut usize) -> Option<u32> {
    let bytes = data.get(*pos..*pos + 4)?;
    *pos += 4;
    Some(u32::from_le_bytes(bytes.try_into().ok()?))
}

#[cfg(test)]
pub(crate) mod tests {
    use std::io::Write;

    use steam_vent_proto_steam::content_manifest::content_manifest_payload::FileMapping;
    use steam_vent_proto_steam::content_manifest::content_manifest_payload::file_mapping::ChunkData;

    use super::*;

    /// Builds a zipped v5 manifest whose files reference the given chunk shas.
    pub fn build_manifest(depot_id: u32, gid: u64, files: &[&[[u8; 20]]]) -> Vec<u8> {
        let mut payload = ContentManifestPayload::new();
        for (i, chunks) in files.iter().enumerate() {
            let mut file = FileMapping::new();
            file.set_filename(format!("file{i}.bin"));
            for (j, sha) in chunks.iter().enumerate() {
                let mut c = ChunkData::new();
                c.set_sha(sha.to_vec());
                c.set_offset(j as u64 * 1024);
                c.set_cb_original(1024);
                c.set_cb_compressed(100);
                file.chunks.push(c);
            }
            payload.mappings.push(file);
        }
        let mut metadata = ContentManifestMetadata::new();
        metadata.set_depot_id(depot_id);
        metadata.set_gid_manifest(gid);

        let mut raw = Vec::new();
        for (magic, body) in [
            (PAYLOAD_MAGIC, payload.write_to_bytes().unwrap()),
            (METADATA_MAGIC, metadata.write_to_bytes().unwrap()),
            (SIGNATURE_MAGIC, Vec::new()),
        ] {
            raw.extend_from_slice(&magic.to_le_bytes());
            raw.extend_from_slice(&(body.len() as u32).to_le_bytes());
            raw.extend_from_slice(&body);
        }
        raw.extend_from_slice(&END_MAGIC.to_le_bytes());
        zip_bytes(&raw)
    }

    pub fn zip_bytes(raw: &[u8]) -> Vec<u8> {
        let mut out = Cursor::new(Vec::new());
        let mut w = zip::ZipWriter::new(&mut out);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        w.start_file("z", opts).unwrap();
        w.write_all(raw).unwrap();
        w.finish().unwrap();
        out.into_inner()
    }

    #[test]
    fn parses_synthetic_manifest() {
        let a = [0xaa; 20];
        let b = [0x0b; 20];
        let c = [0x01; 20];
        let zipped = build_manifest(731, 99, &[&[a, b], &[b, c], &[]]);
        let m = parse(&zipped).unwrap();
        assert_eq!(m.depot_id, 731);
        assert_eq!(m.gid, 99);
        assert_eq!(
            m.chunks,
            vec![hex::encode(c), hex::encode(b), hex::encode(a)]
        );
        assert_eq!(m.compressed_bytes, 300);
    }

    #[test]
    fn rejects_malformed_manifests() {
        assert!(parse(b"not a zip").is_err());
        // missing metadata section
        let mut raw = PAYLOAD_MAGIC.to_le_bytes().to_vec();
        raw.extend_from_slice(&0u32.to_le_bytes());
        raw.extend_from_slice(&END_MAGIC.to_le_bytes());
        assert!(parse(&zip_bytes(&raw)).is_err());
        // unknown magic
        assert!(parse_raw(&0xdeadbeefu32.to_le_bytes()).is_err());
        // truncated section
        let mut raw = METADATA_MAGIC.to_le_bytes().to_vec();
        raw.extend_from_slice(&50u32.to_le_bytes());
        assert!(parse_raw(&raw).is_err());
        // no end marker
        assert!(parse_raw(&[]).is_err());
        assert!(parse_raw(&LEGACY_MAGIC.to_le_bytes()).is_err());
    }
}
