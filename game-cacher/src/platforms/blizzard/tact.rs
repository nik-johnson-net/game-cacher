//! TACT/NGDP text formats: BPSV tables (`versions`, `cdns`) and `key = value` configs.

use std::collections::HashMap;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Bpsv {
    pub seqn: Option<u64>,
    pub rows: Vec<HashMap<String, String>>,
}

/// Parses a BPSV document: `Name!TYPE:len|...` header, `## seqn = N` comments, data rows.
pub fn parse_bpsv(text: &str) -> Bpsv {
    let mut out = Bpsv::default();
    let mut header: Option<Vec<String>> = None;
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        if let Some(comment) = line.strip_prefix('#') {
            let comment = comment.trim_start_matches('#').trim();
            if let Some(v) = comment.strip_prefix("seqn") {
                out.seqn = v.trim_start_matches([' ', '=']).trim().parse().ok();
            }
            continue;
        }
        let cols = line.split('|');
        match &header {
            None => {
                header = Some(
                    cols.map(|c| c.split('!').next().unwrap_or_default().to_string())
                        .collect(),
                );
            }
            Some(h) => {
                out.rows
                    .push(h.iter().cloned().zip(cols.map(str::to_string)).collect());
            }
        }
    }
    out
}

impl Bpsv {
    /// Row for `region`, falling back to the first row.
    pub fn region(&self, region: &str) -> Option<&HashMap<String, String>> {
        self.rows
            .iter()
            .find(|r| r.get("Region").is_some_and(|v| v == region))
            .or_else(|| self.rows.first())
    }
}

/// Parses a TACT config (`key = value` lines, `#` comments).
pub fn parse_config(text: &str) -> HashMap<String, String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect()
}

/// Whitespace-separated values of a config key.
pub fn values<'a>(cfg: &'a HashMap<String, String>, key: &str) -> Vec<&'a str> {
    cfg.get(key)
        .map(|v| v.split_whitespace().collect())
        .unwrap_or_default()
}

/// `/{path}/{kind}/ab/cd/{hash}{suffix}`; None when the hash is too short.
pub fn cdn_path(path: &str, kind: &str, hash: &str, suffix: &str) -> Option<String> {
    if hash.len() < 4 || !hash.is_ascii() {
        return None;
    }
    Some(format!(
        "/{}/{kind}/{}/{}/{hash}{suffix}",
        path.trim_matches('/'),
        &hash[0..2],
        &hash[2..4]
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const VERSIONS: &str = "Region!STRING:0|BuildConfig!HEX:16|CDNConfig!HEX:16|BuildId!DEC:4|VersionsName!String:0\n## seqn = 3021\nus|aabbccdd11|eeff001122|55|10.2.5.1\neu|bb|cc|55|10.2.5.2\n";

    #[test]
    fn bpsv_regions_and_seqn() {
        let b = parse_bpsv(VERSIONS);
        assert_eq!(b.seqn, Some(3021));
        assert_eq!(b.rows.len(), 2);
        assert_eq!(b.region("eu").unwrap()["VersionsName"], "10.2.5.2");
        assert_eq!(b.region("us").unwrap()["BuildConfig"], "aabbccdd11");
        // Unknown region falls back to the first row.
        assert_eq!(b.region("kr").unwrap()["Region"], "us");
        assert!(parse_bpsv("").region("us").is_none());
    }

    #[test]
    fn config_parsing() {
        let c =
            parse_config("# Build\nroot = abc\nencoding = c1 e1\n\narchives = a b  c\nempty =\n");
        assert_eq!(c["root"], "abc");
        assert_eq!(values(&c, "encoding"), vec!["c1", "e1"]);
        assert_eq!(values(&c, "archives"), vec!["a", "b", "c"]);
        assert!(values(&c, "empty").is_empty());
        assert!(values(&c, "missing").is_empty());
    }

    #[test]
    fn paths() {
        assert_eq!(
            cdn_path("tpr/wow", "data", "aabbcc", ".index").unwrap(),
            "/tpr/wow/data/aa/bb/aabbcc.index"
        );
        assert!(cdn_path("tpr/wow", "data", "ab", "").is_none());
    }
}
