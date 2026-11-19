use clap::Parser;
use env_logger::Env;
use game_cacher::config::{Config, EnvSecrets};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::init_from_env(Env::default().filter_or("LOG", "info"));
    // Both ring and aws-lc-rs end up linked (steam-vent), so rustls can't pick one itself.
    let _ = rustls::crypto::ring::default_provider().install_default();
    let config = Config::parse();
    let env = EnvSecrets::from_env();
    config.validate(&env)?;
    game_cacher::app::run(config, env).await
}
