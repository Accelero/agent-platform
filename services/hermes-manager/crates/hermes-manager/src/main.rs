//! `hermes-manager`: CLI and configuration for the api, controller and operator roles.
//! Composition only; rules live in `hermes-core`.

mod cli;
mod config;
mod faults;
mod roles;

use hermes_core::app::error::AppError;

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        // Bounded codes only: never paths, URLs, SQL or provider responses.
        eprintln!("hermes-manager: {}", error.code);
        std::process::exit(1);
    }
}

async fn run() -> Result<(), AppError> {
    let command = cli::parse(std::env::args().skip(1))?;
    if let cli::Role::GenerateSealingKey {
        private_key_file,
        public_key_file,
    } = &command.role
    {
        return roles::operator::generate_sealing_key(private_key_file, public_key_file).await;
    }
    let path = command.config.ok_or_else(AppError::invalid)?;
    let config = config::Config::load(&path).await?;
    match command.role {
        cli::Role::Migrate => roles::operator::migrate(&config).await,
        cli::Role::Check => roles::operator::check(&config).await,
        cli::Role::Initialize => roles::operator::initialize(&config).await,
        cli::Role::RebindCluster => roles::operator::rebind_cluster(&config).await,
        cli::Role::GrantAdministrator {
            identity_file,
            recover,
        } => roles::operator::grant_administrator(&config, &identity_file, recover).await,
        cli::Role::Api => roles::api::run(config).await,
        cli::Role::Controller => roles::controller::run(config).await,
        cli::Role::GenerateSealingKey { .. } => Err(AppError::invalid()),
    }
}
