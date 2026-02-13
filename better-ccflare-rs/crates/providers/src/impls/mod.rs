//! Concrete provider implementations.
//!
//! Each module implements the [`Provider`](crate::traits::Provider) trait
//! for a specific AI service backend.

pub mod anthropic_compatible;
pub mod claude_oauth;
pub mod minimax;
pub mod nanogpt;
pub mod openai_compatible;
pub mod openai_format;
pub mod openai_stream;
pub mod zai;

// Re-exports for convenience
pub use anthropic_compatible::AnthropicCompatibleProvider;
pub use claude_oauth::ClaudeOAuthProvider;
pub use minimax::MinimaxProvider;
pub use nanogpt::NanoGptProvider;
pub use openai_compatible::OpenAiCompatibleProvider;
pub use openai_stream::OpenAiStreamContext;
pub use zai::ZaiProvider;
