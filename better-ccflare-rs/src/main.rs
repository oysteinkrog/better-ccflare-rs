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
use bccf_providers::ProviderRegistry;

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

    // Build provider registry
    let registry = create_provider_registry();

    // Build load balancer
    let session_duration_ms = config.get_runtime().session_duration_ms;
    let load_balancer = bccf_load_balancer::SessionStrategy::new(session_duration_ms);

    // Build token manager for OAuth token refresh
    let client_id = config.get_runtime().client_id.clone();
    let token_manager = bccf_proxy::token_manager::TokenManager::new(client_id);

    // Spawn post-processor for request analytics recording
    let db_receiver = bccf_proxy::post_processor::DbSummaryReceiver::new(pool.clone());
    let post_processor = bccf_proxy::post_processor::spawn_post_processor(db_receiver);

    let state = Arc::new(
        AppStateBuilder::new(config)
            .db_pool(pool)
            .provider_registry(registry)
            .load_balancer(load_balancer)
            .token_manager(token_manager)
            .async_writer(post_processor)
            .build(),
    );

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

/// Create a provider registry with all known provider implementations.
fn create_provider_registry() -> ProviderRegistry {
    use std::sync::Arc;
    use bccf_providers::impls::*;
    use bccf_providers::impls::anthropic_compatible::AnthropicCompatibleConfig;
    use bccf_providers::impls::openai_compatible::OpenAiCompatibleConfig;

    let mut registry = ProviderRegistry::new();

    // Claude OAuth (Anthropic OAuth flow)
    // Registered as both "claude-oauth" and "anthropic" for TS DB compatibility
    let claude_oauth = Arc::new(ClaudeOAuthProvider::claude_oauth());
    registry.register_oauth(claude_oauth.clone());
    registry.register_as("anthropic", claude_oauth);

    // Console (API key via OAuth create_api_key endpoint)
    let console = Arc::new(ClaudeOAuthProvider::console());
    registry.register_oauth(console);

    // Zai (Zhipu AI — Anthropic-compatible)
    registry.register(Arc::new(ZaiProvider::new()));

    // Minimax (Anthropic-compatible)
    registry.register(Arc::new(MinimaxProvider::new()));

    // NanoGPT
    registry.register(Arc::new(NanoGptProvider::new()));

    // Anthropic-compatible (generic)
    registry.register(Arc::new(AnthropicCompatibleProvider::new(
        AnthropicCompatibleConfig::default(),
    )));

    // OpenAI-compatible
    registry.register(Arc::new(OpenAiCompatibleProvider::new(
        OpenAiCompatibleConfig::default(),
    )));

    // Vertex AI
    registry.register(Arc::new(VertexAiProvider::new()));

    tracing::info!(
        providers = ?registry.list(),
        "Provider registry initialized"
    );
    registry
}
