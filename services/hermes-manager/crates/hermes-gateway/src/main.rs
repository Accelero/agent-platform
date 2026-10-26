//! `hermes-gateway --config <file>`.

use hermes_core::app::error::AppError;
use hermes_gateway::config::Config;

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        // Bounded codes only: never paths, URLs or provider responses.
        eprintln!("hermes-gateway: {}", error.code);
        std::process::exit(1);
    }
}

async fn run() -> Result<(), AppError> {
    let mut args = std::env::args().skip(1);
    let path = match (args.next().as_deref(), args.next(), args.next()) {
        (Some("--config"), Some(path), None) => path,
        _ => return Err(AppError::invalid()),
    };
    let config = Config::load(std::path::Path::new(&path)).await?;
    hermes_gateway::run(config).await
}
