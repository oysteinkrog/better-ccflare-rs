//! Database crate — SQLite database layer for better-ccflare.
//! Handles account storage, session management, and analytics data.

pub mod error;
pub mod migrations;
pub mod paths;
pub mod pool;
pub mod retry;
pub mod schema;

pub mod repositories;

// Re-exports
pub use error::DbError;
pub use pool::{DbPool, PoolConfig};
