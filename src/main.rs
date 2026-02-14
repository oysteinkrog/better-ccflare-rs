//! better-ccflare binary — CLI dispatch and HTTP server entry point.

use std::process;
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Parser;

use bccf_cli::Cli;
use bccf_core::config::Config;
use bccf_core::init_logging;
use bccf_core::state::AppStateBuilder;
use bccf_database::paths::{ensure_db_dir, resolve_db_path};
use bccf_database::pool::{create_pool, PoolConfig};
use bccf_database::schema;

fn main() {
    let cli = Cli::parse();
    init_logging();

    if let Err(e) = run(cli) {
        eprintln!("Error: {e:#}");
        process::exit(1);
    }
}

#[tokio::main]
async fn run(cli: Cli) -> Result<()> {
    // Load dotenv files
    bccf_core::config::load_dotenv();

    // Load config
    let mut config = Config::load(None).context("Failed to load config")?;

    // Initialize database
    let db_path = resolve_db_path();
    ensure_db_dir(&db_path)?;

    let pool =
        create_pool(&db_path, &PoolConfig::default()).context("Failed to create database pool")?;

    // Ensure schema exists
    {
        let conn = pool.get().context("Failed to get database connection")?;
        schema::create_tables(&conn)?;
        schema::create_indexes(&conn)?;
    }

    // Dispatch CLI commands
    let handled = bccf_cli::commands::run(&cli, &pool, &mut config)?;
    if handled {
        return Ok(());
    }

    // No command matched — start the HTTP server
    tracing::info!("Starting better-ccflare server");

    let state = Arc::new(AppStateBuilder::new(config).db_pool(pool).build());

    let mut server_config = bccf_proxy::server::ServerConfig::from_env(&state);

    // CLI --port override
    if let Some(port) = cli.port {
        server_config.port = port;
    }

    // CLI --ssl-key / --ssl-cert override
    if let (Some(ref key), Some(ref cert)) = (&cli.ssl_key, &cli.ssl_cert) {
        server_config.tls_enabled = true;
        server_config.tls_key_path = Some(key.clone());
        server_config.tls_cert_path = Some(cert.clone());
    }

    let coordinator = bccf_proxy::shutdown::ShutdownCoordinator::new();
    bccf_proxy::server::start(state, server_config, coordinator).await?;

    Ok(())
}
