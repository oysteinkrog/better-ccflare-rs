//! CLI crate — clap-based command-line interface for better-ccflare.
//!
//! Handles account management (add, remove, list, pause, resume, set-priority,
//! reauthenticate) and other administrative commands. Flag names match the
//! TypeScript CLI for backwards compatibility.

pub mod args;
pub mod commands;
pub mod levenshtein;

pub use args::Cli;
