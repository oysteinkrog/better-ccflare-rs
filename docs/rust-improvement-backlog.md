# Rust Improvement Backlog

This document tracks engineering improvements for `better-ccflare-rs`, with priority and implementation notes.

## Completed in this change

1. Shared eligibility engine in `core`
- Added `crates/core/src/account_eligibility.rs`.
- Centralizes routability decisions (paused/auth/rate-limit/overage/reserve).
- Exposes a typed decision object (`AccountEligibility`) and normalized reasons.

2. Typed upstream status parsing
- Added `UpstreamStatus` enum with parser (`from_raw`) and `is_auth_failure()`.
- Replaced ad-hoc auth-failure string matching with enum-based parsing.

3. Shared epoch normalization helper
- Added `normalize_epoch_millis()` in `core::account_eligibility`.
- Consumers now reuse one implementation instead of duplicating sec/ms handling.

## Coverage runbook (this repo)

Use this exact command in this environment:

```bash
CC=/usr/bin/gcc CXX=/usr/bin/g++ RUSTFLAGS='-Clinker=/usr/bin/gcc' \
cargo +nightly-2026-01-22-x86_64-unknown-linux-gnu llvm-cov --workspace --summary-only
```

Why this override is needed:
- `.cargo/config.toml` forces `CC=/c/users/oystein/.local/bin/cc` (a local `zig cc` wrapper).
- With `cargo llvm-cov`, that wrapper can fail linking `__llvm_profile_runtime`.
- For coverage runs, forcing `gcc` avoids that linker/runtime mismatch.

Current baseline (2026-03-15):
- Total: `72.07% regions`, `74.01% functions`, `70.49% lines`.
- Eligibility/routing core path is strong:
  - `crates/core/src/account_eligibility.rs`: `95.47% lines`
  - `crates/load-balancer/src/lib.rs`: `99.03% lines`

## High priority next

1. Snapshot-test dashboard account cards
- Add snapshot tests for key account states:
  - auth failed
  - paused
  - active rate limit
  - overage blocked
  - hard reserve blocked
  - soft reserve warning + routable
- Goal: prevent UI drift and badge regressions.

2. Structured error taxonomy in proxy path
- Replace free-form operational errors with typed enums (`thiserror`) + stable error codes.
- Include account-id and reason-code tags for metrics and dashboards.

3. Account-state transition log (event sourcing lite)
- Append-only table recording state transitions:
  - `auth_failed`
  - `overage_blocked`
  - `hard_reserve_blocked`
  - `rate_limited`
  - `recovered`
- Goal: debug “out of the blue” incidents quickly.

4. Coverage ROI: top 5 next test targets
- 1) `crates/proxy/src/handlers/xfactor.rs` (0.00% lines)
  - Add route-level tests for each endpoint outcome:
  - empty state, valid state, malformed query/body, auth/permissions denied.
- 2) `src/main.rs` (0.00% lines)
  - Add smoke tests for CLI startup paths:
  - `--help`, `--version`, invalid flag handling, basic argument wiring.
- 3) `crates/dashboard/src/routes.rs` (27.78% lines)
  - Add focused handler tests for untested tabs/actions:
  - account actions, config updates, failed dependency responses.
- 4) `crates/proxy/src/oauth.rs` (30.99% lines)
  - Add integration-style tests for callback/session lifecycle:
  - success path, state mismatch, session expiry, provider error propagation.
- 5) `crates/proxy/src/handlers/analytics.rs` (49.44% lines)
  - Add deterministic analytics tests:
  - empty windows, mixed provider/account filters, percentile/aggregation edge cases.

Expected impact:
- These five files are both low-coverage and high operational value.
- Bringing them to ~70% line coverage should materially lift workspace totals and reduce regression risk in known incident paths.

## Medium priority

1. Property-based tests for usage normalization
- Add `proptest` for mixed/partial usage payloads and edge values (`NaN`, negative, huge numbers).
- Verify parser invariants and fail-open/fail-closed behavior.

2. Concurrency boundaries for caches
- Wrap account/usage caches behind small traits with explicit consistency semantics.
- Add deterministic tests for stale cache + concurrent reload patterns.

3. Add deny-level lints in critical crates
- `load-balancer`, `providers`, `proxy`:
  - `unused_must_use`
  - `unreachable_pub`
  - selected clippy denies
- Keep strictness scoped so iteration speed stays high.

## Lower priority

1. `EpochMs` / `DurationMs` newtypes
- Stronger type-safety against sec/ms mistakes.
- Roll out incrementally starting in `core` and routing/eligibility boundaries.

2. Chaos integration tests
- Simulate:
  - auth flap
  - usage API stale/missing
  - DB reconnect/restart
  - mixed account pool degradation
- Validate “at least one account routable => no 503”.
