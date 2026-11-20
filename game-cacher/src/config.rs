use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;

use clap::{Parser, ValueEnum};

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum StorageKind {
    Disk,
    S3,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum SecretsKind {
    Storage,
    Vault,
}

/// Runtime configuration. Every flag has an env var; credentials are env-only
/// (read in `Config::from_env_secrets`) so they never appear in `ps` output.
#[derive(Debug, Clone, Parser)]
#[command(
    name = "game-cacher",
    version,
    about = "Caching proxy for game distribution platforms"
)]
pub struct Config {
    #[arg(long, env = "GC_HTTP_LISTEN", default_value = "0.0.0.0:80")]
    pub http_listen: SocketAddr,
    #[arg(long, env = "GC_HTTPS_LISTEN", default_value = "0.0.0.0:443")]
    pub https_listen: SocketAddr,
    #[arg(long, env = "GC_DNS_LISTEN", default_value = "0.0.0.0:53")]
    pub dns_listen: SocketAddr,
    #[arg(long, env = "GC_ADMIN_LISTEN", default_value = "0.0.0.0:8080")]
    pub admin_listen: SocketAddr,

    #[arg(long, env = "GC_DISABLE_DNS", default_value_t = false)]
    pub disable_dns: bool,
    #[arg(long, env = "GC_DISABLE_SNI", default_value_t = false)]
    pub disable_sni: bool,

    /// IPs returned for overridden domains (this server's LAN address).
    #[arg(long, env = "GC_CACHE_IP", value_delimiter = ',')]
    pub cache_ip: Vec<IpAddr>,
    /// Real DNS servers used to resolve upstream CDNs (never our own DNS).
    #[arg(
        long,
        env = "GC_UPSTREAM_DNS",
        value_delimiter = ',',
        default_value = "1.1.1.1,8.8.8.8"
    )]
    pub upstream_dns: Vec<SocketAddrOrIp>,
    /// DNS servers to forward non-overridden queries to. Unset → REFUSED.
    #[arg(long, env = "GC_DNS_FORWARD", value_delimiter = ',')]
    pub dns_forward: Vec<SocketAddrOrIp>,

    #[arg(long, env = "GC_STORAGE", value_enum, default_value = "disk")]
    pub storage: StorageKind,
    #[arg(long, env = "GC_STORAGE_PATH", default_value = "/data")]
    pub storage_path: PathBuf,
    #[arg(long, env = "GC_S3_BUCKET")]
    pub s3_bucket: Option<String>,
    #[arg(long, env = "GC_S3_ENDPOINT")]
    pub s3_endpoint: Option<String>,
    #[arg(long, env = "GC_S3_REGION", default_value = "us-east-1")]
    pub s3_region: String,
    #[arg(long, env = "GC_S3_PREFIX", default_value = "")]
    pub s3_prefix: String,
    #[arg(long, env = "GC_S3_PATH_STYLE", default_value_t = false)]
    pub s3_path_style: bool,

    #[arg(long, env = "GC_SECRETS", value_enum, default_value = "storage")]
    pub secrets: SecretsKind,
    #[arg(long, env = "GC_VAULT_ADDR")]
    pub vault_addr: Option<String>,
    #[arg(long, env = "GC_VAULT_MOUNT", default_value = "secret")]
    pub vault_mount: String,
    #[arg(long, env = "GC_VAULT_PATH", default_value = "game-cacher")]
    pub vault_path: String,

    /// Maximum cache size, e.g. `2TB`, `500GiB`. Unset → unbounded.
    #[arg(long, env = "GC_CACHE_MAX_SIZE", value_parser = parse_size)]
    pub cache_max_size: Option<u64>,
    #[arg(long, env = "GC_SLICE_SIZE", value_parser = parse_size, default_value = "1MiB")]
    pub slice_size: u64,
    /// Slices fetched in parallel per client download (1 = one at a time).
    #[arg(long, env = "GC_READ_AHEAD", default_value_t = 4)]
    pub read_ahead: usize,

    #[arg(long, env = "GC_UI_PATH", default_value = "/opt/game-cacher/ui")]
    pub ui_path: PathBuf,

    #[arg(long, env = "GC_DISABLE_AUTH", default_value_t = false)]
    pub disable_auth: bool,

    /// Identifies this instance in the loop-detection header. Defaults to hostname.
    #[arg(long, env = "GC_INSTANCE_ID")]
    pub instance_id: Option<String>,

    /// Allow upstream CDN hosts that resolve to private/loopback addresses
    /// (e.g. a local mirror). Off by default so the proxy can't reach into the LAN.
    #[arg(long, env = "GC_ALLOW_PRIVATE_UPSTREAM", default_value_t = false)]
    pub allow_private_upstream: bool,

    /// Port used for upstream HTTP fetches. Only changed by tests.
    #[arg(long, env = "GC_UPSTREAM_HTTP_PORT", default_value_t = 80, hide = true)]
    pub upstream_http_port: u16,
}

/// Env-only credentials.
#[derive(Debug, Clone, Default)]
pub struct EnvSecrets {
    pub admin_password: Option<String>,
    pub secrets_key: Option<String>,
    pub vault_token: Option<String>,
    pub vault_role_id: Option<String>,
    pub vault_secret_id: Option<String>,
}

impl EnvSecrets {
    pub fn from_env() -> Self {
        let get = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        Self {
            admin_password: get("GC_ADMIN_PASSWORD"),
            secrets_key: get("GC_SECRETS_KEY"),
            vault_token: get("VAULT_TOKEN"),
            vault_role_id: get("VAULT_ROLE_ID"),
            vault_secret_id: get("VAULT_SECRET_ID"),
        }
    }
}

impl Config {
    pub fn instance_id(&self) -> String {
        self.instance_id.clone().unwrap_or_else(|| {
            hostname::get()
                .ok()
                .and_then(|h| h.into_string().ok())
                .unwrap_or_else(|| "game-cacher".to_string())
        })
    }

    /// Cross-field validation clap can't express.
    pub fn validate(&self, env: &EnvSecrets) -> anyhow::Result<()> {
        if !self.disable_dns && self.cache_ip.is_empty() {
            anyhow::bail!("--cache-ip / GC_CACHE_IP is required when DNS is enabled");
        }
        if self.storage == StorageKind::S3 && self.s3_bucket.is_none() {
            anyhow::bail!("--s3-bucket / GC_S3_BUCKET is required with --storage s3");
        }
        match self.secrets {
            SecretsKind::Storage if env.secrets_key.is_none() => {
                anyhow::bail!("GC_SECRETS_KEY is required with --secrets storage")
            }
            SecretsKind::Vault if self.vault_addr.is_none() => {
                anyhow::bail!("--vault-addr / GC_VAULT_ADDR is required with --secrets vault")
            }
            SecretsKind::Vault
                if env.vault_token.is_none()
                    && (env.vault_role_id.is_none() || env.vault_secret_id.is_none()) =>
            {
                anyhow::bail!("Vault needs VAULT_TOKEN or VAULT_ROLE_ID + VAULT_SECRET_ID")
            }
            _ => {}
        }
        if !self.disable_auth && env.admin_password.is_none() {
            anyhow::bail!("GC_ADMIN_PASSWORD is required unless --disable-auth is set");
        }
        if self.slice_size < 64 * 1024 || self.slice_size > 64 * 1024 * 1024 {
            anyhow::bail!("--slice-size must be between 64KiB and 64MiB");
        }
        if !(1..=32).contains(&self.read_ahead) {
            anyhow::bail!("--read-ahead must be between 1 and 32");
        }
        Ok(())
    }
}

/// A DNS server given as `ip` (port 53 implied) or `ip:port`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SocketAddrOrIp(pub SocketAddr);

impl std::str::FromStr for SocketAddrOrIp {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        if let Ok(addr) = s.parse::<SocketAddr>() {
            return Ok(Self(addr));
        }
        s.parse::<IpAddr>()
            .map(|ip| Self(SocketAddr::new(ip, 53)))
            .map_err(|_| format!("invalid DNS server address: {s}"))
    }
}

/// Parses `1024`, `512KiB`, `1MiB`, `2GB`, `1.5TB` (SI for kB/MB/GB/TB, binary for KiB/MiB/...).
pub fn parse_size(s: &str) -> Result<u64, String> {
    let s = s.trim();
    let split = s
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(s.len());
    let (num, unit) = s.split_at(split);
    let num: f64 = num.parse().map_err(|_| format!("invalid size: {s}"))?;
    let mult: f64 = match unit.trim().to_ascii_lowercase().as_str() {
        "" | "b" => 1.0,
        "k" | "kb" => 1e3,
        "m" | "mb" => 1e6,
        "g" | "gb" => 1e9,
        "t" | "tb" => 1e12,
        "kib" => 1024.0,
        "mib" => 1024.0 * 1024.0,
        "gib" => 1024.0 * 1024.0 * 1024.0,
        "tib" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
        other => return Err(format!("unknown size unit: {other}")),
    };
    if num < 0.0 {
        return Err(format!("invalid size: {s}"));
    }
    Ok((num * mult) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(parse_size("1024").unwrap(), 1024);
        assert_eq!(parse_size("1MiB").unwrap(), 1 << 20);
        assert_eq!(parse_size("2TB").unwrap(), 2_000_000_000_000);
        assert_eq!(parse_size("1.5 GiB").unwrap(), 3 << 29);
        assert!(parse_size("5 parsecs").is_err());
        assert!(parse_size("abc").is_err());
    }

    #[test]
    fn dns_addr() {
        assert_eq!(
            "1.1.1.1".parse::<SocketAddrOrIp>().unwrap().0,
            "1.1.1.1:53".parse().unwrap()
        );
        assert_eq!(
            "127.0.0.1:5353".parse::<SocketAddrOrIp>().unwrap().0.port(),
            5353
        );
        assert!("nope".parse::<SocketAddrOrIp>().is_err());
    }

    fn base() -> Config {
        Config::parse_from(["game-cacher", "--cache-ip", "10.0.0.2"])
    }

    #[test]
    fn validation() {
        let env = EnvSecrets {
            admin_password: Some("pw".into()),
            secrets_key: Some("k".into()),
            ..Default::default()
        };
        base().validate(&env).unwrap();
        let mut c = base();
        c.cache_ip.clear();
        assert!(c.validate(&env).is_err());
        assert!(base().validate(&EnvSecrets::default()).is_err());
        let mut c = base();
        c.secrets = SecretsKind::Vault;
        c.vault_addr = Some("http://vault:8200".into());
        assert!(c.validate(&env).is_err(), "vault without token");
        for (n, ok) in [(0, false), (1, true), (32, true), (33, false)] {
            let mut c = base();
            c.read_ahead = n;
            assert_eq!(c.validate(&env).is_ok(), ok, "read_ahead {n}");
        }
    }
}
