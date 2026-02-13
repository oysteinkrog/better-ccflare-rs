use crate::constants::{network, time};
use crate::errors::AppError;
use crate::types::StrategyName;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// Config data persisted to `config.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConfigData {
    #[serde(default)]
    pub lb_strategy: Option<StrategyName>,
    #[serde(default)]
    pub client_id: Option<String>,
    #[serde(default)]
    pub retry_attempts: Option<u32>,
    #[serde(default)]
    pub retry_delay_ms: Option<u64>,
    #[serde(default)]
    pub retry_backoff: Option<f64>,
    #[serde(default)]
    pub session_duration_ms: Option<i64>,
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub default_agent_model: Option<String>,
    #[serde(default)]
    pub data_retention_days: Option<u32>,
    #[serde(default)]
    pub request_retention_days: Option<u32>,
    // Database configuration
    #[serde(default)]
    pub db_wal_mode: Option<bool>,
    #[serde(default)]
    pub db_busy_timeout_ms: Option<u64>,
    #[serde(default)]
    pub db_cache_size: Option<i64>,
    #[serde(default)]
    pub db_synchronous: Option<String>,
    #[serde(default)]
    pub db_mmap_size: Option<u64>,
    #[serde(default)]
    pub db_retry_attempts: Option<u32>,
    #[serde(default)]
    pub db_retry_delay_ms: Option<u64>,
    #[serde(default)]
    pub db_retry_backoff: Option<f64>,
    #[serde(default)]
    pub db_retry_max_delay_ms: Option<u64>,
}

/// Runtime configuration with resolved defaults.
#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    pub client_id: String,
    pub retry_attempts: u32,
    pub retry_delay_ms: u64,
    pub retry_backoff: f64,
    pub session_duration_ms: i64,
    pub port: u16,
    pub database: DatabaseConfig,
}

#[derive(Debug, Clone)]
pub struct DatabaseConfig {
    pub wal_mode: bool,
    pub busy_timeout_ms: u64,
    pub cache_size: i64,
    pub synchronous: String,
    pub mmap_size: u64,
    pub retry_attempts: u32,
    pub retry_delay_ms: u64,
    pub retry_backoff: f64,
    pub retry_max_delay_ms: u64,
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            wal_mode: true,
            busy_timeout_ms: 5000,
            cache_size: -20000,
            synchronous: "NORMAL".into(),
            mmap_size: 268_435_456,
            retry_attempts: 3,
            retry_delay_ms: 100,
            retry_backoff: 2.0,
            retry_max_delay_ms: 5000,
        }
    }
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            client_id: "9d1c250a-e61b-44d9-88ed-5944d1962f5e".into(),
            retry_attempts: 3,
            retry_delay_ms: time::RETRY_DELAY_DEFAULT as u64,
            retry_backoff: 2.0,
            session_duration_ms: time::SESSION_DURATION_DEFAULT,
            port: network::DEFAULT_PORT,
            database: DatabaseConfig::default(),
        }
    }
}

/// Application configuration manager.
#[derive(Debug, Clone)]
pub struct Config {
    path: PathBuf,
    data: ConfigData,
}

impl Config {
    /// Load configuration from the given path, creating defaults if missing.
    pub fn load(path: Option<PathBuf>) -> Result<Self, AppError> {
        let config_path = path.unwrap_or_else(resolve_config_path);

        let data = if config_path.exists() {
            match fs::read_to_string(&config_path) {
                Ok(content) => serde_json::from_str::<ConfigData>(&content).unwrap_or_default(),
                Err(e) => {
                    tracing::error!("Failed to read config file: {e}");
                    ConfigData::default()
                }
            }
        } else {
            // Create the config directory and write defaults
            if let Some(parent) = config_path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            let data = ConfigData {
                lb_strategy: Some(StrategyName::Session),
                ..Default::default()
            };
            if let Ok(json) = serde_json::to_string_pretty(&data) {
                let _ = fs::write(&config_path, json);
            }
            data
        };

        Ok(Self {
            path: config_path,
            data,
        })
    }

    pub fn data(&self) -> &ConfigData {
        &self.data
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn save(&self) {
        if let Ok(json) = serde_json::to_string_pretty(&self.data) {
            if let Err(e) = fs::write(&self.path, &json) {
                tracing::error!("Failed to save config: {e}");
            }
        }
    }

    pub fn get_strategy(&self) -> StrategyName {
        // Env > config > default
        if let Ok(env_val) = std::env::var("LB_STRATEGY") {
            if crate::types::is_valid_strategy(&env_val) {
                return StrategyName::Session; // Only one variant for now
            }
        }
        self.data.lb_strategy.unwrap_or_default()
    }

    pub fn set_strategy(&mut self, strategy: StrategyName) {
        self.data.lb_strategy = Some(strategy);
        self.save();
    }

    pub fn get_default_agent_model(&self) -> String {
        if let Ok(env_val) = std::env::var("DEFAULT_AGENT_MODEL") {
            if !env_val.is_empty() {
                return env_val;
            }
        }
        self.data
            .default_agent_model
            .clone()
            .unwrap_or_else(|| crate::models::DEFAULT_AGENT_MODEL.to_string())
    }

    pub fn get_data_retention_days(&self) -> u32 {
        if let Ok(env_val) = std::env::var("DATA_RETENTION_DAYS") {
            if let Ok(n) = env_val.parse::<u32>() {
                return n.clamp(1, 365);
            }
        }
        self.data.data_retention_days.unwrap_or(7).clamp(1, 365)
    }

    pub fn get_request_retention_days(&self) -> u32 {
        if let Ok(env_val) = std::env::var("REQUEST_RETENTION_DAYS") {
            if let Ok(n) = env_val.parse::<u32>() {
                return n.clamp(1, 3650);
            }
        }
        self.data
            .request_retention_days
            .unwrap_or(365)
            .clamp(1, 3650)
    }

    /// Resolve all settings into a `RuntimeConfig`, applying env > config > defaults precedence.
    pub fn get_runtime(&self) -> RuntimeConfig {
        let mut rt = RuntimeConfig::default();

        // Environment variable overrides
        if let Ok(v) = std::env::var("CLIENT_ID") {
            rt.client_id = v;
        }
        if let Ok(v) = std::env::var("RETRY_ATTEMPTS") {
            if let Ok(n) = v.parse() {
                rt.retry_attempts = n;
            }
        }
        if let Ok(v) = std::env::var("RETRY_DELAY_MS") {
            if let Ok(n) = v.parse() {
                rt.retry_delay_ms = n;
            }
        }
        if let Ok(v) = std::env::var("RETRY_BACKOFF") {
            if let Ok(n) = v.parse() {
                rt.retry_backoff = n;
            }
        }
        if let Ok(v) = std::env::var("SESSION_DURATION_MS") {
            if let Ok(n) = v.parse() {
                rt.session_duration_ms = n;
            }
        }
        if let Ok(v) = std::env::var("PORT") {
            if let Ok(n) = v.parse() {
                rt.port = n;
            }
        }

        // Config file overrides
        if let Some(ref v) = self.data.client_id {
            rt.client_id.clone_from(v);
        }
        if let Some(v) = self.data.retry_attempts {
            rt.retry_attempts = v;
        }
        if let Some(v) = self.data.retry_delay_ms {
            rt.retry_delay_ms = v;
        }
        if let Some(v) = self.data.retry_backoff {
            rt.retry_backoff = v;
        }
        if let Some(v) = self.data.session_duration_ms {
            rt.session_duration_ms = v;
        }
        if let Some(v) = self.data.port {
            rt.port = v;
        }

        // Database config overrides
        if let Some(v) = self.data.db_wal_mode {
            rt.database.wal_mode = v;
        }
        if let Some(v) = self.data.db_busy_timeout_ms {
            rt.database.busy_timeout_ms = v;
        }
        if let Some(v) = self.data.db_cache_size {
            rt.database.cache_size = v;
        }
        if let Some(ref v) = self.data.db_synchronous {
            rt.database.synchronous.clone_from(v);
        }
        if let Some(v) = self.data.db_mmap_size {
            rt.database.mmap_size = v;
        }
        if let Some(v) = self.data.db_retry_attempts {
            rt.database.retry_attempts = v;
        }
        if let Some(v) = self.data.db_retry_delay_ms {
            rt.database.retry_delay_ms = v;
        }
        if let Some(v) = self.data.db_retry_backoff {
            rt.database.retry_backoff = v;
        }
        if let Some(v) = self.data.db_retry_max_delay_ms {
            rt.database.retry_max_delay_ms = v;
        }

        rt
    }
}

// ---------------------------------------------------------------------------
// Path resolution
// ---------------------------------------------------------------------------

/// Get the platform-specific config directory for better-ccflare.
pub fn get_platform_config_dir() -> PathBuf {
    if cfg!(target_os = "windows") {
        let base = std::env::var("LOCALAPPDATA")
            .or_else(|_| std::env::var("APPDATA"))
            .unwrap_or_else(|_| {
                dirs::home_dir()
                    .map(|h| {
                        h.join("AppData")
                            .join("Local")
                            .to_string_lossy()
                            .into_owned()
                    })
                    .unwrap_or_default()
            });
        PathBuf::from(base).join("better-ccflare")
    } else {
        let base = std::env::var("XDG_CONFIG_HOME").unwrap_or_else(|_| {
            dirs::home_dir()
                .map(|h| h.join(".config").to_string_lossy().into_owned())
                .unwrap_or_else(|| "~/.config".into())
        });
        PathBuf::from(base).join("better-ccflare")
    }
}

/// Get the legacy ccflare config directory for migration.
pub fn get_legacy_config_dir() -> PathBuf {
    if cfg!(target_os = "windows") {
        let base = std::env::var("LOCALAPPDATA")
            .or_else(|_| std::env::var("APPDATA"))
            .unwrap_or_else(|_| {
                dirs::home_dir()
                    .map(|h| {
                        h.join("AppData")
                            .join("Local")
                            .to_string_lossy()
                            .into_owned()
                    })
                    .unwrap_or_default()
            });
        PathBuf::from(base).join("ccflare")
    } else {
        let base = std::env::var("XDG_CONFIG_HOME").unwrap_or_else(|_| {
            dirs::home_dir()
                .map(|h| h.join(".config").to_string_lossy().into_owned())
                .unwrap_or_else(|| "~/.config".into())
        });
        PathBuf::from(base).join("ccflare")
    }
}

/// Resolve the config file path, checking env vars first.
pub fn resolve_config_path() -> PathBuf {
    if let Ok(path) = std::env::var("BETTER_CCFLARE_CONFIG_PATH") {
        return PathBuf::from(path);
    }
    if let Ok(path) = std::env::var("ccflare_CONFIG_PATH") {
        return PathBuf::from(path);
    }
    get_platform_config_dir().join("better-ccflare.json")
}

/// Load `.env` files using dotenvy with the same precedence as the TS version.
pub fn load_dotenv() {
    // Try multiple .env file locations
    let _ = dotenvy::dotenv(); // .env in current directory
                               // Also try config directory
    let config_dir = get_platform_config_dir();
    let _ = dotenvy::from_path(config_dir.join(".env"));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_data_deserialize_empty() {
        let data: ConfigData = serde_json::from_str("{}").unwrap();
        assert!(data.lb_strategy.is_none());
        assert!(data.port.is_none());
    }

    #[test]
    fn config_data_deserialize_full() {
        let json = r#"{
            "lb_strategy": "session",
            "client_id": "test-id",
            "retry_attempts": 5,
            "port": 9090,
            "db_wal_mode": true,
            "db_cache_size": -10000,
            "data_retention_days": 30
        }"#;
        let data: ConfigData = serde_json::from_str(json).unwrap();
        assert_eq!(data.lb_strategy, Some(StrategyName::Session));
        assert_eq!(data.client_id.as_deref(), Some("test-id"));
        assert_eq!(data.retry_attempts, Some(5));
        assert_eq!(data.port, Some(9090));
        assert_eq!(data.db_wal_mode, Some(true));
        assert_eq!(data.db_cache_size, Some(-10000));
        assert_eq!(data.data_retention_days, Some(30));
    }

    #[test]
    fn runtime_config_defaults() {
        let rt = RuntimeConfig::default();
        assert_eq!(rt.port, 8080);
        assert_eq!(rt.retry_attempts, 3);
        assert_eq!(rt.session_duration_ms, 18_000_000);
        assert!(rt.database.wal_mode);
        assert_eq!(rt.database.synchronous, "NORMAL");
    }

    #[test]
    fn config_load_nonexistent_path() {
        let path = PathBuf::from("/tmp/bccf-test-nonexistent-12345/config.json");
        // Should not panic, just use defaults
        let config = Config::load(Some(path));
        assert!(config.is_ok());
    }

    #[test]
    fn platform_config_dir_not_empty() {
        let dir = get_platform_config_dir();
        assert!(dir.to_string_lossy().contains("better-ccflare"));
    }

    #[test]
    fn legacy_config_dir_not_empty() {
        let dir = get_legacy_config_dir();
        assert!(dir.to_string_lossy().contains("ccflare"));
        assert!(!dir.to_string_lossy().contains("better-ccflare"));
    }
}
