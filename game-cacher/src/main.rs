use clap::Parser;
use env_logger::Env;
use game_cacher::config::{Config, EnvSecrets};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::init_from_env(Env::default().filter_or("LOG", "info"));
    let config = Config::parse();
    let env = EnvSecrets::from_env();
    config.validate(&env)?;
    game_cacher::app::run(config, env).await
}
