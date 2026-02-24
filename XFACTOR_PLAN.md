# X-Factor Estimation & Pool Capacity System — Master Plan

*Synthesized from 5 Opus agents + 2 oracle models (Gemini 1.5 Pro, GPT-5.2 Pro), 4 rounds of deep Q&A.*

---

## Goals

1. **Per-account X-factor/value**: Know the real effective capacity of each account and its cost-per-million-tokens vs pay-as-you-go, so you can decide which subscription tier gives best value per USD.
2. **Pool capacity estimation**: Real-time remaining tokens per account, time-to-exhaustion prediction, proactive session migration before hitting 429s.

---

## Part 1: Mathematical Foundation

### 1.1 The Observation Model

Each Anthropic account has an unknown absolute token capacity `C_i` per 5h rolling window.

At any poll time `τ_k` (utilization API, ~90s lag behind wall clock):

```
U_i(τ_k) = 100 * (P_i(τ_k) + E_i(τ_k)) / C_i
```

Where:
- `P_i(τ_k)` = proxy tokens consumed in [τ_k - 5h, τ_k] — **exactly computable** from our request log
- `E_i(τ_k)` = external tokens consumed in same window — **unobserved** (zero if not shared)
- `U_i` = utilization percent reported by Anthropic — **observable but lagged and noisy**

Rearranging gives a **lower bound** on `C_i` per poll:

```
LB_ik = P_i(τ_k) / (U_i(τ_k) / 100)    [only valid when 5% ≤ U ≤ 95%]
```

Because `E_i ≥ 0`, we always have `C_i ≥ LB_ik`. External usage can only push LB down (looser bound), never up.

### 1.2 Identifiability Fix: Anchor Pro = 1×

`C_i = X_i × C_base` has infinitely many solutions. **Fix**: declare Pro tier as `X_pro ≡ 1.0`. Then `C_base = C_i` for any Pro account, and `X_i = C_i / C_base` for others.

After sufficient observations across multiple Pro accounts: `C_base = median(C_i for Pro accounts, is_shared=false)`.

### 1.3 Bayesian Posterior for C_i

**Prior** (log-normal, based on subscription tier and community estimates):

| Tier     | μ_T = ln(m_T)  | m_T (tokens/5h) | σ_T   |
|----------|---------------|-----------------|-------|
| Pro      | ln(88,000)    | 88,000          | 0.353 |
| Max 5x   | ln(440,000)   | 440,000         | 0.353 |
| Max 20x  | ln(880,000)   | 880,000         | 0.353 |
| Unknown  | ln(200,000)   | 200,000         | 0.693 |

σ_T = 0.353 means "95% confidence within ×2 of median". σ_T = 0.693 for unknown tier means "very uncertain".

**Effective sample size** (not raw poll count — polls are autocorrelated at 90s resolution):

```
n_eff = time_span_hours / 5.0   [cap at reasonable max, e.g. 100]
```

**Blending weight**: `w = n_eff / (n_eff + n0)` where `n0 = 4` (prior equivalent to 4 independent windows).

**Empirical estimate** (high-quantile of lower bounds, adaptive to sample size):

```
q(n_eff) = 1 - 1/(n_eff + 1)   [so 4 samples → Q0.80, 9 → Q0.90, 19 → Q0.95]
Gate LB samples to U ∈ [5%, 95%] to avoid division by tiny U.
```

**Blended posterior (log-space conjugate update)**:

```
log_C_i_estimate = (1 - w) * μ_T + w * log(quantile(LB_ik, q(n_eff)))
```

For uncertainty propagation, maintain full log-normal posterior `(μ, σ²)` updated via inverse-variance weighting:

```
μ_new = (μ_old / σ²_old + log(LB_new) / σ²_obs) / (1/σ²_old + 1/σ²_obs)
σ²_new = 1 / (1/σ²_old + 1/σ²_obs)
```

Where `σ²_obs ≈ 0.05` (variance of a single uncensored observation in log-space).

**Cold start routing**: Use prior 25th percentile (pessimistic) to avoid 429s:

```
C_i_routing = exp(μ_T - 0.674 * σ_T)
```

### 1.4 Model-Weighted Tokens

**Critical**: Do NOT pre-weight tokens at ingestion time. Store raw per-model token counts.

Anthropic's utilization is likely weighted by model compute intensity. Proxy weights based on published TPM limits:

```
w_opus   = 4.0  (relative to sonnet baseline)
w_sonnet = 1.0
w_haiku  = 0.25
```

These can be refined empirically: if LB estimates diverge across sessions with different model mixes, adjust weights. Keep them configurable.

Effective proxy tokens for a session:
```
P_weighted = Σ (opus_tokens * w_opus + sonnet_tokens * w_sonnet + haiku_tokens * w_haiku)
```

### 1.5 Lag Alignment

The utilization API lags by ~90s but with variance. Approach:

1. **Global estimate**: Start with `δ_global = 90s`. Works for routing; fine as v1.
2. **Per-account refinement** (once 20+ "events" observed): Find large token requests (≥10k tokens), find first subsequent utilization jump ≥ 0.5%. Median of event delays → `δ_i`.
3. **Discrete candidate set**: `{60s, 90s, 120s, 150s, 180s}` — pick one minimizing squared residuals.

**Ring buffer alignment**: Maintain a deque of `(timestamp, P_i_rolling)` snapshots for the last 5 minutes. When a utilization poll arrives at wall time `t_k`, look up `P_i(t_k - δ)` from the buffer.

### 1.6 External Usage Estimation (Kalman Filter)

Separate the deterministic proxy rolling sum from the stochastic external component:

**State**: `x = [E_i, Ė_i]` (external tokens in 5h rolling window, their rate)

**Dynamics** (each Δt seconds, process noise Q drives external rate changes):
```
F = [[1, Δt],   Q = [[0,     0      ],
     [0, 1 ]]         [0, σ²_rate*Δt]]
```

**Observation** (when poll arrives):
```
H = [1, 0]    (we observe E = max(0, C_i * U/100 - P_i))
R = (C_i * 0.01)^2 / 12   (variance of 1% quantization bucket)
```

**Process noise**: `σ_rate ≈ 1000 tokens/sec²` (allow external rate to jump rapidly).

**Lagged update procedure**:
1. Maintain ring buffer of KF states for last 5 minutes (one snapshot per predict step)
2. When poll at wall time `t_k` arrives → rollback to `τ_k = t_k - δ`
3. Apply KF update step at `τ_k`
4. Replay predict steps forward to current time
5. **Confidence decay is free**: covariance P inflates every predict step (P += Q); no manual decay needed.

**Shared usage detection** (from KF output):
```
E_mean_5h = mean(E_i estimates over last 24h)
shared_score = E_mean_5h / P_mean_5h
Flag as suspicious if shared_score > 0.15
```

### 1.7 Rolling Window Recovery

**Key insight**: Remaining capacity can INCREASE as old usage falls off the rolling window. This means TTE is NOT monotonically decreasing. Must track deterministic future refunds.

Maintain proxy deque with exact timestamps. Compute:

```
F(Δ) = tokens in deque with timestamp < (now - 5h + Δ)
     = tokens that will fall off the window within the next Δ minutes
```

Conservative future slack:
```
R_min(Δ) = R_now - D_forecast(Δ) + F(Δ) - safety_buffer

Where:
  R_now = C_i_estimate - (P_5h_weighted + E_i_kf)
  D_forecast(Δ) = ema_rate * Δ * 60  (seconds)
  F(Δ) = sum of proxy tokens falling off in [now, now+Δ]
  safety_buffer = max(5000, p99_request_size)
```

**Wait vs migrate decision**: If `R_min(Δ=15min) > 0`, safe to wait (capacity will recover). If `R_min(Δ=5min) ≤ 0`, migrate now.

### 1.8 Censored Observations

When `U ≥ 95%`: RIGHT-CENSORED. Do NOT use for MAP update. Do NOT throw away — treat as:
- Constraint: `C_i ≥ P_i(τ_k) / 0.95`
- Update lower bound: `LB_hard = max(LB_hard, P_i(τ_k) / 0.95)`
- Apply barrier in routing: "stop assigning, account is near-saturated"

---

## Part 2: Per-Account Value Analysis

### 2.1 Token Aggregations

From the requests table, maintain two parallel monthly aggregates (rolling 30d):

```sql
SELECT account_used,
  SUM(input_tokens + output_tokens)                         AS budget_tokens_raw,
  SUM(input_tokens + output_tokens
      + cache_creation_input_tokens
      + cache_read_input_tokens * 0.1)                      AS budget_tokens_billed_equiv,
  SUM(output_tokens * 15.0/1e6
      + input_tokens * 3.0/1e6
      + cache_creation_input_tokens * 3.75/1e6
      + cache_read_input_tokens * 0.3/1e6)                  AS payg_cost_usd,
  COUNT(*)                                                  AS request_count,
  MIN(timestamp)                                            AS first_request,
  MAX(timestamp)                                            AS last_request
FROM requests
WHERE timestamp >= (unixepoch('now') - 30*86400) * 1000
GROUP BY account_used
```

Note: `total_tokens` in our schema = `input_tokens + output_tokens` (cache tokens stored separately). Prices: Sonnet $3/$15/$3.75/$0.30 per million (input/output/cache_create/cache_read). Configurable.

### 2.2 Cost Inference

```
monthly_cost_usd → use if set
else → infer from subscription_tier:
  "Pro"       → $20
  "Max 5x"    → $100
  "Max 20x"   → $200
  "Max"       → $200  (conservative until confirmed)
  "Free"      → $0
  unknown     → null (suppress ROI comparisons)
```

Always report `cost_source: "explicit" | "inferred_from_tier" | "unknown"`.

### 2.3 Active-Time Normalization

Avoid penalizing newly-added accounts:

```
active_start = max(account.created_at, now - 30d)
active_days  = (now - active_start) / 86400
tokens_30d_equiv = (tokens_observed / active_days) * 30
  [only if active_days ≥ 3; else null]
```

### 2.4 Break-Even Analysis

Blended PAYG rate at 70% input / 30% output (Sonnet):
```
rate_blended = 0.70 * $3/M + 0.30 * $15/M = $6.60/M tokens
```

Break-even monthly token volume for each tier:
```
T* = subscription_cost / 6.60 * 1e6

Pro ($20):     T* ≈  3.03M tokens/month
Max 5x ($100): T* ≈ 15.15M tokens/month
Max 20x ($200):T* ≈ 30.30M tokens/month
```

### 2.5 Cost Per Million Tokens (CPM)

Three variants to report:

```
cpm_actual    = monthly_cost_usd / (tokens_30d_equiv / 1e6)
              [what you actually paid per million delivered tokens]

cpm_payg_equiv = payg_cost_usd_30d_equiv / (tokens_30d_equiv / 1e6)
               [what it would've cost on pay-as-you-go]

cpm_theoretical = monthly_cost_usd / (max_tokens_per_month / 1e6)
                [best-case if fully utilized]

max_tokens_per_month = C_i_estimate * (30 * 24 / 5)  [5h windows]
  constrained by weekly cap if known
```

### 2.6 Realized Utilization

```
realized_pct = tokens_30d_equiv / max_tokens_per_month * 100
```

### 2.7 Recommendation Engine (10 Rules)

Priority-ordered. All thresholds configurable. Suppress rules requiring cost if `cost_source = "unknown"`.

1. **EXPENSIVE_VS_PAYG**: `cpm_actual > 1.25 * 6.60` AND `active_days ≥ 14`
   → "Subscription costs more than pay-as-you-go. Consider removing unless needed for burst capacity."

2. **GOOD_VALUE**: `cpm_actual < 0.8 * 6.60` AND `active_days ≥ 14`
   → "Excellent value vs PAYG. Consider adding more accounts of this tier if you're pool-constrained."

3. **UNDERUTILIZED**: `realized_pct < 20%` AND `active_days ≥ 21`
   → "Account used less than 20% of capacity over 30 days. Candidate for removal."

4. **NEAR_EXHAUSTION_5H**: `R_5h < safety_buffer`
   → "Near 5h token limit. Stop assigning new sessions."

5. **NEAR_EXHAUSTION_7D**: `weekly_utilization > 90%`
   → "Approaching weekly token limit. Reassign sustained workloads."

6. **SUSPECTED_SHARED**: `shared_score > 0.15` AND NOT `is_shared`
   → "Utilization growing faster than proxy traffic explains. Possible external usage — mark as shared if true."

7. **FREQUENT_ERRORS**: `rate_limited_count_7d ≥ 10`
   → "This account hit rate limits frequently. Capacity estimate may be too high."

8. **LOW_CONFIDENCE**: `n_eff < 2`
   → "Insufficient data to estimate capacity reliably. Routing conservatively."

9. **POOL_BURST_BOUND**: Pool-level: `demand_5h_p99 > 0.90 * total_pool_cap_5h`
   → "Pool burst capacity near limit. Add higher-5h-cap tier accounts."

10. **POOL_SUSTAINED_BOUND**: Pool-level: `demand_7d_p99 > 0.90 * total_pool_cap_7d`
    → "Pool sustained throughput near weekly limit. Add accounts or consider PAYG overflow."

### 2.8 Tier Optimization

Small integer program (enumerate all feasible mixes up to e.g. 10 accounts per tier):

**Minimize**: `Σ n_t * cost_t`
**Subject to**: `Σ n_t * C_t_5h ≥ demand_5h_p99` AND `Σ n_t * W_t_7d ≥ demand_7d_p99`

Where `demand_5h_p99` = 99th percentile of rolling 5h proxy token sums from the last 30d.

Output includes: optimal mix, candidates ranked by cost, which constraint is binding, headroom on each constraint.

---

## Part 3: Pool Capacity Forecasting

### 3.1 Per-Account State

```rust
struct CapacityState {
    // Rolling deques (store raw per-model counts, NOT pre-weighted)
    window_5h: VecDeque<(Timestamp, TokenRecord)>,  // last 5h of proxy requests
    window_7d: VecDeque<(Timestamp, TokenRecord)>,  // last 7d (can be hourly buckets)

    // Running weighted sum for O(1) queries
    proxy_tokens_5h_weighted: f64,

    // Capacity posterior (log-normal)
    mu: f64,         // log-space mean
    sigma_sq: f64,   // log-space variance
    n_eff: f64,      // effective observation count

    // Kalman filter for external usage
    kf_e: f64,       // external tokens in current 5h window
    kf_e_dot: f64,   // external token rate (tokens/sec)
    kf_p: [[f64;2];2], // 2x2 covariance matrix
    kf_history: VecDeque<(Timestamp, KfSnapshot)>, // last 5min for lag rollback

    // Rate estimator
    ema_proxy_rate: f64,  // tokens/sec EMA (α = 0.05 per request)

    // Lag tracking
    lag_estimate_ms: u64,  // default 90_000

    // Hard lower bound from censored observations
    c_i_hard_lower: f64,

    // Global c_base (shared, updated by coordinator)
    c_base: f64,
}
```

**Persistence** (to DB, periodically and on shutdown):
```
account_id, mu, sigma_sq, n_eff, kf_e, kf_e_dot, kf_p_flat[4],
ema_proxy_rate, lag_estimate_ms, c_i_hard_lower, updated_at_ms
```

On restart: restore KF/posterior from DB. Rebuild rolling deques from `requests` table for the last 7d. This is O(N_requests_per_account) which is fine.

### 3.2 Remaining Tokens (3-value estimate)

```
R_pessimistic = max(0, exp(μ - 1.645*σ) - total_used)  [5th pct]
R_expected    = max(0, exp(μ) - total_used)              [median]
R_optimistic  = max(0, exp(μ + 1.645*σ) - total_used)   [95th pct]

total_used = proxy_tokens_5h_weighted + max(0, kf_e)
```

Also respect hard lower bound: `C_i ≥ c_i_hard_lower`, so `exp(μ) = max(exp(μ), c_i_hard_lower)`.

### 3.3 Time-to-Exhaustion

```
total_rate = ema_proxy_rate + max(0, kf_e_dot)

if total_rate ≤ 0.01:
    tte = ∞ (no drain)
else:
    tte_minutes = R_pessimistic / total_rate / 60.0

# With rolling-window recovery:
F_15min = tokens in 5h deque with ts < (now - 5h + 15min)
R_adjusted = R_pessimistic + F_15min
tte_adjusted = R_adjusted / total_rate / 60.0
```

### 3.4 Proactive Migration Trigger

```
# Stop assigning new sessions when:
should_stop_assign = R_min(5min) ≤ 0
  where R_min(5min) = R_pessimistic - ema_proxy_rate*300 + F(5min) - safety_buffer

# Resume assigning when (hysteresis):
should_resume_assign = R_min(15min) > resume_threshold
  where resume_threshold = max(20_000, p99_request_size * 3)
```

### 3.5 Pool-Level Aggregates

```
pool_capacity_expected = Σ R_expected_i           [across active, non-paused accounts]
pool_capacity_pessimistic = Σ R_pessimistic_i
pool_tte_minutes = min(tte_minutes_i)              [soonest exhaustion]

accounts_by_slack: rank by (R_min(15min) / C_i_estimate) DESC
```

---

## Part 4: Rust Implementation Architecture

### 4.1 New Files

```
crates/proxy/src/
  xfactor/
    mod.rs            — public API, XFactorService struct
    state.rs          — CapacityState, KfState, Posterior structs
    estimator.rs      — on_poll_update, on_request_complete, query functions
    capacity_cache.rs — Arc<DashMap<AccountId, CapacityState>>, replaces/extends UsageCache
    coordinator.rs    — global C_base estimation across accounts, tier comparison

crates/database/src/repositories/
  xfactor.rs          — persist/restore CapacityState, query window observations

crates/proxy/src/handlers/
  xfactor.rs          — HTTP handlers for new API endpoints
```

### 4.2 New DB Tables

```sql
-- Persisted estimator state (one row per account)
CREATE TABLE IF NOT EXISTS account_xfactor_state (
    account_id TEXT PRIMARY KEY REFERENCES accounts(id),
    mu REAL NOT NULL,
    sigma_sq REAL NOT NULL,
    n_eff REAL NOT NULL DEFAULT 0,
    kf_e REAL NOT NULL DEFAULT 0,
    kf_e_dot REAL NOT NULL DEFAULT 0,
    kf_p00 REAL NOT NULL DEFAULT 1000000,
    kf_p01 REAL NOT NULL DEFAULT 0,
    kf_p10 REAL NOT NULL DEFAULT 0,
    kf_p11 REAL NOT NULL DEFAULT 1000000,
    ema_proxy_rate REAL NOT NULL DEFAULT 0,
    lag_estimate_ms INTEGER NOT NULL DEFAULT 90000,
    c_i_hard_lower REAL NOT NULL DEFAULT 0,
    updated_at INTEGER NOT NULL
);

-- Raw per-window LB observations for offline analysis (optional, can skip v1)
CREATE TABLE IF NOT EXISTS xfactor_observations (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    account_id TEXT NOT NULL,
    timestamp_ms INTEGER NOT NULL,
    proxy_tokens_weighted REAL NOT NULL,
    utilization_pct REAL NOT NULL,
    lb_estimate REAL NOT NULL,
    censored INTEGER NOT NULL DEFAULT 0,
    window_type TEXT NOT NULL DEFAULT '5h'
);
CREATE INDEX IF NOT EXISTS idx_xfactor_obs_account ON xfactor_observations(account_id, timestamp_ms);
```

Migration (add to `run_column_migrations`):
```rust
let _ = conn.execute("CREATE TABLE IF NOT EXISTS account_xfactor_state (...)", []);
let _ = conn.execute("CREATE TABLE IF NOT EXISTS xfactor_observations (...)", []);
```

### 4.3 XFactorService

Background service, similar to UsagePollingService:

```rust
pub struct XFactorService {
    capacity_cache: Arc<DashMap<String, CapacityState>>,
    c_base: Arc<RwLock<f64>>,  // global Pro capacity estimate
    model_weights: ModelWeights,
}

impl XFactorService {
    // Called by UsagePollingService when new poll arrives
    pub fn on_usage_poll(&self, account_id: &str, data: &AnyUsageData) { ... }

    // Called by post_processor after each request completes
    pub fn on_request_complete(&self, account_id: &str, tokens: &TokenBreakdown, model: &str) { ... }

    // Periodic task: update C_base from Pro accounts, persist state to DB
    pub async fn run_periodic(&self, pool: &DbPool) { ... }
}
```

Wiring in `AppState`: add `xfactor_service: Option<Arc<dyn Any + Send + Sync>>` (same pattern as existing).

### 4.4 Load Balancer Integration

In `SessionStrategy::select_account`, after existing rate-limit/pause checks, add capacity check:

```rust
// Skip accounts near exhaustion
if let Some(cap_state) = capacity_cache.get(account_id) {
    let (r_pessimistic, _, _) = cap_state.remaining_tokens_estimate();
    if r_pessimistic < safety_buffer {
        continue;  // skip this account
    }
}

// Prefer accounts with most remaining capacity (optional, weighted routing)
```

This is an additive guard — doesn't change existing session-pinning logic, just prevents routing to near-exhausted accounts.

### 4.5 New API Endpoints

```
GET  /api/analytics/xfactor         — full X-factor report (all accounts)
GET  /api/analytics/pool-capacity   — real-time pool state
GET  /api/analytics/value           — ROI/CPM/break-even report
GET  /api/analytics/tier-optimizer  — optimal tier mix recommendation
GET  /api/accounts/{id}/xfactor     — per-account X-factor detail
```

### 4.6 API Response Shapes

**`GET /api/analytics/xfactor`**:
```json
{
  "computed_at": "2026-02-24T12:00:00Z",
  "c_base_estimate": 88000,
  "c_base_source": "median_of_3_pro_accounts",
  "accounts": [
    {
      "id": "acc1",
      "name": "oystein-claude-1",
      "subscription_tier": "Max 20x",
      "x_factor": { "lo": 8.2, "mid": 10.4, "hi": 13.1 },
      "c_5h_tokens": { "lo": 720000, "mid": 914000, "hi": 1153000 },
      "n_eff": 12.3,
      "confidence": "medium",
      "shared_score": 0.04,
      "is_shared": false,
      "suspected_shared": false,
      "cost_source": "explicit",
      "monthly_cost_usd": 200.0,
      "cpm_actual_usd": 2.14,
      "cpm_payg_equiv_usd": 7.82,
      "cpm_theoretical_usd": 0.18,
      "realized_pct": 3.2,
      "tokens_30d": 93500000,
      "active_days": 30,
      "recommendations": ["GOOD_VALUE"]
    }
  ],
  "pool_summary": {
    "total_capacity_5h": { "pessimistic": 1800000, "expected": 2300000 },
    "total_remaining_5h": { "pessimistic": 1200000, "expected": 1600000 },
    "soonest_exhaustion_minutes": 47.3,
    "recommendations": ["GOOD_VALUE"]
  }
}
```

**`GET /api/analytics/pool-capacity`**:
```json
{
  "computed_at": "2026-02-24T12:00:00Z",
  "accounts": [
    {
      "id": "acc1",
      "name": "oystein-claude-1",
      "remaining_5h": { "pessimistic": 412000, "expected": 530000, "optimistic": 680000 },
      "remaining_7d": { "pessimistic": 2100000, "expected": 2800000, "optimistic": null },
      "tte_minutes": { "current": 47.3, "with_window_recovery": 82.1 },
      "binding_constraint": "5h",
      "utilization_5h_pct": 43.2,
      "should_stop_assign": false,
      "last_poll_age_seconds": 34
    }
  ],
  "pool_total": {
    "remaining_expected": 1600000,
    "remaining_pessimistic": 1200000,
    "soonest_tte_minutes": 47.3
  }
}
```

### 4.7 Implementation Phases

**Phase 1** (foundation, ~1 week):
- `account_xfactor_state` and `xfactor_observations` DB tables + migration
- `CapacityState` and `Posterior` structs (no KF yet — just Bayesian posterior)
- Rolling 5h deque with per-model token counts
- Simple LB estimator + posterior update on each poll
- Restore state from DB on startup; rebuild deques from request log

**Phase 2** (capacity queries, ~3 days):
- `remaining_tokens_estimate()` → 3-value output
- EMA rate estimator from request completions
- Basic `tte_minutes()` (no rolling-window recovery yet)
- `GET /api/analytics/pool-capacity` endpoint

**Phase 3** (Kalman + lag, ~1 week):
- KF for external usage estimation: `[E, Ė]` state
- Ring buffer for lagged updates
- Lag estimation: start with global 90s, add event-based refinement
- Rolling-window recovery `F(Δ)` computation
- Improved `tte_minutes` with recovery

**Phase 4** (value analysis, ~3 days):
- Rolling 30d SQL aggregation query (tokens, cost, PAYG equivalent)
- CPM variants, break-even analysis
- Active-time normalization
- Recommendation engine (10 rules)
- `GET /api/analytics/xfactor` and `/value` endpoints

**Phase 5** (advanced, ~1 week):
- Global `C_base` estimation from Pro accounts
- Model weight refinement via EM (optional)
- Tier optimizer (integer enumeration)
- Load balancer integration (skip near-exhausted accounts)
- `GET /api/analytics/tier-optimizer` endpoint
- Dashboard UI cards

---

## Part 5: Key Risks and Mitigations

| Risk | Mitigation |
|---|---|
| Anthropic changes window type (rolling → resetting) | Track utilization recovery patterns; window type detection via dU behavior |
| Anthropic changes utilization weights per model | Make `model_weights` configurable; EM-based weight refinement in phase 5 |
| is_shared causes bad capacity estimates | Detect via shared_score; flag in UI; use LB quantile (robust to external) |
| C_base drifts over time (Anthropic adjusts limits) | Sliding window LB quantile (last 7d, not all-time); slowly forget old data |
| KF diverges if lag estimate is wrong | Clip E_i ≥ 0 always; large R (measurement noise) prevents overconfident updates |
| New account with zero observations routes poorly | Always route at prior 25th percentile for first n_eff < 1 |
| total_tokens vs weighted tokens mismatch | Fit k_read empirically (same cross-correlation used for lag estimation) |

---

## Part 6: Quick Reference — Parameter Values

```
WINDOW_5H_MS            = 5 * 3600 * 1000
LAG_ESTIMATE_DEFAULT_MS = 90_000
KF_PROCESS_NOISE_RATE   = 100.0          -- tokens/sec² variance on external rate
KF_MEASUREMENT_NOISE_R  = 8000.0         -- quantization noise variance (1% bucket)
EMA_ALPHA               = 0.05           -- rate estimator smoothing
PRIOR_N0                = 4.0            -- prior strength in n_eff units
PRIOR_SIGMA_T           = 0.353          -- "95% within ×2" for known tiers
SHARED_SCORE_THRESHOLD  = 0.15           -- flag if external > 15% of proxy
STOP_ASSIGN_BUFFER      = 20_000         -- token safety buffer for stop-assign
CENSORED_U_THRESHOLD    = 95.0           -- skip MAP update above this %
MIN_U_FOR_LB            = 5.0            -- skip LB update below this %

-- Community prior medians (tokens per 5h window)
PRIOR_PRO_TOKENS        = 88_000
PRIOR_MAX5X_TOKENS      = 440_000
PRIOR_MAX20X_TOKENS     = 880_000

-- PAYG rates (USD/million tokens, Sonnet defaults)
PAYG_INPUT_USD_PER_M    = 3.0
PAYG_OUTPUT_USD_PER_M   = 15.0
PAYG_CACHE_CREATE_PER_M = 3.75
PAYG_CACHE_READ_PER_M   = 0.30

-- Break-even at 70/30 input/output mix
PAYG_BLENDED_USD_PER_M  = 6.60
BREAK_EVEN_PRO_TOKENS   = 3_030_000
BREAK_EVEN_MAX5X_TOKENS = 15_150_000
BREAK_EVEN_MAX20X_TOKENS= 30_300_000
```
