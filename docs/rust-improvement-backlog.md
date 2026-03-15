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
