//! Per-account capacity state — Bayesian log-normal posterior + Kalman Filter for external usage.
//!
//! Design notes (from XFACTOR_PLAN.md):
//! - Store raw token counts per request, weight on access (don't pre-weight).
//! - Lower-bound estimator: LB = P_5h_weighted / (U/100), valid for U in [5%, 95%).
//! - Bayesian update in log-space via inverse-variance weighting.
//! - Decay posterior variance each window to handle non-stationarity.
//! - Kalman Filter for external usage [E, Ė]: separates proxy from external consumption.
//! - Lag correction: P_at_lag computed from window timestamps (API lags ~90s).
//! - Window recovery F(Δ): tokens expiring in next Δ minutes improve TTE estimate.

use std::collections::VecDeque;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// 5-hour rolling window in milliseconds.
pub const WINDOW_5H_MS: i64 = 5 * 3600 * 1000;

/// Minimum utilization % below which LB is unreliable (U too small → huge LB).
pub const MIN_U_FOR_LB: f64 = 5.0;

/// Utilization % above which observations are right-censored (skip MAP update).
pub const CENSORED_U_THRESHOLD: f64 = 95.0;

/// Log-space observation variance for a single uncensored poll.
pub const SIGMA_SQ_OBS: f64 = 0.05;

/// EMA smoothing factor for proxy token rate estimator.
pub const EMA_ALPHA: f64 = 0.05;

/// Variance decay factor per posterior update (allows C_i to change over time).
/// Equivalent to `σ²_new = σ²_old / DECAY_FACTOR`, capped at 4.0.
pub const DECAY_FACTOR: f64 = 0.98;

/// Token safety buffer for stop-assign decision.
pub const STOP_ASSIGN_BUFFER: f64 = 20_000.0;

/// Assumed Pro tier baseline capacity per 5h window.
pub const C_BASE_PRO: f64 = 88_000.0;

/// Default API utilization lag in milliseconds (~90s typical).
pub const LAG_ESTIMATE_MS: i64 = 90_000;

// ---------------------------------------------------------------------------
// Kalman Filter constants
// ---------------------------------------------------------------------------

/// Process noise: variance of external rate changes per second (tokens/sec²)².
/// From XFACTOR_PLAN.md Part 6: KF_PROCESS_NOISE_RATE = 100.0.
pub const KF_PROCESS_NOISE_RATE: f64 = 100.0;

/// Initial KF state covariance (very uncertain at start).
pub const KF_INIT_VARIANCE: f64 = 1e12;

/// Shared score threshold: flag if external > 50% of proxy tokens.
///
/// Set conservatively high because the KF naturally absorbs any capacity
/// over-estimation as apparent "external" usage.  We only want to flag
/// accounts where external consumption clearly dominates.
pub const SHARED_SCORE_THRESHOLD: f64 = 0.50;

// ---------------------------------------------------------------------------
// Model weights
// ---------------------------------------------------------------------------

/// Token weight for Anthropic utilization estimation (relative to Sonnet).
/// Approximate: based on published TPM limits ratios.
pub fn model_weight(model: Option<&str>) -> f64 {
    let Some(m) = model else { return 1.0 };
    let m = m.to_ascii_lowercase();
    if m.contains("opus") {
        4.0
    } else if m.contains("haiku") {
        0.25
    } else {
        1.0 // sonnet and unknown
    }
}

// ---------------------------------------------------------------------------
// Prior parameters
// ---------------------------------------------------------------------------

/// Log-normal prior (mu_log, sigma_sq_log) for a subscription tier.
pub fn tier_prior(tier: Option<&str>) -> (f64, f64) {
    const SIGMA_T: f64 = 0.353; // "95% within ×2 of median"
    let sigma_sq = SIGMA_T * SIGMA_T;
    match tier.map(str::trim) {
        Some(t) if t.contains("20x") => ((880_000.0_f64).ln(), sigma_sq),
        Some(t) if t.contains("5x") => ((440_000.0_f64).ln(), sigma_sq),
        Some(t) if t.to_ascii_lowercase().contains("max") => {
            // Generic Max — unknown multiplier; wider prior
            ((440_000.0_f64).ln(), 0.5_f64 * 0.5)
        }
        Some(t) if t.to_ascii_lowercase().contains("pro") => ((88_000.0_f64).ln(), sigma_sq),
        Some(t) if t.to_ascii_lowercase().contains("free") => {
            ((10_000.0_f64).ln(), 0.693 * 0.693)
        }
        _ => {
            // Unknown tier — very uncertain prior (σ ≈ 0.693 → 95% within ×4)
            ((200_000.0_f64).ln(), 0.693 * 0.693)
        }
    }
}

// Re-export the DB type so callers can use it via the state module.
pub use bccf_database::repositories::xfactor::XFactorDbState;

// ---------------------------------------------------------------------------
// Per-account in-memory state
// ---------------------------------------------------------------------------

/// Full per-account capacity state maintained in memory.
#[derive(Debug)]
pub struct AccountCapacityState {
    pub account_id: String,
    pub account_name: String,
    pub subscription_tier: Option<String>,
    pub is_shared: bool,

    /// Rolling 5h deque of (timestamp_ms, weighted_tokens).
    /// Entries older than 5h are evicted lazily on access.
    pub window_5h: VecDeque<(i64, f64)>,
    /// Running sum of weighted tokens inside the 5h window.
    pub proxy_tokens_5h_weighted: f64,

    /// Log-normal posterior: log-space mean.
    pub mu: f64,
    /// Log-normal posterior: log-space variance.
    pub sigma_sq: f64,
    /// Effective independent-window sample count (autocorrelation-adjusted).
    pub n_eff: f64,

    /// EMA proxy token rate (weighted tokens/sec, α = 0.05 per request).
    pub ema_proxy_rate: f64,

    /// Hard lower bound on C_i: max LB seen from censored (U ≥ 95%) observations.
    pub c_i_hard_lower: f64,

    // -----------------------------------------------------------------------
    // Kalman Filter state for external usage estimation.
    // State vector x = [E, Ė]:
    //   E   = external tokens consumed in current 5h window
    //   Ė   = external token consumption rate (tokens/sec)
    // -----------------------------------------------------------------------

    /// KF state: estimated external tokens in 5h window.
    pub kf_e: f64,
    /// KF state: estimated external token rate (tokens/sec).
    pub kf_e_dot: f64,
    /// KF covariance matrix (2×2, symmetric): [[P00, P01], [P10, P11]].
    pub kf_p: [[f64; 2]; 2],
    /// Wall-clock time (ms) when KF was last predicted forward.
    pub last_kf_predict_ms: i64,
    /// Estimated API utilization lag in milliseconds (default 90s).
    pub lag_estimate_ms: i64,

    /// Wall-clock time (ms) when the most recent usage poll touched this account.
    pub last_poll_at_ms: Option<i64>,
    /// Wall-clock time (ms) of the most recent state update.
    pub updated_at_ms: i64,
}

impl AccountCapacityState {
    /// Create a fresh state with the tier-appropriate prior.
    pub fn new(
        account_id: String,
        account_name: String,
        subscription_tier: Option<String>,
        is_shared: bool,
    ) -> Self {
        let (mu, sigma_sq) = tier_prior(subscription_tier.as_deref());
        let now_ms = chrono::Utc::now().timestamp_millis();
        Self {
            account_id,
            account_name,
            subscription_tier,
            is_shared,
            window_5h: VecDeque::new(),
            proxy_tokens_5h_weighted: 0.0,
            mu,
            sigma_sq,
            n_eff: 0.0,
            ema_proxy_rate: 0.0,
            c_i_hard_lower: 0.0,
            kf_e: 0.0,
            kf_e_dot: 0.0,
            kf_p: [[KF_INIT_VARIANCE, 0.0], [0.0, KF_INIT_VARIANCE]],
            last_kf_predict_ms: 0,
            lag_estimate_ms: LAG_ESTIMATE_MS,
            last_poll_at_ms: None,
            updated_at_ms: now_ms,
        }
    }

    /// Overlay a DB snapshot (posterior + EMA + KF) on top of the tier prior.
    /// Call after `new()` during startup restore.
    ///
    /// Note: KF state (kf_e, kf_e_dot, kf_p) is intentionally NOT restored from DB.
    /// Persisted kf_e values can be tainted by early high-prior observations (before
    /// the posterior converged), producing false positives in suspected_shared().
    /// Starting the KF fresh on each startup is safe — it re-learns external usage
    /// within a few hours.  Two gates prevent premature detections:
    ///   - KF measurement updates only start at n_eff >= 5 (~25 min of polls)
    ///   - suspected_shared() detection only fires at n_eff >= 10 (~50 min of polls),
    ///     giving the KF ~25 minutes of clean updates before any detection is possible.
    pub fn restore_from_db(&mut self, db: &XFactorDbState) {
        self.mu = db.mu;
        self.sigma_sq = db.sigma_sq;
        self.n_eff = db.n_eff;
        self.ema_proxy_rate = db.ema_proxy_rate;
        self.c_i_hard_lower = db.c_i_hard_lower;
        self.updated_at_ms = db.updated_at_ms;
        // KF state reset intentionally (see doc comment).
        self.kf_e = 0.0;
        self.kf_e_dot = 0.0;
        self.kf_p = [[KF_INIT_VARIANCE, 0.0], [0.0, KF_INIT_VARIANCE]];
        self.lag_estimate_ms = db.lag_estimate_ms;
    }

    /// Evict deque entries older than WINDOW_5H_MS from `now_ms`.
    pub fn evict_old(&mut self, now_ms: i64) {
        let cutoff = now_ms - WINDOW_5H_MS;
        while let Some(&(ts, w)) = self.window_5h.front() {
            if ts < cutoff {
                self.window_5h.pop_front();
                self.proxy_tokens_5h_weighted -= w;
            } else {
                break;
            }
        }
        // Prevent floating-point drift below zero
        self.proxy_tokens_5h_weighted = self.proxy_tokens_5h_weighted.max(0.0);
    }

    /// Record a completed request and update rolling window + EMA rate.
    ///
    /// `weighted_tokens` = raw tokens × model_weight — compute before calling.
    pub fn on_request(&mut self, now_ms: i64, weighted_tokens: f64) {
        if weighted_tokens <= 0.0 {
            return;
        }
        self.evict_old(now_ms);
        self.window_5h.push_back((now_ms, weighted_tokens));
        self.proxy_tokens_5h_weighted += weighted_tokens;

        // EMA: treat each completed request as ~1s duration
        self.ema_proxy_rate =
            EMA_ALPHA * weighted_tokens + (1.0 - EMA_ALPHA) * self.ema_proxy_rate;

        self.updated_at_ms = now_ms;
    }

    /// Process a usage poll: update Bayesian posterior + KF for external usage.
    ///
    /// `utilization_pct` must be in 0-100 range (already normalised).
    pub fn on_usage_poll(&mut self, now_ms: i64, utilization_pct: f64) {
        self.evict_old(now_ms);
        self.last_poll_at_ms = Some(now_ms);
        self.updated_at_ms = now_ms;

        let u = utilization_pct;

        // Right-censored: tighten hard lower bound only, skip MAP update
        if u >= CENSORED_U_THRESHOLD {
            if self.proxy_tokens_5h_weighted > 0.0 {
                let lb = self.proxy_tokens_5h_weighted / (u / 100.0);
                self.c_i_hard_lower = self.c_i_hard_lower.max(lb);
            }
            // KF: advance to now but skip measurement update (E is near max, don't update)
            self.kf_predict_to(now_ms);
            return;
        }

        // Too close to zero — LB would be unreliably large
        if u < MIN_U_FOR_LB {
            self.kf_predict_to(now_ms);
            return;
        }

        // Lower-bound estimate: C_i ≥ P_weighted / (U/100)
        let lb = self.proxy_tokens_5h_weighted / (u / 100.0);
        if lb <= 0.0 || !lb.is_finite() {
            self.kf_predict_to(now_ms);
            return;
        }

        // Decay variance for non-stationarity before incorporating new data
        self.sigma_sq = (self.sigma_sq / DECAY_FACTOR).min(4.0);

        // Bayesian inverse-variance update in log-space
        let log_lb = lb.ln();
        let precision_prior = 1.0 / self.sigma_sq;
        let precision_obs = 1.0 / SIGMA_SQ_OBS;
        let precision_post = precision_prior + precision_obs;

        self.mu = (precision_prior * self.mu + precision_obs * log_lb) / precision_post;
        self.sigma_sq = 1.0 / precision_post;

        // Effective sample count: each 90s poll contributes ~0.3 independent windows
        // (90s interval vs 5h window = ~200 polls per window, highly autocorrelated)
        self.n_eff = (self.n_eff + 0.3).min(100.0);

        // Update hard lower bound (conservative: 90% of observed LB)
        self.c_i_hard_lower = self.c_i_hard_lower.max(lb * 0.90);

        // -----------------------------------------------------------------------
        // Kalman Filter update for external usage estimation.
        // -----------------------------------------------------------------------
        // Advance KF to now first.
        self.kf_predict_to(now_ms);

        // Gate: only feed KF once the posterior has stabilised enough (n_eff >= 5,
        // i.e. ~25 minutes of polls at 90s intervals).  Early on, C_estimate is
        // dominated by the prior (which may be 200k for unknown-tier accounts)
        // and systematically inflates E_obs = C_est × U/100 − P_at_lag, poisoning
        // the KF with false external-usage signal.
        if self.n_eff >= 5.0 {
            // Lag-corrected proxy token sum: use P at (now - lag) from the deque.
            // This corrects for the ~90s API reporting lag.
            let lagged_ms = now_ms - self.lag_estimate_ms;
            let p_at_lag = self.p_at_lagged_time(lagged_ms);

            // Observed external usage: E_obs = max(0, C_estimate × U/100 - P_at_lag)
            let c_est = self.c_estimate();
            let e_obs = (c_est * u / 100.0 - p_at_lag).max(0.0);

            // Measurement noise: variance of 1% quantization bucket
            // R = (C_estimate * 0.01)² / 12
            let r = (c_est * 0.01).powi(2) / 12.0;

            self.kf_update(e_obs, r);

            // Same soft floor as in kf_predict_to: allow slight negative so the
            // filter can self-correct (see comment there for rationale).
            self.kf_e = self.kf_e.max(-5_000.0);
            self.kf_e_dot = self.kf_e_dot.max(0.0);
        }
    }

    // -----------------------------------------------------------------------
    // Kalman Filter methods
    // -----------------------------------------------------------------------

    /// Predict KF state forward to `now_ms` (constant-velocity dynamics).
    ///
    /// F = [[1, dt], [0, 1]], Q = [[0, 0], [0, KF_PROCESS_NOISE_RATE * dt]]
    pub fn kf_predict_to(&mut self, now_ms: i64) {
        if self.last_kf_predict_ms == 0 {
            self.last_kf_predict_ms = now_ms;
            return;
        }
        let dt = ((now_ms - self.last_kf_predict_ms).max(0) as f64) / 1000.0;
        if dt < 0.001 {
            return;
        }

        // State transition: [E_new, Ė_new] = [E + Ė*dt, Ė]
        self.kf_e += self.kf_e_dot * dt;
        // kf_e_dot unchanged (constant-velocity model)

        // Covariance prediction: P_new = F * P * F^T + Q
        // Expanded (P symmetric):
        //   P00' = P00 + 2*dt*P01 + dt²*P11
        //   P01' = P01 + dt*P11
        //   P11' = P11 + σ²_rate * dt
        let p00 = self.kf_p[0][0];
        let p01 = self.kf_p[0][1];
        let p11 = self.kf_p[1][1];

        self.kf_p[0][0] = p00 + 2.0 * dt * p01 + dt * dt * p11;
        self.kf_p[0][1] = p01 + dt * p11;
        self.kf_p[1][0] = self.kf_p[0][1]; // symmetric
        self.kf_p[1][1] = p11 + KF_PROCESS_NOISE_RATE * dt;

        // Clip to prevent covariance runaway.
        // P01/P10 are also clamped: they grow via dt*P11 during poll outages and
        // could overflow f64 if left unchecked.
        self.kf_p[0][0] = self.kf_p[0][0].min(1e14);
        self.kf_p[0][1] = self.kf_p[0][1].clamp(-1e14, 1e14);
        self.kf_p[1][0] = self.kf_p[0][1]; // keep symmetric
        self.kf_p[1][1] = self.kf_p[1][1].min(1e14);

        // Allow kf_e to go slightly negative so the filter can absorb downward
        // corrections and self-correct back toward zero.  A hard floor of 0 creates
        // an asymmetric random walk: noise drives kf_e up but can never pull it back
        // down, causing slow upward drift even with no external usage.
        // All callers that compute capacity / shared-score apply .max(0.0) themselves.
        self.kf_e = self.kf_e.max(-5_000.0);
        self.kf_e_dot = self.kf_e_dot.max(0.0);

        self.last_kf_predict_ms = now_ms;
    }

    /// Apply a KF measurement update.
    ///
    /// H = [1, 0] (we observe E directly), R = measurement noise variance.
    fn kf_update(&mut self, e_obs: f64, r: f64) {
        // Innovation: y = z - H*x = e_obs - kf_e
        let y = e_obs - self.kf_e;

        // Innovation covariance: S = P00 + R
        let s = self.kf_p[0][0] + r;
        if s <= 0.0 || !s.is_finite() {
            return;
        }

        // Kalman gain: K = P * H^T / S = [P00/S, P10/S]^T
        let k0 = self.kf_p[0][0] / s;
        let k1 = self.kf_p[1][0] / s;

        // State update: x_new = x + K * y
        self.kf_e += k0 * y;
        self.kf_e_dot += k1 * y;

        // Covariance update: P_new = (I - K*H) * P
        // Expanded:
        //   P00' = P00 * (1 - k0) = P00 * R / S
        //   P01' = P01 * (1 - k0) = P01 * R / S
        //   P10' = P10 - k1*P00
        //   P11' = P11 - k1*P01
        let p00 = self.kf_p[0][0];
        let p01 = self.kf_p[0][1];
        let p11 = self.kf_p[1][1];
        let factor = r / s; // = 1 - k0

        self.kf_p[0][0] = p00 * factor;
        self.kf_p[0][1] = p01 * factor;
        self.kf_p[1][0] = p01 * factor; // symmetric
        self.kf_p[1][1] = p11 - k1 * p01;

        // Clip to keep matrix positive semi-definite
        self.kf_p[0][0] = self.kf_p[0][0].max(0.0);
        self.kf_p[1][1] = self.kf_p[1][1].max(0.0);
    }

    /// Compute the proxy-weighted token sum at a past time (for lag correction).
    ///
    /// Returns the sum of window_5h entries where `window_start ≤ ts ≤ lagged_ms`.
    pub fn p_at_lagged_time(&self, lagged_ms: i64) -> f64 {
        let window_start = lagged_ms - WINDOW_5H_MS;
        self.window_5h
            .iter()
            .filter(|(ts, _)| *ts >= window_start && *ts <= lagged_ms)
            .map(|(_, w)| *w)
            .sum()
    }

    // -----------------------------------------------------------------------
    // Query methods
    // -----------------------------------------------------------------------

    /// Median (p50) capacity estimate: max(exp(μ), hard_lower_bound).
    pub fn c_estimate(&self) -> f64 {
        self.mu.exp().max(self.c_i_hard_lower)
    }

    /// Three-value remaining token estimate: (pessimistic_p5, expected_p50, optimistic_p95).
    ///
    /// Subtracts both proxy tokens and KF-estimated external usage from capacity.
    pub fn remaining_tokens_estimate(&self) -> (f64, f64, f64) {
        let sigma = self.sigma_sq.sqrt();
        let c_p5 = (self.mu - 1.645 * sigma).exp().max(self.c_i_hard_lower);
        let c_p50 = self.c_estimate();
        let c_p95 = (self.mu + 1.645 * sigma).exp();
        let used = self.proxy_tokens_5h_weighted + self.kf_e.max(0.0);
        (
            (c_p5 - used).max(0.0),
            (c_p50 - used).max(0.0),
            (c_p95 - used).max(0.0),
        )
    }

    /// Tokens in the 5h window that will fall off within the next `delta_ms` milliseconds.
    ///
    /// An entry at `ts` falls off at `ts + WINDOW_5H_MS`. It expires in the next `delta_ms`
    /// iff `ts ≤ now + delta_ms - WINDOW_5H_MS`.
    pub fn window_recovery_tokens(&self, now_ms: i64, delta_ms: i64) -> f64 {
        // Entries must be currently in window: ts >= now - WINDOW_5H_MS
        let window_start = now_ms - WINDOW_5H_MS;
        // Entries that expire within next delta_ms: ts <= now + delta_ms - WINDOW_5H_MS
        let cutoff = now_ms + delta_ms - WINDOW_5H_MS;
        self.window_5h
            .iter()
            .filter(|(ts, _)| *ts >= window_start && *ts <= cutoff)
            .map(|(_, w)| *w)
            .sum()
    }

    /// Time-to-exhaustion in minutes using pessimistic estimate + EMA rate.
    /// Returns `f64::INFINITY` when rate is effectively zero.
    pub fn tte_minutes(&self) -> f64 {
        let total_rate = self.ema_proxy_rate + self.kf_e_dot.max(0.0);
        if total_rate <= 0.01 {
            return f64::INFINITY;
        }
        let (r_pessimistic, _, _) = self.remaining_tokens_estimate();
        r_pessimistic / total_rate / 60.0
    }

    /// Improved TTE accounting for rolling-window recovery.
    ///
    /// Tokens expiring in the next 15 minutes are added to pessimistic remaining,
    /// since they effectively free up capacity (no new accounting needed).
    pub fn tte_minutes_with_recovery(&self, now_ms: i64) -> f64 {
        const DELTA_15MIN: i64 = 15 * 60 * 1000;
        let total_rate = self.ema_proxy_rate + self.kf_e_dot.max(0.0);
        if total_rate <= 0.01 {
            return f64::INFINITY;
        }
        let (r_pessimistic, _, _) = self.remaining_tokens_estimate();
        let f_15min = self.window_recovery_tokens(now_ms, DELTA_15MIN);
        let r_adjusted = (r_pessimistic + f_15min).max(0.0);
        r_adjusted / total_rate / 60.0
    }

    /// Utilization percentage (0–100) based on median capacity estimate.
    pub fn utilization_pct(&self) -> f64 {
        let c = self.c_estimate();
        if c <= 0.0 {
            return 0.0;
        }
        ((self.proxy_tokens_5h_weighted + self.kf_e.max(0.0)) / c * 100.0).min(100.0)
    }

    /// X-factor (lo, mid, hi): C_i / C_base_pro at the 5th, 50th, 95th percentiles.
    pub fn x_factor(&self) -> (f64, f64, f64) {
        let sigma = self.sigma_sq.sqrt();
        let c_lo = (self.mu - 1.645 * sigma).exp().max(self.c_i_hard_lower);
        let c_mid = self.c_estimate();
        let c_hi = (self.mu + 1.645 * sigma).exp();
        (c_lo / C_BASE_PRO, c_mid / C_BASE_PRO, c_hi / C_BASE_PRO)
    }

    /// Human-readable confidence label for the X-factor estimate.
    pub fn confidence(&self) -> &'static str {
        if self.n_eff < 1.0 {
            "cold"
        } else if self.n_eff < 3.0 {
            "low"
        } else if self.n_eff < 10.0 {
            "medium"
        } else {
            "high"
        }
    }

    /// Whether new sessions should stop being routed to this account.
    ///
    /// Triggers when: pessimistic_remaining − 5min_drain ≤ safety_buffer.
    pub fn should_stop_assign(&self) -> bool {
        let (r_pessimistic, _, _) = self.remaining_tokens_estimate();
        let total_rate = self.ema_proxy_rate + self.kf_e_dot.max(0.0);
        let drain_5min = total_rate * 300.0;
        (r_pessimistic - drain_5min) <= STOP_ASSIGN_BUFFER
    }

    /// Shared score: ratio of estimated external tokens to proxy tokens (0.0–∞).
    ///
    /// Values > SHARED_SCORE_THRESHOLD (0.50) suggest the account is being used externally.
    pub fn shared_score(&self) -> f64 {
        if self.proxy_tokens_5h_weighted <= 100.0 {
            return 0.0; // not enough data
        }
        self.kf_e.max(0.0) / self.proxy_tokens_5h_weighted
    }

    /// Whether the account is suspected to have external usage (based on KF E estimate).
    ///
    /// Three independent gates:
    /// - n_eff >= 10: KF has been fed stable C_estimate for at least ~50 minutes
    /// - kf_e >= 30_000: absolute minimum to rule out pure noise / small overestimates
    /// - shared_score > 0.50: external tokens clearly dominate proxy usage
    ///
    /// The absolute-kf_e gate is critical because shared_score is a ratio — a tiny
    /// proxy_tokens_5h_weighted makes even small kf_e noise look dominant.
    pub fn suspected_shared(&self) -> bool {
        !self.is_shared
            && self.n_eff >= 10.0
            && self.kf_e >= 30_000.0
            && self.shared_score() > SHARED_SCORE_THRESHOLD
    }

    /// Seconds since the last usage poll, or None if never polled.
    pub fn poll_age_seconds(&self, now_ms: i64) -> Option<f64> {
        self.last_poll_at_ms.map(|t| (now_ms - t).max(0) as f64 / 1000.0)
    }

    /// Snapshot for DB persistence.
    ///
    /// Note: KF fields (kf_e, kf_e_dot, kf_p*) are written for analytics/debugging
    /// purposes but are intentionally NOT restored on startup (see `restore_from_db`).
    pub fn to_db_state(&self) -> XFactorDbState {
        XFactorDbState {
            account_id: self.account_id.clone(),
            mu: self.mu,
            sigma_sq: self.sigma_sq,
            n_eff: self.n_eff,
            ema_proxy_rate: self.ema_proxy_rate,
            c_i_hard_lower: self.c_i_hard_lower,
            updated_at_ms: self.updated_at_ms,
            kf_e: self.kf_e,
            kf_e_dot: self.kf_e_dot,
            kf_p00: self.kf_p[0][0],
            kf_p01: self.kf_p[0][1],
            kf_p10: self.kf_p[1][0],
            kf_p11: self.kf_p[1][1],
            lag_estimate_ms: self.lag_estimate_ms,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_state(tier: Option<&str>) -> AccountCapacityState {
        AccountCapacityState::new(
            "acc1".to_string(),
            "test".to_string(),
            tier.map(String::from),
            false,
        )
    }

    #[test]
    fn prior_pro_tier() {
        let s = make_state(Some("Pro"));
        assert!((s.mu - (88_000.0_f64).ln()).abs() < 1e-6);
    }

    #[test]
    fn prior_max20x_tier() {
        let s = make_state(Some("Max 20x"));
        assert!((s.mu - (880_000.0_f64).ln()).abs() < 1e-6);
    }

    #[test]
    fn on_usage_poll_updates_posterior() {
        let mut s = make_state(Some("Pro"));
        let now = 1_700_000_000_000_i64;

        // Inject 10k proxy tokens into the 5h window
        s.on_request(now - 60_000, 10_000.0);

        // Poll: 50% utilisation → LB = 10000 / 0.5 = 20000
        s.on_usage_poll(now, 50.0);

        assert!(s.n_eff > 0.0);
        assert!(s.c_i_hard_lower > 0.0);

        // c_estimate should be ≥ 20000 * 0.9 after hard-lower update
        assert!(s.c_estimate() >= 18_000.0);
    }

    #[test]
    fn censored_poll_does_not_update_posterior() {
        let mut s = make_state(Some("Pro"));
        let prior_mu = s.mu;
        let now = 1_700_000_000_000_i64;

        s.on_request(now - 10_000, 80_000.0);
        s.on_usage_poll(now, 97.0); // censored

        // mu should be unchanged (censored obs skips MAP)
        assert!((s.mu - prior_mu).abs() < 1e-9);
        // But hard lower bound should be set
        assert!(s.c_i_hard_lower > 0.0);
    }

    #[test]
    fn poll_below_min_u_skipped() {
        let mut s = make_state(Some("Pro"));
        let prior_n_eff = s.n_eff;
        s.on_usage_poll(1_700_000_000_000, 2.0); // U < 5%
        assert_eq!(s.n_eff, prior_n_eff);
    }

    #[test]
    fn remaining_tokens_non_negative() {
        let mut s = make_state(Some("Pro"));
        let now = 1_700_000_000_000_i64;
        // Inject more tokens than the prior median
        s.on_request(now, 200_000.0);
        let (p, e, o) = s.remaining_tokens_estimate();
        assert!(p >= 0.0);
        assert!(e >= 0.0);
        assert!(o >= 0.0);
    }

    #[test]
    fn tte_infinite_when_rate_zero() {
        let s = make_state(Some("Pro"));
        assert_eq!(s.tte_minutes(), f64::INFINITY);
    }

    #[test]
    fn model_weight_values() {
        assert_eq!(model_weight(Some("claude-opus-4")), 4.0);
        assert_eq!(model_weight(Some("claude-haiku-3")), 0.25);
        assert_eq!(model_weight(Some("claude-sonnet-4")), 1.0);
        assert_eq!(model_weight(None), 1.0);
    }

    #[test]
    fn x_factor_pro_baseline() {
        let s = make_state(Some("Pro"));
        let (lo, mid, hi) = s.x_factor();
        // Prior median = 88000, C_base_pro = 88000 → mid ≈ 1.0
        assert!((mid - 1.0).abs() < 0.05);
        assert!(lo < mid);
        assert!(hi > mid);
    }

    #[test]
    fn evict_old_removes_expired_entries() {
        let mut s = make_state(None);
        let base = 1_700_000_000_000_i64;
        // Add token 6h ago (should be evicted)
        s.window_5h.push_back((base - 6 * 3600 * 1000, 10_000.0));
        s.proxy_tokens_5h_weighted = 10_000.0;
        // Add recent token
        s.window_5h.push_back((base - 1000, 5_000.0));
        s.proxy_tokens_5h_weighted += 5_000.0;

        s.evict_old(base);

        assert_eq!(s.window_5h.len(), 1);
        assert!((s.proxy_tokens_5h_weighted - 5_000.0).abs() < 1.0);
    }

    #[test]
    fn kf_predict_advances_state() {
        let mut s = make_state(Some("Pro"));
        let t0 = 1_700_000_000_000_i64;
        s.kf_e = 1000.0;
        s.kf_e_dot = 10.0; // 10 tokens/sec
        s.last_kf_predict_ms = t0;

        // Advance 30 seconds
        s.kf_predict_to(t0 + 30_000);

        // E should have increased by 10 * 30 = 300
        assert!((s.kf_e - 1300.0).abs() < 1.0);
        // Ė unchanged
        assert!((s.kf_e_dot - 10.0).abs() < 0.01);
        // Covariance should have grown
        assert!(s.kf_p[1][1] > KF_INIT_VARIANCE);
    }

    #[test]
    fn kf_update_reduces_uncertainty() {
        let mut s = make_state(Some("Pro"));
        let p00_before = s.kf_p[0][0];

        // Apply an update with a known R
        s.kf_update(5000.0, 64_000.0); // e_obs = 5000, R = 64000

        // P00 should have decreased (innovation reduced uncertainty)
        assert!(s.kf_p[0][0] < p00_before);
        // E should have moved toward e_obs = 5000 from 0
        assert!(s.kf_e > 0.0);
    }

    #[test]
    fn window_recovery_tokens_correct() {
        let mut s = make_state(None);
        let now = 1_700_000_000_000_i64;

        // Add a token block 4h55m ago (will fall off in next 5 minutes)
        let ts_old = now - 5 * 3600 * 1000 + 5 * 60 * 1000; // exactly 5h - 5min ago
        s.window_5h.push_back((ts_old, 50_000.0));
        s.proxy_tokens_5h_weighted = 50_000.0;

        // Add a recent token block
        s.window_5h.push_back((now - 60_000, 10_000.0));
        s.proxy_tokens_5h_weighted += 10_000.0;

        // Recovery in next 10min: the old block should be included
        let f_10min = s.window_recovery_tokens(now, 10 * 60 * 1000);
        assert!((f_10min - 50_000.0).abs() < 1.0);

        // Recovery in next 1min: the old block should NOT be included
        let f_1min = s.window_recovery_tokens(now, 60 * 1000);
        assert!(f_1min < 1.0);
    }

    #[test]
    fn tte_with_recovery_longer_than_basic() {
        let mut s = make_state(Some("Pro"));
        let now = 1_700_000_000_000_i64;

        // Add a big block that expires in ~5min (will add recovery capacity)
        let ts_old = now - 5 * 3600 * 1000 + 5 * 60 * 1000 - 1; // just under 5min until expiry
        s.window_5h.push_back((ts_old, 50_000.0));
        s.proxy_tokens_5h_weighted = 50_000.0;

        // Set a moderate rate so TTE is meaningful
        s.ema_proxy_rate = 100.0; // 100 tokens/sec

        let tte_basic = s.tte_minutes();
        let tte_recovery = s.tte_minutes_with_recovery(now);

        // Recovery TTE should be ≥ basic TTE (more capacity available)
        assert!(tte_recovery >= tte_basic);
    }

    #[test]
    fn shared_score_zero_for_no_external() {
        let mut s = make_state(Some("Pro"));
        let now = 1_700_000_000_000_i64;
        s.on_request(now, 10_000.0);
        // kf_e = 0 → shared_score = 0
        assert_eq!(s.shared_score(), 0.0);
    }

    #[test]
    fn kf_not_updated_when_n_eff_below_gate() {
        let mut s = make_state(Some("Pro"));
        let now = 1_700_000_000_000_i64;

        // Inject proxy tokens so the LB is valid
        s.on_request(now - 60_000, 10_000.0);

        // Single poll → n_eff = 0.3, well below the KF gate of 5.0
        s.on_usage_poll(now, 50.0);
        assert!(s.n_eff < 5.0);

        // KF measurement update should have been gated out
        assert_eq!(s.kf_e, 0.0, "kf_e must stay 0 when n_eff < 5");
        assert_eq!(s.kf_e_dot, 0.0, "kf_e_dot must stay 0 when n_eff < 5");

        // kf_predict_to should still have advanced the clock
        assert!(s.last_kf_predict_ms > 0, "kf_predict_to must still run");
    }

    #[test]
    fn kf_updated_when_n_eff_above_gate() {
        let mut s = make_state(Some("Pro"));
        let now = 1_700_000_000_000_i64;

        s.on_request(now - 60_000, 10_000.0);

        // Force n_eff above the KF gate threshold
        s.n_eff = 5.0;

        // Poll at 50% — C_est(Pro) ≈ 88k, E_obs = 88k*0.5 - 10k = 34k
        s.on_usage_poll(now, 50.0);

        // KF should have received the measurement update
        assert!(s.kf_e > 0.0, "kf_e should be positive after KF update fires");
    }

    #[test]
    fn restore_from_db_resets_kf_state() {
        let mut s = make_state(Some("Pro"));
        let now = 1_700_000_000_000_i64;

        let db = XFactorDbState {
            account_id: "acc1".to_string(),
            mu: s.mu,
            sigma_sq: s.sigma_sq,
            n_eff: 15.0,
            ema_proxy_rate: 42.0,
            c_i_hard_lower: 80_000.0,
            updated_at_ms: now,
            // Tainted KF state that must NOT be restored
            kf_e: 50_000.0,
            kf_e_dot: 5.0,
            kf_p00: 100.0,
            kf_p01: 10.0,
            kf_p10: 10.0,
            kf_p11: 50.0,
            lag_estimate_ms: 120_000,
        };

        s.restore_from_db(&db);

        // KF state must be reset — not restored from DB
        assert_eq!(s.kf_e, 0.0, "kf_e must be reset on restore");
        assert_eq!(s.kf_e_dot, 0.0, "kf_e_dot must be reset on restore");
        assert_eq!(s.kf_p[0][0], KF_INIT_VARIANCE, "kf_p[0][0] must be reset");
        assert_eq!(s.kf_p[0][1], 0.0, "kf_p off-diagonal must be reset");
        assert_eq!(s.kf_p[1][1], KF_INIT_VARIANCE, "kf_p[1][1] must be reset");

        // Non-KF fields should be restored from DB
        assert_eq!(s.n_eff, 15.0);
        assert_eq!(s.ema_proxy_rate, 42.0);
        assert_eq!(s.c_i_hard_lower, 80_000.0);

        // lag_estimate_ms is infrastructure, not KF state — must be restored
        assert_eq!(s.lag_estimate_ms, 120_000);
    }

    #[test]
    fn suspected_shared_requires_all_three_gates() {
        let mut s = make_state(Some("Pro"));
        let now = 1_700_000_000_000_i64;

        // Baseline: set up state that passes all gates
        s.n_eff = 15.0;
        s.kf_e = 50_000.0;
        s.on_request(now, 10_000.0); // proxy_tokens = 10k → shared_score = 5.0

        assert!(s.suspected_shared(), "should detect when all gates pass");

        // Gate 1 fail: n_eff too low
        s.n_eff = 9.9;
        assert!(!s.suspected_shared(), "n_eff < 10 must block detection");
        s.n_eff = 15.0;

        // Gate 2 fail: kf_e below absolute minimum
        s.kf_e = 29_999.0;
        assert!(!s.suspected_shared(), "kf_e < 30000 must block detection");
        s.kf_e = 50_000.0;

        // Gate 3 fail: shared_score <= 0.50
        // With kf_e = 50k, need proxy > 100k for score to drop below 0.50
        s.proxy_tokens_5h_weighted = 200_000.0;
        assert!(!s.suspected_shared(), "shared_score <= 0.50 must block detection");
    }

    #[test]
    fn suspected_shared_skips_already_shared_accounts() {
        let mut s = AccountCapacityState::new(
            "acc1".to_string(),
            "test".to_string(),
            Some("Pro".to_string()),
            true, // is_shared = true
        );
        let now = 1_700_000_000_000_i64;

        // Set up state that would pass all numeric gates
        s.n_eff = 15.0;
        s.kf_e = 50_000.0;
        s.on_request(now, 10_000.0);

        assert!(!s.suspected_shared(), "is_shared accounts must never be flagged");
    }

    #[test]
    fn suspected_shared_kf_e_absolute_minimum_boundary() {
        let mut s = make_state(Some("Pro"));
        let now = 1_700_000_000_000_i64;

        s.n_eff = 15.0;
        s.on_request(now, 10_000.0);

        // Exactly at boundary — should pass (>=)
        s.kf_e = 30_000.0;
        assert!(s.suspected_shared(), "kf_e == 30000 should pass the >= gate");

        // Just below — should fail
        s.kf_e = 29_999.9;
        assert!(!s.suspected_shared(), "kf_e < 30000 should fail the gate");
    }

    #[test]
    fn p_at_lagged_time_excludes_recent() {
        let mut s = make_state(None);
        let now = 1_700_000_000_000_i64;

        // Add tokens at various times
        s.window_5h.push_back((now - 120_000, 5_000.0)); // 2min ago — in lagged window
        s.window_5h.push_back((now - 30_000, 3_000.0));  // 30s ago — after lag

        let lagged = now - 90_000; // 90s lag
        let p = s.p_at_lagged_time(lagged);

        // Only the 2min-ago entry should be in the lagged window
        assert!((p - 5_000.0).abs() < 1.0);
    }
}
