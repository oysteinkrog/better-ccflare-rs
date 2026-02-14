//! CLI argument definitions using clap derive.
//!
//! Flag names match the TypeScript CLI for backwards compatibility.

use clap::Parser;

/// Valid provider modes for account creation.
pub const VALID_MODES: &[&str] = &[
    "claude-oauth",
    "console",
    "zai",
    "minimax",
    "nanogpt",
    "anthropic-compatible",
    "openai-compatible",
    "vertex-ai",
];

/// Load balancer proxy for Claude — distributes requests across multiple
/// account providers to avoid rate limiting.
#[derive(Parser, Debug)]
#[command(
    name = "better-ccflare",
    version,
    about = "Load balancer proxy for Claude"
)]
pub struct Cli {
    /// Enable verbose logging
    #[arg(short, long)]
    pub verbose: bool,

    // -- Account management --------------------------------------------------
    /// Add a new account
    #[arg(long = "add-account", value_name = "NAME")]
    pub add_account: Option<String>,

    /// Provider type for the new account
    #[arg(long, value_name = "PROVIDER")]
    pub mode: Option<String>,

    /// Priority for the new account (lower = higher priority, 0 = first)
    #[arg(long, value_name = "NUMBER")]
    pub priority: Option<i64>,

    /// Remove an account
    #[arg(long, value_name = "NAME")]
    pub remove: Option<String>,

    /// List all accounts
    #[arg(long)]
    pub list: bool,

    /// Pause an account
    #[arg(long, value_name = "NAME")]
    pub pause: Option<String>,

    /// Resume an account
    #[arg(long, value_name = "NAME")]
    pub resume: Option<String>,

    /// Set account priority (format: NAME PRIORITY)
    #[arg(long = "set-priority", num_args = 2, value_names = ["NAME", "PRIORITY"])]
    pub set_priority: Option<Vec<String>>,

    /// Re-authenticate an account (preserves metadata)
    #[arg(long, value_name = "NAME")]
    pub reauthenticate: Option<String>,

    // -- Server mode ---------------------------------------------------------
    /// Start API server with dashboard
    #[arg(long)]
    pub serve: bool,

    /// Server port (default: 8080, or PORT/BETTER_CCFLARE_PORT env var)
    #[arg(long, value_name = "PORT")]
    pub port: Option<u16>,

    /// Path to SSL private key file (enables HTTPS)
    #[arg(long = "ssl-key", value_name = "PATH")]
    pub ssl_key: Option<String>,

    /// Path to SSL certificate file (enables HTTPS)
    #[arg(long = "ssl-cert", value_name = "PATH")]
    pub ssl_cert: Option<String>,

    // -- Stats and maintenance -----------------------------------------------
    /// Show statistics (JSON output)
    #[arg(long)]
    pub stats: bool,

    /// Analyze database performance
    #[arg(long)]
    pub analyze: bool,

    /// Reset usage statistics
    #[arg(long = "reset-stats")]
    pub reset_stats: bool,

    /// Clear request history
    #[arg(long = "clear-history")]
    pub clear_history: bool,

    /// Run database integrity check and optimize
    #[arg(long = "repair-db")]
    pub repair_db: bool,

    // -- API key management --------------------------------------------------
    /// Generate a new API key
    #[arg(long = "generate-api-key", value_name = "NAME")]
    pub generate_api_key: Option<String>,

    /// List all API keys
    #[arg(long = "list-api-keys")]
    pub list_api_keys: bool,

    /// Disable an API key
    #[arg(long = "disable-api-key", value_name = "NAME")]
    pub disable_api_key: Option<String>,

    /// Enable an API key
    #[arg(long = "enable-api-key", value_name = "NAME")]
    pub enable_api_key: Option<String>,

    /// Delete an API key permanently
    #[arg(long = "delete-api-key", value_name = "NAME")]
    pub delete_api_key: Option<String>,

    // -- Model and config ----------------------------------------------------
    /// Show current default agent model
    #[arg(long = "get-model")]
    pub get_model: bool,

    /// Set default agent model
    #[arg(long = "set-model", value_name = "MODEL")]
    pub set_model: Option<String>,

    /// Show all configuration variables
    #[arg(long = "show-config")]
    pub show_config: bool,
}

impl Cli {
    pub fn parse_args() -> Self {
        Self::parse()
    }
}
