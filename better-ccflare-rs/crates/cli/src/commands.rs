//! CLI command handlers for account management.
//!
//! Each function corresponds to a CLI flag and operates on the database pool.

use anyhow::{bail, Context, Result};
use tracing::info;

use bccf_core::types::Account;
use bccf_database::repositories::account;
use bccf_database::DbPool;

use crate::args::VALID_MODES;
use crate::levenshtein;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Find an account by name (case-insensitive).
fn find_account_by_name(pool: &DbPool, name: &str) -> Result<Account> {
    let conn = pool.get().context("Failed to get database connection")?;
    let accounts = account::find_all(&conn)?;
    let name_lower = name.to_lowercase();
    accounts
        .into_iter()
        .find(|a| a.name.to_lowercase() == name_lower)
        .ok_or_else(|| anyhow::anyhow!("Account '{}' not found", name))
}

/// Map a mode string to a provider string stored in the database.
fn mode_to_provider(mode: &str) -> &str {
    match mode {
        "claude-oauth" => "claude-oauth",
        "console" => "claude-console-api",
        "zai" => "zai",
        "minimax" => "minimax",
        "nanogpt" => "nanogpt",
        "anthropic-compatible" => "anthropic-compatible",
        "openai-compatible" => "openai-compatible",
        "vertex-ai" => "vertex-ai",
        other => other,
    }
}

/// Check if a mode requires an API key (as opposed to OAuth flow).
fn mode_needs_api_key(mode: &str) -> bool {
    matches!(
        mode,
        "console"
            | "zai"
            | "minimax"
            | "nanogpt"
            | "anthropic-compatible"
            | "openai-compatible"
            | "vertex-ai"
    )
}

/// Check if a mode requires OAuth flow.
fn mode_needs_oauth(mode: &str) -> bool {
    mode == "claude-oauth"
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

/// Add a new account to the database.
///
/// For API key providers, reads the key from environment variables or stdin.
/// For OAuth providers (claude-oauth), starts an interactive OAuth flow.
pub fn add_account(
    pool: &DbPool,
    name: &str,
    mode: &str,
    priority: i64,
    api_key_input: Option<&str>,
) -> Result<()> {
    // Validate mode
    if !VALID_MODES.contains(&mode) {
        let suggestions = levenshtein::suggest(mode, VALID_MODES, 2, 3);
        let mut msg = format!(
            "Invalid mode: '{}'\nValid modes: {}",
            mode,
            VALID_MODES.join(", ")
        );
        if !suggestions.is_empty() {
            msg.push_str(&format!("\nDid you mean: {}?", suggestions.join(", ")));
        }
        bail!("{}", msg);
    }

    // Check for duplicate name
    if find_account_by_name(pool, name).is_ok() {
        bail!("Account '{}' already exists", name);
    }

    let provider = mode_to_provider(mode);
    let now = chrono::Utc::now().timestamp_millis();
    let id = uuid::Uuid::new_v4().to_string();

    if mode_needs_oauth(mode) {
        // OAuth flow - create a placeholder account that needs authentication
        // The actual OAuth flow will be handled by --reauthenticate
        let acct = Account {
            id,
            name: name.to_string(),
            provider: provider.to_string(),
            api_key: None,
            refresh_token: String::new(),
            access_token: None,
            expires_at: None,
            request_count: 0,
            total_requests: 0,
            last_used: None,
            created_at: now,
            rate_limited_until: None,
            session_start: None,
            session_request_count: 0,
            paused: false,
            rate_limit_reset: None,
            rate_limit_status: None,
            rate_limit_remaining: None,
            priority,
            auto_fallback_enabled: true,
            auto_refresh_enabled: true,
            custom_endpoint: None,
            model_mappings: None,
        };

        let conn = pool.get().context("Failed to get database connection")?;
        account::create(&conn, &acct)?;

        println!("Account '{}' created with provider '{}'", name, provider);
        println!(
            "Run `better-ccflare --reauthenticate {}` to complete the OAuth flow",
            name
        );
    } else if mode_needs_api_key(mode) {
        // API key mode
        let api_key = if let Some(key) = api_key_input {
            key.to_string()
        } else {
            // Try environment variables
            let env_key = format!(
                "BETTER_CCFLARE_API_KEY_{}",
                name.to_uppercase().replace('-', "_")
            );
            let alt_key = format!("API_KEY_{}", name.to_uppercase().replace('-', "_"));

            std::env::var(&env_key)
                .or_else(|_| std::env::var(&alt_key))
                .unwrap_or_default()
        };

        if api_key.is_empty() {
            bail!(
                "API key required for '{}' accounts. Provide via:\n  \
                 - --add-account {} --mode {} (then enter key when prompted)\n  \
                 - Set BETTER_CCFLARE_API_KEY_{} environment variable",
                mode,
                name,
                mode,
                name.to_uppercase().replace('-', "_")
            );
        }

        let acct = Account {
            id,
            name: name.to_string(),
            provider: provider.to_string(),
            api_key: Some(api_key.clone()),
            refresh_token: String::new(),
            access_token: Some(api_key),
            expires_at: Some(now + 30 * 24 * 60 * 60 * 1000), // 30 days
            request_count: 0,
            total_requests: 0,
            last_used: None,
            created_at: now,
            rate_limited_until: None,
            session_start: None,
            session_request_count: 0,
            paused: false,
            rate_limit_reset: None,
            rate_limit_status: None,
            rate_limit_remaining: None,
            priority,
            auto_fallback_enabled: true,
            auto_refresh_enabled: true,
            custom_endpoint: None,
            model_mappings: None,
        };

        let conn = pool.get().context("Failed to get database connection")?;
        account::create(&conn, &acct)?;

        println!("Account '{}' added successfully ({})", name, provider);
    } else {
        bail!("Unknown mode: '{}'", mode);
    }

    Ok(())
}

/// Remove an account by name.
pub fn remove_account(pool: &DbPool, name: &str) -> Result<()> {
    let acct = find_account_by_name(pool, name)?;
    let conn = pool.get().context("Failed to get database connection")?;

    if account::delete(&conn, &acct.id)? {
        println!("Account '{}' removed successfully", name);
        Ok(())
    } else {
        bail!("Failed to remove account '{}'", name)
    }
}

/// List all accounts with status information.
pub fn list_accounts(pool: &DbPool) -> Result<()> {
    let conn = pool.get().context("Failed to get database connection")?;
    let accounts = account::find_all(&conn)?;

    if accounts.is_empty() {
        println!("No accounts configured");
        return Ok(());
    }

    println!("\nAccounts:");
    for acc in &accounts {
        let status = if acc.paused {
            "paused"
        } else if acc
            .expires_at
            .is_some_and(|e| e < chrono::Utc::now().timestamp_millis())
        {
            "expired"
        } else if acc.rate_limited_until.is_some() {
            "rate-limited"
        } else {
            "active"
        };

        println!(
            "  {} ({}, priority {}, {} requests, {})",
            acc.name, acc.provider, acc.priority, acc.total_requests, status
        );
    }

    println!("\nTotal: {} account(s)", accounts.len());
    Ok(())
}

/// Pause an account.
pub fn pause_account(pool: &DbPool, name: &str) -> Result<()> {
    let acct = find_account_by_name(pool, name)?;

    if acct.paused {
        println!("Account '{}' is already paused", name);
        return Ok(());
    }

    let conn = pool.get().context("Failed to get database connection")?;
    account::pause(&conn, &acct.id)?;
    println!("Account '{}' paused", name);
    Ok(())
}

/// Resume a paused account.
pub fn resume_account(pool: &DbPool, name: &str) -> Result<()> {
    let acct = find_account_by_name(pool, name)?;

    if !acct.paused {
        println!("Account '{}' is not paused", name);
        return Ok(());
    }

    let conn = pool.get().context("Failed to get database connection")?;
    account::resume(&conn, &acct.id)?;
    println!("Account '{}' resumed", name);
    Ok(())
}

/// Update an account's priority.
pub fn set_priority(pool: &DbPool, name: &str, priority: i64) -> Result<()> {
    let acct = find_account_by_name(pool, name)?;
    let conn = pool.get().context("Failed to get database connection")?;
    account::update_priority(&conn, &acct.id, priority)?;
    println!("Account '{}' priority set to {}", name, priority);
    Ok(())
}

/// Re-authenticate an account, preserving metadata.
///
/// For OAuth accounts, this would restart the OAuth flow.
/// For API key accounts, this updates the API key.
pub fn reauthenticate_account(pool: &DbPool, name: &str, new_key: Option<&str>) -> Result<()> {
    let acct = find_account_by_name(pool, name)?;
    let conn = pool.get().context("Failed to get database connection")?;

    if acct.provider == "claude-oauth" {
        // OAuth flow would go here — for now, print instructions
        println!(
            "OAuth reauthentication for '{}' is not yet implemented in the Rust CLI.",
            name
        );
        println!("Use the TypeScript CLI or the dashboard to reauthenticate OAuth accounts.");
        return Ok(());
    }

    // API key provider — update the key
    let api_key = if let Some(key) = new_key {
        key.to_string()
    } else {
        // Try environment variables
        let env_key = format!(
            "BETTER_CCFLARE_API_KEY_{}",
            name.to_uppercase().replace('-', "_")
        );
        std::env::var(&env_key).unwrap_or_default()
    };

    if api_key.is_empty() {
        bail!(
            "API key required for reauthentication. Provide via argument or set BETTER_CCFLARE_API_KEY_{}",
            name.to_uppercase().replace('-', "_")
        );
    }

    let now = chrono::Utc::now().timestamp_millis();
    let expires_at = now + 30 * 24 * 60 * 60 * 1000; // 30 days

    account::update_tokens(&conn, &acct.id, &api_key, expires_at, None)?;

    info!(account = %name, "Reauthenticated account");
    println!("Account '{}' reauthenticated successfully", name);
    Ok(())
}

/// Show statistics in JSON format.
pub fn show_stats(pool: &DbPool) -> Result<()> {
    let conn = pool.get().context("Failed to get database connection")?;
    let accounts = account::find_all(&conn)?;
    let now = chrono::Utc::now().timestamp_millis();

    let active = accounts
        .iter()
        .filter(|a| {
            !a.paused && a.expires_at.is_none_or(|e| e > now) && a.rate_limited_until.is_none()
        })
        .count();

    let paused = accounts.iter().filter(|a| a.paused).count();
    let expired = accounts
        .iter()
        .filter(|a| a.expires_at.is_some_and(|e| e <= now))
        .count();
    let total_requests: i64 = accounts.iter().map(|a| a.total_requests).sum();

    let stats = serde_json::json!({
        "totalAccounts": accounts.len(),
        "activeAccounts": active,
        "pausedAccounts": paused,
        "expiredAccounts": expired,
        "totalRequests": total_requests,
        "accounts": accounts.iter().map(|acc| {
            let token_status = if acc.expires_at.is_some_and(|e| e <= now) {
                "expired"
            } else if acc.access_token.is_some() {
                "valid"
            } else {
                "missing"
            };

            serde_json::json!({
                "name": acc.name,
                "provider": acc.provider,
                "priority": acc.priority,
                "requestCount": acc.total_requests,
                "paused": acc.paused,
                "tokenStatus": token_status,
                "rateLimitStatus": acc.rate_limit_status,
            })
        }).collect::<Vec<_>>(),
    });

    println!("{}", serde_json::to_string_pretty(&stats)?);
    Ok(())
}

/// Reset all usage statistics.
pub fn reset_stats(pool: &DbPool) -> Result<()> {
    let conn = pool.get().context("Failed to get database connection")?;
    conn.execute_batch(
        "UPDATE accounts SET request_count = 0, session_request_count = 0, \
         session_start = NULL, last_used = NULL",
    )?;
    println!("Statistics reset successfully");
    Ok(())
}

/// Clear request history.
pub fn clear_history(pool: &DbPool) -> Result<()> {
    let conn = pool.get().context("Failed to get database connection")?;
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM requests", [], |row| row.get(0))?;
    conn.execute_batch("DELETE FROM requests")?;
    println!("Request history cleared ({} records removed)", count);
    Ok(())
}

/// Execute the appropriate command based on CLI args.
///
/// Returns `true` if a command was handled, `false` if no command matched
/// (meaning the server should start).
pub fn run(cli: &crate::args::Cli, pool: &DbPool) -> Result<bool> {
    // Account management
    if let Some(ref name) = cli.add_account {
        let mode = cli.mode.as_deref().ok_or_else(|| {
            anyhow::anyhow!(
                "Please provide --mode to specify account type\n\
                 Available modes: {}\n\
                 Example: better-ccflare --add-account {} --mode claude-oauth --priority 0",
                VALID_MODES.join(", "),
                name
            )
        })?;
        let priority = cli.priority.unwrap_or(0);
        add_account(pool, name, mode, priority, None)?;
        return Ok(true);
    }

    if let Some(ref name) = cli.remove {
        remove_account(pool, name)?;
        return Ok(true);
    }

    if cli.list {
        list_accounts(pool)?;
        return Ok(true);
    }

    if let Some(ref name) = cli.pause {
        pause_account(pool, name)?;
        return Ok(true);
    }

    if let Some(ref name) = cli.resume {
        resume_account(pool, name)?;
        return Ok(true);
    }

    if let Some(ref values) = cli.set_priority {
        if values.len() != 2 {
            bail!("--set-priority requires NAME and PRIORITY");
        }
        let name = &values[0];
        let priority: i64 = values[1].parse().context("Priority must be a number")?;
        set_priority(pool, name, priority)?;
        return Ok(true);
    }

    if let Some(ref name) = cli.reauthenticate {
        reauthenticate_account(pool, name, None)?;
        return Ok(true);
    }

    // Stats and maintenance
    if cli.stats {
        show_stats(pool)?;
        return Ok(true);
    }

    if cli.reset_stats {
        reset_stats(pool)?;
        return Ok(true);
    }

    if cli.clear_history {
        clear_history(pool)?;
        return Ok(true);
    }

    // No command matched
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bccf_database::pool::{create_memory_pool, PoolConfig};

    fn test_pool() -> DbPool {
        create_memory_pool(&PoolConfig::default()).unwrap()
    }

    #[test]
    fn add_and_list_account() {
        let pool = test_pool();
        add_account(&pool, "test-acc", "console", 5, Some("sk-test-key")).unwrap();

        let conn = pool.get().unwrap();
        let accounts = account::find_all(&conn).unwrap();
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].name, "test-acc");
        assert_eq!(accounts[0].provider, "claude-console-api");
        assert_eq!(accounts[0].priority, 5);
        assert_eq!(accounts[0].api_key.as_deref(), Some("sk-test-key"));
    }

    #[test]
    fn add_account_invalid_mode() {
        let pool = test_pool();
        let err = add_account(&pool, "test", "invalid-mode", 0, Some("key")).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("Invalid mode"));
    }

    #[test]
    fn add_account_typo_suggestion() {
        let pool = test_pool();
        let err = add_account(&pool, "test", "claude-outh", 0, Some("key")).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("Did you mean"));
        assert!(msg.contains("claude-oauth"));
    }

    #[test]
    fn add_duplicate_account_fails() {
        let pool = test_pool();
        add_account(&pool, "dup", "zai", 0, Some("key1")).unwrap();
        let err = add_account(&pool, "dup", "zai", 0, Some("key2")).unwrap_err();
        assert!(err.to_string().contains("already exists"));
    }

    #[test]
    fn add_api_key_account_no_key_fails() {
        let pool = test_pool();
        // Clear env vars to ensure no accidental match
        std::env::remove_var("BETTER_CCFLARE_API_KEY_TEST");
        std::env::remove_var("API_KEY_TEST");
        let err = add_account(&pool, "test", "console", 0, None).unwrap_err();
        assert!(err.to_string().contains("API key required"));
    }

    #[test]
    fn remove_account_works() {
        let pool = test_pool();
        add_account(&pool, "removeme", "zai", 0, Some("key")).unwrap();
        remove_account(&pool, "removeme").unwrap();

        let conn = pool.get().unwrap();
        assert!(account::find_all(&conn).unwrap().is_empty());
    }

    #[test]
    fn remove_nonexistent_fails() {
        let pool = test_pool();
        let err = remove_account(&pool, "ghost").unwrap_err();
        assert!(err.to_string().contains("not found"));
    }

    #[test]
    fn pause_and_resume() {
        let pool = test_pool();
        add_account(&pool, "toggle", "zai", 0, Some("key")).unwrap();

        pause_account(&pool, "toggle").unwrap();
        let acct = find_account_by_name(&pool, "toggle").unwrap();
        assert!(acct.paused);

        resume_account(&pool, "toggle").unwrap();
        let acct = find_account_by_name(&pool, "toggle").unwrap();
        assert!(!acct.paused);
    }

    #[test]
    fn set_priority_works() {
        let pool = test_pool();
        add_account(&pool, "prio", "zai", 0, Some("key")).unwrap();

        set_priority(&pool, "prio", 10).unwrap();
        let acct = find_account_by_name(&pool, "prio").unwrap();
        assert_eq!(acct.priority, 10);
    }

    #[test]
    fn show_stats_runs() {
        let pool = test_pool();
        add_account(&pool, "stat-acc", "zai", 0, Some("key")).unwrap();
        // Just verify it doesn't panic/error
        show_stats(&pool).unwrap();
    }

    #[test]
    fn reset_stats_works() {
        let pool = test_pool();
        add_account(&pool, "reset-acc", "zai", 0, Some("key")).unwrap();
        reset_stats(&pool).unwrap();

        let acct = find_account_by_name(&pool, "reset-acc").unwrap();
        assert_eq!(acct.request_count, 0);
    }

    #[test]
    fn mode_to_provider_mapping() {
        assert_eq!(mode_to_provider("claude-oauth"), "claude-oauth");
        assert_eq!(mode_to_provider("console"), "claude-console-api");
        assert_eq!(mode_to_provider("zai"), "zai");
        assert_eq!(mode_to_provider("minimax"), "minimax");
        assert_eq!(mode_to_provider("nanogpt"), "nanogpt");
        assert_eq!(
            mode_to_provider("anthropic-compatible"),
            "anthropic-compatible"
        );
        assert_eq!(mode_to_provider("openai-compatible"), "openai-compatible");
        assert_eq!(mode_to_provider("vertex-ai"), "vertex-ai");
    }

    #[test]
    fn mode_flags() {
        assert!(mode_needs_oauth("claude-oauth"));
        assert!(!mode_needs_oauth("console"));

        assert!(mode_needs_api_key("console"));
        assert!(mode_needs_api_key("zai"));
        assert!(mode_needs_api_key("vertex-ai"));
        assert!(!mode_needs_api_key("claude-oauth"));
    }

    #[test]
    fn oauth_account_created_as_placeholder() {
        let pool = test_pool();
        add_account(&pool, "oauth-test", "claude-oauth", 0, None).unwrap();

        let acct = find_account_by_name(&pool, "oauth-test").unwrap();
        assert_eq!(acct.provider, "claude-oauth");
        assert!(acct.api_key.is_none());
        assert!(acct.access_token.is_none());
    }

    #[test]
    fn case_insensitive_name_lookup() {
        let pool = test_pool();
        add_account(&pool, "MyAccount", "zai", 0, Some("key")).unwrap();

        // Should find regardless of case
        assert!(find_account_by_name(&pool, "myaccount").is_ok());
        assert!(find_account_by_name(&pool, "MYACCOUNT").is_ok());
        assert!(find_account_by_name(&pool, "MyAccount").is_ok());
    }
}
