//! TACT binary formats: BLTE containers, the encoding file and CDN archive indices.

use std::collections::HashMap;
use std::io::Read;

use anyhow::{Result, bail, ensure};
use flate2::read::ZlibDecoder;

fn be(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0, |a, b| (a << 8) | u64::from(*b))
}

fn slice(data: &[u8], start: usize, len: usize) -> Result<&[u8]> {
    start
        .checked_add(len)
        .and_then(|end| data.get(start..end))
        .ok_or_else(|| anyhow::anyhow!("truncated data at offset {start} (+{len})"))
}

fn decode_chunk(chunk: &[u8], out: &mut Vec<u8>) -> Result<()> {
    let Some((&mode, body)) = chunk.split_first() else {
        bail!("empty BLTE chunk");
    };
    match mode {
        b'N' => out.extend_from_slice(body),
        b'Z' => {
            ZlibDecoder::new(body).read_to_end(out)?;
        }
        m => bail!("unsupported BLTE chunk mode {:?}", m as char),
    }
    Ok(())
}

/// Decodes a BLTE container (chunk modes `N` and `Z`). Chunk MD5s are not verified.
pub fn decode_blte(data: &[u8]) -> Result<Vec<u8>> {
    ensure!(slice(data, 0, 4)? == b"BLTE", "not a BLTE container");
    let header_size = be(slice(data, 4, 4)?) as usize;
    let mut out = Vec::new();
    if header_size == 0 {
        decode_chunk(&data[8..], &mut out)?;
        return Ok(out);
    }
    let table = slice(data, 8, 4)?;
    let count = be(&table[1..4]) as usize;
    ensure!(
        header_size == 12 + count * 24,
        "BLTE header size {header_size} does not match {count} chunks"
    );
    let mut pos = header_size;
    for i in 0..count {
        let compressed = be(slice(data, 12 + i * 24, 4)?) as usize;
        decode_chunk(slice(data, pos, compressed)?, &mut out)?;
        pos += compressed;
    }
    Ok(out)
}

/// Layout of an (already BLTE-decoded) encoding file, from its 22-byte header.
struct EncodingLayout {
    ckey_size: usize,
    ekey_size: usize,
    ckey_page: usize,
    ekey_page: usize,
    ckey_pages: usize,
    ekey_pages: usize,
    /// Offset of the first CEKey page.
    ckey_start: usize,
}

impl EncodingLayout {
    fn parse(data: &[u8]) -> Result<Self> {
        ensure!(slice(data, 0, 2)? == b"EN", "not an encoding file");
        let h = slice(data, 0, 22)?;
        let (ckey_size, ekey_size) = (h[3] as usize, h[4] as usize);
        let ckey_pages = be(&h[9..13]) as usize;
        let espec_size = be(&h[18..22]) as usize;
        Ok(Self {
            ckey_size,
            ekey_size,
            ckey_page: be(&h[5..7]) as usize * 1024,
            ekey_page: be(&h[7..9]) as usize * 1024,
            ckey_pages,
            ekey_pages: be(&h[13..17]) as usize,
            ckey_start: 22 + espec_size + ckey_pages * (ckey_size + 16),
        })
    }

    /// Offset of the first EKey-spec page (after the CEKey pages and the EKey page index).
    fn ekey_start(&self) -> usize {
        self.ckey_start + self.ckey_pages * self.ckey_page + self.ekey_pages * (self.ekey_size + 16)
    }
}

/// All encoding keys listed in an (already BLTE-decoded) encoding file's CEKey pages.
pub fn encoding_ekeys(data: &[u8]) -> Result<Vec<Vec<u8>>> {
    let l = EncodingLayout::parse(data)?;
    let mut out = Vec::new();
    for p in 0..l.ckey_pages {
        let page = slice(data, l.ckey_start + p * l.ckey_page, l.ckey_page)?;
        let mut pos = 0;
        while let Some(&keys) = page.get(pos) {
            if keys == 0 {
                break;
            }
            pos += 1 + 5 + l.ckey_size;
            for _ in 0..keys {
                out.push(slice(page, pos, l.ekey_size)?.to_vec());
                pos += l.ekey_size;
            }
        }
    }
    Ok(out)
}

/// Encoded (download) size per encoding key, from the encoding file's EKey-spec
/// pages: entries of ekey, espec index (u32) and size (40-bit), zero-padded.
pub fn encoding_sizes(data: &[u8]) -> Result<HashMap<Vec<u8>, u64>> {
    let l = EncodingLayout::parse(data)?;
    let entry = l.ekey_size + 4 + 5;
    let mut out = HashMap::new();
    for p in 0..l.ekey_pages {
        let page = slice(data, l.ekey_start() + p * l.ekey_page, l.ekey_page)?;
        for e in page.chunks_exact(entry) {
            let key = &e[..l.ekey_size];
            if key.iter().all(|&b| b == 0) {
                break;
            }
            out.insert(key.to_vec(), be(&e[l.ekey_size + 4..]));
        }
    }
    Ok(out)
}

/// A parsed CDN archive `.index` file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ArchiveIndex {
    /// Keys of all entries.
    pub keys: Vec<Vec<u8>>,
    /// Size of the archive the index describes (end of its last blob).
    pub archive_size: u64,
}

/// Parses a CDN archive `.index` file (entries: key, size, offset; big-endian).
pub fn parse_index(data: &[u8]) -> Result<ArchiveIndex> {
    // Footer: toc_hash[cs], version, 2 reserved, block_size_kb, offset_bytes, size_bytes,
    // key_size, checksum_size, num_elements (LE u32), footer_hash[cs]. checksum_size
    // is 8 in practice but also locates itself, so probe for a consistent value.
    // Try the usual 8 first: other sizes can match by accident on bytes of the
    // element count, so a candidate must also have version 1 and a sane key size.
    let len = data.len();
    let plausible = |cs: usize| {
        if len < 12 + 2 * cs || data[len - cs - 5] as usize != cs {
            return false;
        }
        let f = &data[len - (12 + 2 * cs)..];
        f[cs] == 1 && (1..=32).contains(&f[cs + 6])
    };
    let Some(cs) = std::iter::once(8).chain(1..=16).find(|&cs| plausible(cs)) else {
        bail!("unrecognised archive index footer");
    };
    let footer = &data[len - (12 + 2 * cs)..];
    let block_size = footer[cs + 3] as usize * 1024;
    let (offset_bytes, size_bytes, key_size) = (
        footer[cs + 4] as usize,
        footer[cs + 5] as usize,
        footer[cs + 6] as usize,
    );
    let count = u32::from_le_bytes(footer[cs + 8..cs + 12].try_into()?) as usize;
    let entry = key_size + size_bytes + offset_bytes;
    ensure!(entry > 0 && block_size >= entry, "invalid index geometry");
    let per_block = block_size / entry;
    let mut out = ArchiveIndex {
        keys: Vec::with_capacity(count.min(1 << 20)),
        archive_size: 0,
    };
    for i in 0..count {
        let start = (i / per_block) * block_size + (i % per_block) * entry;
        let e = slice(data, start, entry)?;
        let size = be(&e[key_size..key_size + size_bytes]);
        let offset = be(&e[key_size + size_bytes..]);
        out.keys.push(e[..key_size].to_vec());
        out.archive_size = out.archive_size.max(offset + size);
    }
    Ok(out)
}

#[cfg(test)]
pub mod testing {
    //! Synthetic builders for the formats above.
    use std::io::Write;

    use flate2::Compression;
    use flate2::write::ZlibEncoder;

    pub fn zlib(data: &[u8]) -> Vec<u8> {
        let mut e = ZlibEncoder::new(Vec::new(), Compression::default());
        e.write_all(data).unwrap();
        e.finish().unwrap()
    }

    /// Chunks are `(mode, decoded)`; mode `N` stores raw, `Z` zlib.
    pub fn blte(chunks: &[(u8, &[u8])]) -> Vec<u8> {
        let bodies: Vec<Vec<u8>> = chunks
            .iter()
            .map(|(m, d)| {
                let mut v = vec![*m];
                v.extend(if *m == b'Z' { zlib(d) } else { d.to_vec() });
                v
            })
            .collect();
        let mut out = b"BLTE".to_vec();
        out.extend(((12 + chunks.len() * 24) as u32).to_be_bytes());
        out.push(0x0F);
        out.extend(&(chunks.len() as u32).to_be_bytes()[1..]);
        for (b, (_, d)) in bodies.iter().zip(chunks) {
            out.extend((b.len() as u32).to_be_bytes());
            out.extend((d.len() as u32).to_be_bytes());
            out.extend([0u8; 16]);
        }
        bodies.into_iter().for_each(|b| out.extend(b));
        out
    }

    /// Single-chunk BLTE with a zero header size.
    pub fn blte_simple(mode: u8, decoded: &[u8]) -> Vec<u8> {
        let mut out = b"BLTE".to_vec();
        out.extend(0u32.to_be_bytes());
        out.push(mode);
        out.extend(if mode == b'Z' {
            zlib(decoded)
        } else {
            decoded.to_vec()
        });
        out
    }

    pub type CeEntry = ([u8; 16], Vec<[u8; 16]>);

    /// Encoding file with one 1 KiB CEKey page per group; entries are `(ckey, ekeys)`.
    pub fn encoding(pages: &[Vec<CeEntry>]) -> Vec<u8> {
        let page_kb = 1u16;
        let mut out = b"EN".to_vec();
        out.extend([1, 16, 16]);
        out.extend(page_kb.to_be_bytes());
        out.extend(page_kb.to_be_bytes());
        out.extend((pages.len() as u32).to_be_bytes());
        out.extend(1u32.to_be_bytes()); // one EKey-spec page
        out.push(0);
        let espec = b"z\0";
        out.extend((espec.len() as u32).to_be_bytes());
        out.extend(espec);
        for p in pages {
            out.extend(p[0].0);
            out.extend([0u8; 16]);
        }
        for p in pages {
            let mut page = Vec::new();
            for (ckey, ekeys) in p {
                page.push(ekeys.len() as u8);
                page.extend([0, 0, 0, 0, 1]);
                page.extend(ckey);
                ekeys.iter().for_each(|e| page.extend(e));
            }
            page.resize(page_kb as usize * 1024, 0);
            out.extend(page);
        }
        // EKey page index (unused by the parser), then one page: every ekey with
        // encoded size 0x100 + its first byte.
        out.extend([0u8; 32]);
        let mut page = Vec::new();
        for (_, ekeys) in pages.iter().flatten() {
            for e in ekeys {
                page.extend(e);
                page.extend(0u32.to_be_bytes());
                page.extend((0x100 + u64::from(e[0])).to_be_bytes()[3..].iter());
            }
        }
        page.resize(page_kb as usize * 1024, 0);
        out.extend(page);
        out
    }

    /// Archive index with 4 KiB blocks, 16-byte keys, 4-byte sizes and offsets:
    /// each blob is 100 bytes, laid out back to back.
    pub fn index(keys: &[[u8; 16]]) -> Vec<u8> {
        let per_block = 4096 / 24;
        let mut out = Vec::new();
        let mut offset = 0u32;
        for chunk in keys.chunks(per_block) {
            let mut block = Vec::new();
            for k in chunk {
                block.extend(k);
                block.extend(100u32.to_be_bytes());
                block.extend(offset.to_be_bytes());
                offset += 100;
            }
            block.resize(4096, 0);
            out.extend(block);
        }
        out.extend([0u8; 8]); // toc hash (unused by the parser)
        out.extend([1, 0, 0, 4, 4, 4, 16, 8]);
        out.extend((keys.len() as u32).to_le_bytes());
        out.extend([0u8; 8]);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;

    #[test]
    fn blte_chunks_and_simple_form() {
        let multi = blte(&[(b'N', b"hello "), (b'Z', b"world"), (b'Z', b"!")]);
        assert_eq!(decode_blte(&multi).unwrap(), b"hello world!");
        assert_eq!(decode_blte(&blte_simple(b'N', b"raw")).unwrap(), b"raw");
        assert_eq!(
            decode_blte(&blte_simple(b'Z', b"zipped")).unwrap(),
            b"zipped"
        );
    }

    #[test]
    fn blte_errors() {
        assert!(decode_blte(b"XXXX\0\0\0\0N").is_err());
        assert!(decode_blte(b"BLT").is_err());
        let e = decode_blte(&blte(&[(b'E', b"x")])).unwrap_err();
        assert!(e.to_string().contains("mode"), "{e}");
        let mut truncated = blte(&[(b'N', b"hello")]);
        truncated.truncate(truncated.len() - 2);
        assert!(decode_blte(&truncated).is_err());
    }

    #[test]
    fn encoding_lists_all_ekeys() {
        let k = |n: u8| [n; 16];
        let data = encoding(&[
            vec![(k(1), vec![k(0xa1)]), (k(2), vec![k(0xa2), k(0xa3)])],
            vec![(k(3), vec![k(0xa4)])],
        ]);
        assert_eq!(
            encoding_ekeys(&data).unwrap(),
            vec![
                k(0xa1).to_vec(),
                k(0xa2).to_vec(),
                k(0xa3).to_vec(),
                k(0xa4).to_vec()
            ]
        );
        assert!(encoding_ekeys(b"XX").is_err());
        assert!(encoding_ekeys(&data[..data.len() - 2500]).is_err());
    }

    #[test]
    fn encoding_sizes_from_ekey_pages() {
        let k = |n: u8| [n; 16];
        let data = encoding(&[vec![(k(1), vec![k(0xa1), k(0xa2)])]]);
        let sizes = encoding_sizes(&data).unwrap();
        assert_eq!(sizes.len(), 2);
        assert_eq!(sizes[&k(0xa1).to_vec()], 0x1a1);
        assert_eq!(sizes[&k(0xa2).to_vec()], 0x1a2);
        assert!(encoding_sizes(b"XX").is_err());
        assert!(encoding_sizes(&data[..data.len() - 100]).is_err());
    }

    #[test]
    fn index_keys_across_blocks() {
        let keys: Vec<[u8; 16]> = (0..400u16)
            .map(|i| {
                let mut k = [0u8; 16];
                k[..2].copy_from_slice(&i.to_be_bytes());
                k
            })
            .collect();
        let parsed = parse_index(&index(&keys)).unwrap();
        assert_eq!(parsed.keys.len(), 400);
        assert_eq!(parsed.keys[0], keys[0].to_vec());
        assert_eq!(parsed.keys[399], keys[399].to_vec());
        assert_eq!(parsed.archive_size, 400 * 100);
        assert_eq!(parse_index(&index(&[])).unwrap(), ArchiveIndex::default());
        assert!(parse_index(&[0u8; 10]).is_err());
        let mut bad = index(&keys);
        bad.drain(..4096);
        assert!(parse_index(&bad).is_err());
    }
}

/// Parses real CDN files when `GC_BLIZZARD_SAMPLES` points at a directory holding
/// `encoding.blte` (BLTE-encoded encoding file) and `archive.index`, fetched from a
/// live Blizzard CDN. Run with `cargo test -- --ignored live_samples`.
#[cfg(test)]
mod live {
    use super::*;

    #[test]
    #[ignore]
    fn live_samples() {
        let Ok(dir) = std::env::var("GC_BLIZZARD_SAMPLES") else {
            return;
        };
        let dir = std::path::Path::new(&dir);
        let enc = decode_blte(&std::fs::read(dir.join("encoding.blte")).unwrap()).unwrap();
        assert_eq!(&enc[..2], b"EN", "decoded encoding file has EN magic");
        let ekeys = encoding_ekeys(&enc).unwrap();
        assert!(ekeys.len() > 100, "encoding ekeys: {}", ekeys.len());
        assert!(ekeys.iter().all(|k| k.len() == 16));
        let idx = std::fs::read(dir.join("archive.index")).unwrap();
        let keys = parse_index(&idx).unwrap().keys;
        assert!(!keys.is_empty());
        assert!(keys.iter().all(|k| k.len() == 16));
        // Entries are sorted by key, which checks the block/entry geometry.
        assert!(keys.windows(2).all(|w| w[0] < w[1]), "index keys sorted");
        // Across all of a build's archive indexes (idx/*.index), most encoding
        // ekeys must be archived; archives also hold other builds' content.
        let mut archived = std::collections::HashSet::new();
        if let Ok(entries) = std::fs::read_dir(dir.join("idx")) {
            for e in entries {
                archived.extend(
                    parse_index(&std::fs::read(e.unwrap().path()).unwrap())
                        .unwrap()
                        .keys,
                );
            }
        }
        if !archived.is_empty() {
            let covered = ekeys.iter().filter(|k| archived.contains(*k)).count();
            eprintln!(
                "encoding ekeys {}, archived {}, loose {}",
                ekeys.len(),
                covered,
                ekeys.len() - covered
            );
            for k in ekeys.iter().filter(|k| !archived.contains(*k)).take(5) {
                eprintln!("loose {}", hex::encode(k));
            }
            assert!(
                covered * 2 > ekeys.len(),
                "most encoding ekeys are archived"
            );
        }
    }
}
