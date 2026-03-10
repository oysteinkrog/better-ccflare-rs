# better-ccflare-rs 🛡️

**Track Every Request. Go Low-Level. Never Hit Rate Limits Again.**

A ground-up rewrite of [better-ccflare](https://github.com/oysteinkrog/better-ccflare) in Rust. Same mission — intelligent Claude API load balancing across multiple accounts — but rebuilt from scratch for a single, fast, dependency-free binary you can drop on any server.

> **What this is:** A complete rewrite in Rust. Not a fork of the TypeScript code, not a wrapper. New architecture, new binary, same ideas.

## Why the Rust Rewrite?

The original TypeScript project required Node.js or Bun at runtime, ~250 MB of node_modules, and a build step before first use. This rewrite trades all of that for:

- **Single binary** — one file, no runtime, no `npm install`, no build step
- **~10 MB on disk** vs hundreds of MB of JavaScript dependencies
- **<5ms overhead** per proxied request
- **Trivial deployment** — `scp` the binary, write a systemd unit, done
- **Memory-safe by default** — Rust's ownership model eliminates entire classes of bugs

## Quick Start

### Download binary (Linux x86_64)

```bash
wget https://github.com/oysteinkrog/better-ccflare-rs/releases/latest/download/better-ccflare-linux-amd64
chmod +x better-ccflare-linux-amd64
./better-ccflare-linux-amd64 --serve
```

### Build from source

```bash
git clone https://github.com/oysteinkrog/better-ccflare-rs
cd better-ccflare-rs
cargo build --release
./target/release/better-ccflare --serve
```

Requires Rust 1.75+. No other dependencies.

## Configure Claude SDK

Point any Claude SDK or tool at your proxy:

```bash
export ANTHROPIC_BASE_URL=http://localhost:8080
export ANTHROPIC_API_KEY=<your-proxy-api-key>
```

For remote VPS deployments:

```bash
export ANTHROPIC_BASE_URL=https://your-proxy-domain.com
export ANTHROPIC_API_KEY=<your-proxy-api-key>
```

## Account Management

```bash
# Add a Claude OAuth account
better-ccflare --add-account myaccount --mode claude-oauth

# Add an Anthropic API key account
better-ccflare --add-account work --mode anthropic-compatible --priority 1

# List all accounts
better-ccflare --list

# Remove an account
better-ccflare --remove myaccount

# Re-authenticate (preserves metadata, auto-notifies running servers)
better-ccflare --reauthenticate myaccount

# Set priority (lower = higher priority, 0 = first)
better-ccflare --set-priority myaccount 0
```

## API Key Management

```bash
# Generate a proxy-scoped key (for agent machines, CI, etc.)
better-ccflare --generate-api-key "machine-1" --scopes proxy

# Generate an admin key (full access including dashboard API)
better-ccflare --generate-api-key "admin" --scopes "*"

# List keys
better-ccflare --list-api-keys
```

### Key Scopes

| Scope | Access |
|-------|--------|
| `proxy` | `/v1/*` — proxying Claude API requests only |
| `admin` | All routes including `/api/*` management endpoints |
| `*` | Full access (same as admin) |

## Environment Variables

```bash
# Server
BETTER_CCFLARE_HOST=127.0.0.1    # Bind address (default: 0.0.0.0)
BETTER_CCFLARE_PORT=8080          # Port (default: 8080)
BETTER_CCFLARE_DB_PATH=/path/to/better-ccflare.db  # SQLite DB path

# Security
DASHBOARD_PASSWORD=your-password  # Required to access dashboard on public deployments

# Logging
RUST_LOG=info                     # Log level: error, warn, info, debug, trace
```

## Providers

| Provider | Mode | Auth |
|----------|------|------|
| Claude OAuth (Pro/Team) | `claude-oauth` | Browser OAuth flow, 5-hour usage windows |
| Anthropic Console API | `anthropic-compatible` | API key, pay-as-you-go |
| Vertex AI | `vertex-ai` | GCP service account |
| OpenAI-compatible (OpenRouter, etc.) | `openai-compatible` | API key |
| z.ai | `zai` | API key |
| Minimax | `minimax` | API key |

## Server Features

### Load Balancing
- **Session-based routing** — maintains conversation context across requests for OAuth accounts
- **Priority-based selection** — lower priority number = higher preference
- **Auto-fallback** — switches back to higher-priority accounts when usage windows reset
- **Auto-refresh** — starts new usage windows when rate limits reset (with 30-minute buffer)
- **Stale usage detection** — ignores out-of-date usage data when making routing decisions

### Monitoring
- **Real-time dashboard** at `http://localhost:8080/dashboard`
- **Request analytics** — latency, token usage, cost per request
- **OAuth token health** — live status of token validity with automatic refresh
- **Usage polling** — background polling of account usage to stay ahead of limits

### Security
- **HTTP Basic Auth** on dashboard (set `DASHBOARD_PASSWORD`)
- **API key scopes** — separate proxy keys from admin keys
- **Fail-closed bootstrap** — when no keys exist and no password is set, all non-exempt endpoints return 403
- **Security headers** — `X-Content-Type-Options`, `X-Frame-Options`

## API Reference

### Proxy

```
POST /v1/messages          — Anthropic Messages API (proxied)
POST /v1/messages/count_tokens
POST /v1/complete          — Legacy completions
POST /v1/chat/completions  — OpenAI-compatible format
GET  /health               — Health check (always public)
```

### Management (requires admin scope or Basic Auth)

```
GET  /api/accounts             — List accounts
POST /api/accounts/:id/reload  — Reload account
POST /api/accounts/:id/pause   — Pause account
POST /api/accounts/:id/resume  — Resume account
GET  /api/api-keys             — List API keys
POST /api/api-keys             — Generate API key
DELETE /api/api-keys/:id       — Delete API key
GET  /metrics                  — Internal metrics
```

## Maintenance

```bash
better-ccflare --stats          # Usage statistics
better-ccflare --analyze        # Account analysis
better-ccflare --reset-stats    # Reset usage counters
better-ccflare --clear-history  # Clear request history
```

## Platform Support

| Platform | Architecture | Status |
|----------|-------------|--------|
| Linux | x86_64 | ✅ |
| Linux | ARM64 | ✅ |
| macOS | Intel | ✅ |
| macOS | Apple Silicon | ✅ |
| Windows | x86_64 | ✅ |

## Acknowledgments

This project builds on the ideas and community of:
- [snipeship/ccflare](https://github.com/snipeship/ccflare) — the original
- [tombii/better-ccflare](https://github.com/tombii/better-ccflare) — the TypeScript version this rewrites

## License

MIT
