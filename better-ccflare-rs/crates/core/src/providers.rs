use serde::{Deserialize, Serialize};
use std::fmt;

/// All known provider names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Provider {
    Anthropic,
    #[serde(rename = "claude-console-api")]
    ClaudeConsoleApi,
    Zai,
    Minimax,
    #[serde(rename = "anthropic-compatible")]
    AnthropicCompatible,
    #[serde(rename = "openai-compatible")]
    OpenaiCompatible,
    Nanogpt,
    #[serde(rename = "vertex-ai")]
    VertexAi,
}

impl fmt::Display for Provider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Anthropic => write!(f, "anthropic"),
            Self::ClaudeConsoleApi => write!(f, "claude-console-api"),
            Self::Zai => write!(f, "zai"),
            Self::Minimax => write!(f, "minimax"),
            Self::AnthropicCompatible => write!(f, "anthropic-compatible"),
            Self::OpenaiCompatible => write!(f, "openai-compatible"),
            Self::Nanogpt => write!(f, "nanogpt"),
            Self::VertexAi => write!(f, "vertex-ai"),
        }
    }
}

impl Provider {
    pub fn from_str_loose(s: &str) -> Option<Self> {
        match s {
            "anthropic" => Some(Self::Anthropic),
            "claude-console-api" => Some(Self::ClaudeConsoleApi),
            "zai" => Some(Self::Zai),
            "minimax" => Some(Self::Minimax),
            "anthropic-compatible" => Some(Self::AnthropicCompatible),
            "openai-compatible" => Some(Self::OpenaiCompatible),
            "nanogpt" => Some(Self::Nanogpt),
            "vertex-ai" => Some(Self::VertexAi),
            _ => None,
        }
    }

    /// All known provider variants.
    pub const ALL: &'static [Provider] = &[
        Self::Anthropic,
        Self::ClaudeConsoleApi,
        Self::Zai,
        Self::Minimax,
        Self::AnthropicCompatible,
        Self::OpenaiCompatible,
        Self::Nanogpt,
        Self::VertexAi,
    ];
}

/// Account modes for adding new accounts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AccountMode {
    ClaudeOauth,
    Console,
    Zai,
    Minimax,
    #[serde(rename = "anthropic-compatible")]
    AnthropicCompatible,
    #[serde(rename = "openai-compatible")]
    OpenaiCompatible,
    Nanogpt,
    #[serde(rename = "vertex-ai")]
    VertexAi,
}

impl fmt::Display for AccountMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ClaudeOauth => write!(f, "claude-oauth"),
            Self::Console => write!(f, "console"),
            Self::Zai => write!(f, "zai"),
            Self::Minimax => write!(f, "minimax"),
            Self::AnthropicCompatible => write!(f, "anthropic-compatible"),
            Self::OpenaiCompatible => write!(f, "openai-compatible"),
            Self::Nanogpt => write!(f, "nanogpt"),
            Self::VertexAi => write!(f, "vertex-ai"),
        }
    }
}

impl AccountMode {
    pub fn to_provider(self) -> Provider {
        match self {
            Self::ClaudeOauth => Provider::Anthropic,
            Self::Console => Provider::ClaudeConsoleApi,
            Self::Zai => Provider::Zai,
            Self::Minimax => Provider::Minimax,
            Self::AnthropicCompatible => Provider::AnthropicCompatible,
            Self::OpenaiCompatible => Provider::OpenaiCompatible,
            Self::Nanogpt => Provider::Nanogpt,
            Self::VertexAi => Provider::VertexAi,
        }
    }
}

/// Provider-specific configuration.
#[derive(Debug, Clone)]
pub struct ProviderConfig {
    pub requires_session_tracking: bool,
    pub supports_usage_tracking: bool,
    pub supports_oauth: bool,
    pub default_endpoint: &'static str,
}

impl Provider {
    pub fn config(&self) -> ProviderConfig {
        match self {
            Self::Anthropic => ProviderConfig {
                requires_session_tracking: true,
                supports_usage_tracking: true,
                supports_oauth: true,
                default_endpoint: "https://api.anthropic.com",
            },
            Self::ClaudeConsoleApi => ProviderConfig {
                requires_session_tracking: false,
                supports_usage_tracking: false,
                supports_oauth: false,
                default_endpoint: "https://api.anthropic.com",
            },
            Self::Zai => ProviderConfig {
                requires_session_tracking: false,
                supports_usage_tracking: true,
                supports_oauth: false,
                default_endpoint: "https://api.z.ai/api/anthropic",
            },
            Self::Minimax => ProviderConfig {
                requires_session_tracking: false,
                supports_usage_tracking: false,
                supports_oauth: false,
                default_endpoint: "https://api.minimax.io/anthropic",
            },
            Self::AnthropicCompatible => ProviderConfig {
                requires_session_tracking: false,
                supports_usage_tracking: false,
                supports_oauth: false,
                default_endpoint: "https://api.anthropic.com",
            },
            Self::OpenaiCompatible => ProviderConfig {
                requires_session_tracking: false,
                supports_usage_tracking: false,
                supports_oauth: false,
                default_endpoint: "https://api.anthropic.com",
            },
            Self::Nanogpt => ProviderConfig {
                requires_session_tracking: false,
                supports_usage_tracking: true,
                supports_oauth: false,
                default_endpoint: "https://nano-gpt.com/api",
            },
            Self::VertexAi => ProviderConfig {
                requires_session_tracking: false,
                supports_usage_tracking: false,
                supports_oauth: false,
                default_endpoint: "https://aiplatform.googleapis.com",
            },
        }
    }

    pub fn requires_session_tracking(&self) -> bool {
        self.config().requires_session_tracking
    }

    pub fn supports_usage_tracking(&self) -> bool {
        self.config().supports_usage_tracking
    }

    pub fn supports_oauth(&self) -> bool {
        self.config().supports_oauth
    }

    pub fn default_endpoint(&self) -> &'static str {
        self.config().default_endpoint
    }

    /// Whether this provider uses API key auth (all except Anthropic OAuth).
    pub fn uses_api_key(&self) -> bool {
        !matches!(self, Self::Anthropic)
    }
}

/// Get the default endpoint for a provider string, falling back to anthropic.
pub fn get_default_endpoint(provider_str: &str) -> &'static str {
    match Provider::from_str_loose(provider_str) {
        Some(p) => p.default_endpoint(),
        None => "https://api.anthropic.com",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_roundtrip_serde() {
        for p in Provider::ALL {
            let json = serde_json::to_string(p).unwrap();
            let back: Provider = serde_json::from_str(&json).unwrap();
            assert_eq!(*p, back);
        }
    }

    #[test]
    fn from_str_loose_valid() {
        assert_eq!(
            Provider::from_str_loose("anthropic"),
            Some(Provider::Anthropic)
        );
        assert_eq!(
            Provider::from_str_loose("vertex-ai"),
            Some(Provider::VertexAi)
        );
        assert_eq!(Provider::from_str_loose("unknown"), None);
    }

    #[test]
    fn provider_display() {
        assert_eq!(Provider::Anthropic.to_string(), "anthropic");
        assert_eq!(Provider::OpenaiCompatible.to_string(), "openai-compatible");
    }

    #[test]
    fn account_mode_to_provider() {
        assert_eq!(AccountMode::ClaudeOauth.to_provider(), Provider::Anthropic);
        assert_eq!(
            AccountMode::Console.to_provider(),
            Provider::ClaudeConsoleApi
        );
        assert_eq!(AccountMode::Nanogpt.to_provider(), Provider::Nanogpt);
    }

    #[test]
    fn anthropic_requires_session_tracking() {
        assert!(Provider::Anthropic.requires_session_tracking());
        assert!(!Provider::Zai.requires_session_tracking());
    }

    #[test]
    fn anthropic_supports_oauth() {
        assert!(Provider::Anthropic.supports_oauth());
        assert!(!Provider::Minimax.supports_oauth());
    }

    #[test]
    fn uses_api_key() {
        assert!(!Provider::Anthropic.uses_api_key());
        assert!(Provider::Zai.uses_api_key());
        assert!(Provider::ClaudeConsoleApi.uses_api_key());
    }
}
