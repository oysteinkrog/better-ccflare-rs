//! Dashboard crate — HTMX + Askama server-rendered dashboard.
//!
//! Serves the dashboard UI with tab-based navigation. Templates are compiled
//! at build time via Askama. Static assets (Pico CSS, HTMX) are embedded
//! in the binary via `include_str!`.

pub mod routes;
pub mod templates;
