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
use bccf_proxy::auto_refresh::{AutoRefreshScheduler, RefreshAccountSource, RefreshableAccount};
use bccf_proxy::token_health::{TokenHealthService, HEALTH_CHECK_INTERVAL_MS};
use bccf_providers::usage_polling::{AccountSource, PollableAccount, UsageCache, UsagePollingService};
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

    // Shared HTTP client — reuses TCP connections and TLS sessions across requests
    let http_client = reqwest::Client::builder()
        .pool_max_idle_per_host(20)
        .timeout(std::time::Duration::from_secs(300))
        .connect_timeout(std::time::Duration::from_secs(10))
        .build()
        .expect("Failed to create HTTP client");

    // Start usage polling service (fetches utilization from provider APIs)
    let usage_cache = UsageCache::new();
    let account_source = Arc::new(DbAccountSource { pool: pool.clone() });
    let _usage_polling = UsagePollingService::start(
        account_source,
        usage_cache.clone(),
        http_client.clone(),
    );

    // Clone pool before moving into AppState (needed for background services)
    let bg_pool = pool.clone();

    let state = Arc::new(
        AppStateBuilder::new(config)
            .db_pool(pool)
            .provider_registry(registry)
            .load_balancer(load_balancer)
            .token_manager(token_manager)
            .async_writer(post_processor)
            .usage_cache(usage_cache)
            .http_client(http_client)
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

    // Start auto-refresh scheduler (sends dummy requests to keep OAuth usage windows fresh)
    let refresh_source = Arc::new(DbRefreshSource { pool: bg_pool.clone() });
    let auto_refresh = AutoRefreshScheduler::start(
        refresh_source,
        server_config.port,
        server_config.tls_enabled,
    );

    // Start token health monitoring service (periodic health checks every 6h)
    let health_pool = bg_pool.clone();
    let token_health_svc = TokenHealthService::start(
        move || {
            let Ok(conn) = health_pool.get() else {
                return Vec::new();
            };
            bccf_database::repositories::account::find_all(&conn).unwrap_or_default()
        },
        HEALTH_CHECK_INTERVAL_MS,
    );

    // Register shutdown steps for background services
    let mut coordinator = bccf_proxy::shutdown::ShutdownCoordinator::new();
    coordinator.register("auto-refresh scheduler", move || async move {
        auto_refresh.shutdown().await;
    });
    coordinator.register("token health service", move || async move {
        token_health_svc.stop();
    });

    bccf_proxy::server::start(state, server_config, coordinator).await?;

    Ok(())
}

/// Database-backed account source for auto-refresh scheduling.
struct DbRefreshSource {
    pool: bccf_database::DbPool,
}

impl RefreshAccountSource for DbRefreshSource {
    fn get_refreshable_accounts(&self) -> Vec<RefreshableAccount> {
        let Ok(conn) = self.pool.get() else {
            return Vec::new();
        };
        // Query accounts with auto_refresh_enabled and anthropic/claude-oauth provider
        let accounts = match conn.prepare(
            "SELECT id, name, rate_limit_reset FROM accounts \
             WHERE auto_refresh_enabled = 1 \
             AND provider IN ('anthropic', 'claude-oauth') \
             AND paused = 0",
        ) {
            Ok(mut stmt) => stmt
                .query_map([], |row| {
                    Ok(RefreshableAccount {
                        id: row.get(0)?,
                        name: row.get(1)?,
                        rate_limit_reset: row.get(2)?,
                    })
                })
                .map(|rows| rows.filter_map(|r| r.ok()).collect())
                .unwrap_or_default(),
            Err(_) => Vec::new(),
        };
        accounts
    }

    fn get_active_refresh_account_ids(&self) -> Vec<String> {
        let Ok(conn) = self.pool.get() else {
            return Vec::new();
        };
        let mut stmt = match conn.prepare(
            "SELECT id FROM accounts \
             WHERE auto_refresh_enabled = 1 \
             AND provider IN ('anthropic', 'claude-oauth')",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        stmt.query_map([], |row| row.get::<_, String>(0))
            .map(|rows| rows.filter_map(|r| r.ok()).collect())
            .unwrap_or_default()
    }
}

/// Database-backed account source for usage polling.
struct DbAccountSource {
    pool: bccf_database::DbPool,
}

impl AccountSource for DbAccountSource {
    fn get_pollable_accounts(&self) -> Vec<PollableAccount> {
        let Ok(conn) = self.pool.get() else {
            return Vec::new();
        };
        let accounts = bccf_database::repositories::account::find_all(&conn).unwrap_or_default();
        accounts
            .into_iter()
            .filter(|a| !a.paused)
            .filter(|a| supports_usage_tracking(&a.provider))
            .map(|a| PollableAccount {
                id: a.id,
                provider: a.provider,
                access_token: a.access_token,
                api_key: a.api_key,
                custom_endpoint: a.custom_endpoint,
                paused: a.paused,
            })
            .collect()
    }
}

/// Check if a provider supports usage tracking.
fn supports_usage_tracking(provider: &str) -> bool {
    matches!(provider, "claude-oauth" | "anthropic" | "nanogpt" | "zai")
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
