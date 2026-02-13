//! Concrete provider implementations.
//!
//! Each module implements the [`Provider`](crate::traits::Provider) trait
//! for a specific AI service backend.

pub mod anthropic_compatible;
pub mod minimax;

// Re-exports for convenience
pub use anthropic_compatible::AnthropicCompatibleProvider;
pub use minimax::MinimaxProvider;
