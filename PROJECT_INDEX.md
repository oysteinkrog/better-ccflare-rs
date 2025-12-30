# PROJECT_INDEX.md

> Auto-generated repository index for **better-ccflare** v3.0.11
> Token-efficient context loading for AI-assisted development

## Project Overview

**better-ccflare** is a Claude API proxy with intelligent load balancing across multiple accounts. It provides full visibility into every request, response, and rate limit.

| Attribute | Value |
|-----------|-------|
| Version | 3.0.11 |
| Runtime | Bun >= 1.2.8 |
| Database | SQLite (auto-migrations) |
| License | Open Source |

### Key Features
- Zero rate limit errors via multi-account load balancing
- Multi-provider support (Claude OAuth, API, NanoGPT, z.ai, Minimax, OpenAI-compatible)
- Real-time OAuth token health monitoring with automatic refresh
- Request-level analytics with latency, token usage, and cost tracking
- Project tracking via `X-CCFlare-Project` header
- <10ms overhead with lazy loading and request deduplication

---

## Repository Structure

```
better-ccflare/
├── apps/                    # Application entry points
│   ├── cli/                 # CLI application (main.ts)
│   ├── server/              # HTTP server (server.ts)
│   └── lander/              # Landing page
├── packages/                # Shared packages (19 total)
│   ├── types/               # TypeScript type definitions
│   ├── core/                # Core utilities, constants, validation
│   ├── database/            # SQLite operations, migrations, repositories
│   ├── providers/           # API provider implementations
│   ├── proxy/               # Request proxying and token handling
│   ├── http-api/            # REST API router and handlers
│   ├── config/              # Configuration management
│   ├── logger/              # Logging system
│   ├── load-balancer/       # Session-based load balancing strategy
│   ├── agents/              # Agent discovery and management
│   ├── cli-commands/        # CLI command implementations
│   ├── errors/              # Error types and HTTP error factories
│   ├── security/            # Path validation, security utilities
│   ├── oauth-flow/          # OAuth authentication flow
│   ├── http-common/         # Shared HTTP utilities
│   ├── core-di/             # Dependency injection container
│   ├── dashboard-web/       # React dashboard UI
│   ├── ui-common/           # Shared UI components
│   └── ui-constants/        # UI constants (colors, chart config)
└── docs/                    # Documentation
```

---

## Package API Reference

### `@better-ccflare/types`
Type definitions for the entire system.

**Exports:** `account`, `agent`, `agent-constants`, `api`, `api-key`, `constants`, `context`, `conversation`, `logging`, `request`, `stats`, `strategy`

---

### `@better-ccflare/core`
Core utilities, constants, error types, and validation logic.

**Key Exports:**
- **Constants:** `BUFFER_SIZES`, `CACHE`, `HTTP_STATUS`, `LIMITS`, `NETWORK`, `TIME_CONSTANTS`
- **Errors:** `OAuthError`, `ProviderError`, `RateLimitError`, `ServiceUnavailableError`, `TokenRefreshError`, `ValidationError`
- **Models:** `CLAUDE_MODEL_IDS`, `DEFAULT_MODEL`, `DEFAULT_AGENT_MODEL`, `getModelDisplayName()`, `isValidModelId()`
- **Strategy:** Load balancing strategy types and utilities
- **Validation:** `validateApiKey()`, `validateEndpointUrl()`, `validateNumber()`, `validateString()`, `patterns`, `sanitizers`
- **Version:** `getVersion()`, `getVersionSync()`, `extractClaudeVersion()`, `CLAUDE_CLI_VERSION`
- **Lifecycle:** Application lifecycle management
- **Intervals:** `intervalManager`, `registerCleanup()`, `registerHeartbeat()`, `registerUIRefresh()`
- **Model Mappings:** `mapModelName()`, `getModelMappings()`, `DEFAULT_MODEL_MAPPINGS`
- **Pricing:** `estimateCostUSD()`, `initializeNanoGPTPricingIfAccountsExist()`

---

### `@better-ccflare/database`
SQLite database operations with automatic migrations.

**Key Exports:**
- `DatabaseOperations` - Main database class (auto-runs migrations on init)
- `DatabaseFactory` - Singleton factory for database instance
- `ensureSchema()`, `runMigrations()` - Migration functions
- `AsyncDbWriter` - Async batch database writer
- `resolveDbPath()`, `getLegacyDbPath()` - Path resolution
- `migrateFromCcflare()` - Legacy migration
- `withDatabaseRetry()`, `withDatabaseRetrySync()` - Retry utilities

**Auto-Migration:** Runs automatically on `DatabaseOperations` instantiation via `DatabaseFactory.getInstance()`

**Key Tables:** `accounts`, `requests` (with `project` column), `api_keys`, `agent_preferences`, `oauth_states`

---

### `@better-ccflare/providers`
API provider implementations and registry.

**Built-in Providers:**
- `AnthropicProvider` - Claude OAuth and API
- `NanoGPTProvider` - NanoGPT with dynamic pricing
- `ZaiProvider` - z.ai provider
- `MinimaxProvider` - Minimax provider
- `OpenAICompatibleProvider` - OpenAI-compatible endpoints
- `AnthropicCompatibleProvider` - Anthropic-compatible endpoints

**Key Exports:**
- `BaseProvider` - Abstract provider class
- `getProvider()`, `listProviders()`, `registerProvider()` - Registry functions
- `getOAuthProvider()`, `listOAuthProviders()` - OAuth-specific registry
- `createProviderForService()`, `createAnthropicCompatibleProvider()` - Factory functions
- `PresetProviders` - Pre-configured provider presets

---

### `@better-ccflare/proxy`
Request proxying, token management, and response handling.

**Key Exports:**
- `handleProxy()` - Main proxy handler
- `ProxyContext` - Request context type
- `forwardToClient()` - Response forwarding
- `AutoRefreshScheduler` - Token auto-refresh
- Token Health: `checkAllAccountsHealth()`, `getValidAccessToken()`, `isRefreshTokenLikelyExpired()`, `formatTokenHealthReport()`
- `startGlobalTokenHealthChecks()`, `stopGlobalTokenHealthChecks()` - Health check lifecycle
- Worker: `getUsageWorker()`, `terminateUsageWorker()` - Usage tracking worker

---

### `@better-ccflare/http-api`
REST API router and handlers.

**Key Exports:**
- `APIRouter` - Main HTTP router
- `AuthService` - API key authentication service
- HTTP error utilities

**API Endpoints:**
| Method | Endpoint | Purpose |
|--------|----------|---------|
| GET | `/api/stats` | Stats summary |
| GET | `/api/accounts` | List accounts |
| POST | `/api/accounts` | Add account |
| GET | `/api/requests` | Request history |
| GET | `/api/requests/detail` | Detailed requests |
| GET | `/api/analytics` | Analytics data |
| GET | `/api/agents` | List agents |
| GET | `/api/api-keys` | API key management |
| GET | `/api/config` | Configuration |
| GET | `/api/token-health` | Token health status |
| GET | `/api/projects` | Distinct projects for filtering |

---

### `@better-ccflare/config`
Configuration management with environment and file-based settings.

**Key Exports:**
- `Config` - Configuration class with EventEmitter
- `RuntimeConfig` - Runtime configuration type
- `ConfigData` - Persisted configuration type
- `resolveConfigPath()`, `getPlatformConfigDir()`, `getLegacyConfigDir()`

**Key Settings:** `lb_strategy`, `port`, `default_agent_model`, `data_retention_days`, `request_retention_days`, database config (`db_*`)

---

### `@better-ccflare/logger`
Logging system with file and event bus output.

**Key Exports:**
- `Logger` - Logger class with DEBUG/INFO/WARN/ERROR levels
- `LogLevel` - Enum for log levels
- `LogFormat` - 'pretty' | 'json'
- `logBus` - EventEmitter for log streaming
- `logFileWriter` - File writer instance

---

### `@better-ccflare/load-balancer`
Session-based load balancing strategy.

**Key Exports:**
- `SessionStrategy` - Load balancing strategy implementation

---

### `@better-ccflare/agents`
Agent discovery and workspace management.

**Key Exports:**
- `AgentRegistry`, `agentRegistry` - Agent discovery and registration
- `getAgentsDirectory()` - Agent directory path
- `workspacePersistence` - Workspace state persistence

---

### `@better-ccflare/cli-commands`
CLI command implementations.

**Exports:** Commands for `account`, `analyze`, `api-key`, `database-repair`, `help`, `stats`, `token-health`
- `runCli()` - Main CLI runner
- Prompts and browser utilities

---

### `@better-ccflare/errors`
Error types and HTTP error factories.

**Error Types:** `OAuthError`, `ProviderError`, `RateLimitError`, `ServiceUnavailableError`, `TokenRefreshError`, `ValidationError`, `HttpError`

**HTTP Factories:** `BadRequest`, `Unauthorized`, `Forbidden`, `NotFound`, `Conflict`, `UnprocessableEntity`, `TooManyRequests`, `InternalServerError`, `BadGateway`, `ServiceUnavailable`, `GatewayTimeout`

**Utilities:** `getErrorType()`, `formatError()`, `parseHttpError()`, `isNetworkError()`, `isAuthError()`, etc.

---

### `@better-ccflare/security`
Path validation and security utilities.

**Key Exports:**
- `validatePath()` - Validate file paths
- `validatePathOrThrow()` - Validate or throw error
- `getDefaultAllowedBasePaths()` - Default allowed paths
- Types: `PathValidationOptions`, `PathValidationResult`, `SecurityConfig`

---

### `@better-ccflare/oauth-flow`
OAuth authentication flow for Anthropic.

**Key Exports:**
- `OAuthFlow` - OAuth flow handler class
- `createOAuthFlow()` - Factory function
- Types: `BeginOptions`, `BeginResult`, `CompleteOptions`, `AccountCreated`, `OAuthFlowResult`

**Modes:** `claude-oauth` (refresh tokens) | `console` (static API key)

---

### `@better-ccflare/http-common`
Shared HTTP utilities.

**Exports:** `client`, `errors`, `headers`, `responses`

---

### `@better-ccflare/core-di`
Dependency injection container.

**Key Exports:**
- `container` - DI container instance
- `SERVICE_KEYS` - Service key symbols (Logger, Config, Database, PricingLogger, AsyncWriter)
- `getService<T>()` - Type-safe service resolution

---

### `@better-ccflare/dashboard-web`
React dashboard UI with charts and analytics.

**Key Components:**
- Tabs: `OverviewTab`, `AccountsTab`, `RequestsTab`, `AnalyticsTab`, `StatsTab`, `LogsTab`, `AgentsTab`, `ApiKeysTab`
- Charts: `RequestVolumeChart`, `ResponseTimeChart`, `TokenUsageChart`, `CostChart`, `TokenSpeedChart`, `ModelPerformanceChart`
- Account Management: `AccountList`, `AccountAddForm`, `AccountPriorityDialog`, dialogs
- Analytics: `AnalyticsCharts`, `ModelAnalytics`, `TokenSpeedAnalytics`
- Hooks: `useApiError`, `useRequestStream`, `useDeduplicatedAnalytics`, `useRequestBatching`
- Version: `packages/dashboard-web/src/lib/version.ts` - reads from root `package.json`

---

### `@better-ccflare/ui-common`
Shared UI components and utilities.

**Exports:** `TokenUsageDisplay`, formatters, parsers (`parse-conversation`), presenters, utilities

---

### `@better-ccflare/ui-constants`
UI constants and configuration.

**Exports:**
- `COLORS` - Color palette (primary, success, warning, error, etc.)
- `CHART_COLORS` - Chart color sequence
- `TIME_RANGES` - Time range options
- `CHART_HEIGHTS`, `CHART_TOOLTIP_STYLE`, `CHART_PROPS`
- `REFRESH_INTERVALS`, `API_TIMEOUT`, `QUERY_CONFIG`, `API_LIMITS`

---

## Application Entry Points

### CLI (`apps/cli/src/main.ts`)
- Interactive CLI for account management and configuration
- Uses `DatabaseFactory.getInstance()` → auto-migrations
- Commands: account add/remove, stats, analyze, api-key management

### Server (`apps/server/src/server.ts`)
- HTTP server with proxy and REST API
- Uses `DatabaseFactory.getInstance()` → auto-migrations
- Serves dashboard at root, API at `/api/*`, proxy at `/v1/*`

---

## Key Architectural Patterns

### Database Initialization
```typescript
// Both CLI and Server use this pattern
DatabaseFactory.initialize(undefined, runtime);
const dbOps = DatabaseFactory.getInstance(); // Triggers migrations
```

### Provider Registration
```typescript
// Providers auto-register on import
import { getProvider, listProviders } from "@better-ccflare/providers";
const provider = getProvider("anthropic");
```

### Configuration Hierarchy
1. Environment variables (highest priority)
2. Config file (`~/.better-ccflare/config.json`)
3. Default values

### Version Management
- **Source of truth**: Root `package.json` version field
- Dashboard reads version via: `packages/dashboard-web/src/lib/version.ts`

---

## Scripts

| Script | Description |
|--------|-------------|
| `bun run cli` | Run CLI application |
| `bun run start` / `bun run server` | Run HTTP server |
| `bun run dev:server` | Run server with hot reload |
| `bun run dev:dashboard` | Run dashboard with hot reload |
| `bun run build` | Build dashboard and CLI |
| `bun run typecheck` | TypeScript type checking |
| `bun run test` | Run tests |
| `bun run lint` | Run Biome linter |

---

## Documentation

| File | Description |
|------|-------------|
| `docs/architecture.md` | System architecture |
| `docs/providers.md` | Provider configuration |
| `docs/cli.md` | CLI reference |
| `docs/auto-fallback.md` | Auto-fallback feature |
| `docs/auto-refresh.md` | Token auto-refresh |
| `docs/deployment.md` | Deployment guide |
| `docs/troubleshooting.md` | Common issues |
| `docs/migration-v2-to-v3.md` | v2 to v3 migration |

---

## File Quick Reference

### Critical Files
- `apps/server/src/server.ts` - Server initialization with DB
- `apps/cli/src/main.ts` - CLI initialization with DB
- `packages/database/src/database-operations.ts` - Main DB operations class
- `packages/database/src/migrations.ts` - Schema and migration definitions
- `packages/database/src/factory.ts` - DatabaseFactory singleton
- `packages/providers/src/registry.ts` - Provider registry
- `packages/http-api/src/router.ts` - API router
- `packages/dashboard-web/src/lib/version.ts` - Dashboard version (reads root package.json)

### Configuration Files
- `package.json` - Root monorepo config, **version source of truth**
- `apps/cli/package.json` - CLI package (npm publish)
- `tsconfig.json` - TypeScript config
- `biome.json` - Linter config

---

## Recent Changes (v3.0.10-v3.0.11)

### v3.0.11
- **Fix**: Dashboard version display now reads from root `package.json` (source of truth)

### v3.0.10
- **Feature**: Project tracking via `X-CCFlare-Project` header
- **Feature**: Advanced filtering in Requests tab (project, model, tokens-only)
- **Feature**: `/api/projects` endpoint for distinct project names
- **Database**: Added `project` column to `requests` table with index

---

*Generated: 2025-12-30*
