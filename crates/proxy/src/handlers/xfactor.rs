//! X-factor analytics handlers — pool capacity, value analysis, and X-factor estimates.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;
use tracing::warn;

use bccf_core::AppState;
use bccf_database::DbPool;

use crate::xfactor::XFactorCache;

// ---------------------------------------------------------------------------
// Break-even constants (from XFACTOR_PLAN.md Part 6)
// ---------------------------------------------------------------------------

/// Blended PAYG rate at 70/30 input/output mix for Sonnet (USD per million tokens).
const PAYG_BLENDED_USD_PER_M: f64 = 6.60;
const PAYG_INPUT_USD_PER_M: f64 = 3.0;
const PAYG_OUTPUT_USD_PER_M: f64 = 15.0;
const PAYG_CACHE_CREATE_USD_PER_M: f64 = 3.75;
const PAYG_CACHE_READ_USD_PER_M: f64 = 0.30;

/// Subscription cost inference by tier (USD/month).
fn infer_subscription_cost(tier: Option<&str>) -> Option<f64> {
    let t = tier?.trim().to_ascii_lowercase();
    let is_team = t.contains("team");
    if t.contains("20x") {
        Some(200.0)
    } else if t.contains("5x") {
        // Team Max 5x = $125/seat, individual Max 5x = $100
        Some(if is_team { 125.0 } else { 100.0 })
    } else if is_team {
        // Team plan without explicit multiplier — assume premium ($125/seat)
        Some(125.0)
    } else if t.contains("max") {
        Some(200.0) // conservative until confirmed
    } else if t.contains("pro") {
        Some(20.0)
    } else if t.contains("enterprise") {
        None // enterprise pricing varies
    } else if t.contains("free") {
        Some(0.0)
    } else {
        None
    }
}

/// Max tokens per month estimate: C_5h * (30d * 24h / 5h) windows.
fn max_tokens_per_month(c_5h: f64) -> f64 {
    c_5h * (30.0 * 24.0 / 5.0)
}

// ---------------------------------------------------------------------------
// GET /api/analytics/pool-capacity
// ---------------------------------------------------------------------------

/// Returns real-time pool capacity state: remaining tokens per account + pool totals.
pub async fn get_pool_capacity(State(state): State<Arc<AppState>>) -> Response {
    let Some(cache) = state.xfactor_cache::<XFactorCache>() else {
        return Json(json!({ "error": "xfactor cache not available" })).into_response();
    };

    let snap = cache.snapshot();

    let mut accounts: Vec<serde_json::Value> = snap
        .values()
        .map(|s| {
            let tte = if s.tte_minutes.is_infinite() {
                serde_json::Value::Null
            } else {
                json!(round2(s.tte_minutes))
            };
            let tte_recovery = if s.tte_minutes_with_recovery.is_infinite() {
                serde_json::Value::Null
            } else {
                json!(round2(s.tte_minutes_with_recovery))
            };
            json!({
                "id": s.account_id,
                "name": s.account_name,
                "subscriptionTier": s.subscription_tier,
                "isShared": s.is_shared,
                "remaining5h": {
                    "pessimistic": (s.remaining_pessimistic as i64).max(0),
                    "expected":    (s.remaining_expected    as i64).max(0),
                    "optimistic":  (s.remaining_optimistic  as i64).max(0),
                },
                "capacityEstimate5h": (s.c_estimate as i64).max(0),
                "proxyTokens5hWeighted": (s.proxy_tokens_5h_weighted as i64).max(0),
                "externalTokens5h": (s.kf_e.max(0.0) as i64),
                "utilization5hPct": round2(s.utilization_pct),
                "tteMinutes": {
                    "current": tte,
                    "withWindowRecovery": tte_recovery,
                },
                "windowRecovery15minTokens": (s.window_recovery_15min as i64).max(0),
                "shouldStopAssign":    s.should_stop_assign,
                "sharedScore":         round3(s.shared_score),
                "suspectedShared":     s.suspected_shared,
                "nEff":                round2(s.n_eff),
                "confidence":          s.confidence,
                "lastPollAgeSeconds":  s.last_poll_age_seconds.map(|v| v as i64),
                "emaProxyRateTokensPerSec": round2(s.ema_proxy_rate),
                "kfExternalRateTokensPerSec": round2(s.kf_e_dot),
            })
        })
        .collect();

    // Sort by remaining_expected descending (most slack first)
    accounts.sort_by(|a, b| {
        let ae = a["remaining5h"]["expected"].as_i64().unwrap_or(0);
        let be = b["remaining5h"]["expected"].as_i64().unwrap_or(0);
        be.cmp(&ae)
    });

    // Pool totals
    let total_remaining_pessimistic: f64 = snap.values().map(|s| s.remaining_pessimistic).sum();
    let total_remaining_expected: f64 = snap.values().map(|s| s.remaining_expected).sum();
    let soonest_tte = snap
        .values()
        .filter(|s| s.ema_proxy_rate > 0.01)
        .filter_map(|s| {
            let t = s.tte_minutes;
            if t.is_finite() {
                Some(t)
            } else {
                None
            }
        })
        .fold(f64::INFINITY, f64::min);

    let soonest_tte_recovery = snap
        .values()
        .filter(|s| s.ema_proxy_rate > 0.01)
        .filter_map(|s| {
            let t = s.tte_minutes_with_recovery;
            if t.is_finite() {
                Some(t)
            } else {
                None
            }
        })
        .fold(f64::INFINITY, f64::min);

    let soonest_tte_json = if soonest_tte.is_infinite() {
        serde_json::Value::Null
    } else {
        json!(round2(soonest_tte))
    };
    let soonest_tte_recovery_json = if soonest_tte_recovery.is_infinite() {
        serde_json::Value::Null
    } else {
        json!(round2(soonest_tte_recovery))
    };

    let computed_at = chrono::Utc::now().to_rfc3339();
    Json(json!({
        "computedAt": computed_at,
        "accounts": accounts,
        "poolTotal": {
            "accountCount": snap.len(),
            "remainingExpected": (total_remaining_expected as i64).max(0),
            "remainingPessimistic": (total_remaining_pessimistic as i64).max(0),
            "soonestTteMinutes": soonest_tte_json,
            "soonestTteWithRecoveryMinutes": soonest_tte_recovery_json,
        }
    }))
    .into_response()
}

// ---------------------------------------------------------------------------
// GET /api/analytics/xfactor
// ---------------------------------------------------------------------------

/// Returns X-factor estimates for all accounts plus C_base estimate.
pub async fn get_xfactor(State(state): State<Arc<AppState>>) -> Response {
    let Some(cache) = state.xfactor_cache::<XFactorCache>() else {
        return Json(json!({ "error": "xfactor cache not available" })).into_response();
    };

    let snap = cache.snapshot();

    // Global C_base: median of Pro accounts with n_eff ≥ 3 (not shared)
    let pro_c_estimates: Vec<f64> = snap
        .values()
        .filter(|s| {
            !s.is_shared
                && s.n_eff >= 3.0
                && s.subscription_tier
                    .as_deref()
                    .map(|t| t.to_ascii_lowercase().contains("pro") && !t.contains("max"))
                    .unwrap_or(false)
        })
        .map(|s| s.c_estimate)
        .collect();

    let (c_base_estimate, c_base_source) = if pro_c_estimates.is_empty() {
        (88_000.0_f64, "prior_default")
    } else {
        let mut sorted = pro_c_estimates.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let median = sorted[sorted.len() / 2];
        (median, "median_of_pro_accounts")
    };

    let mut accounts: Vec<serde_json::Value> = snap
        .values()
        .map(|s| {
            json!({
                "id": s.account_id,
                "name": s.account_name,
                "subscriptionTier": s.subscription_tier,
                "isShared": s.is_shared,
                "suspectedShared": s.suspected_shared,
                "sharedScore": round3(s.shared_score),
                "xFactor": {
                    "lo":  round3(s.x_factor_lo),
                    "mid": round3(s.x_factor_mid),
                    "hi":  round3(s.x_factor_hi),
                },
                "c5hTokens": {
                    "lo":  (s.x_factor_lo  * crate::xfactor::state::C_BASE_PRO) as i64,
                    "mid": (s.c_estimate as i64).max(0),
                    "hi":  (s.x_factor_hi  * crate::xfactor::state::C_BASE_PRO) as i64,
                },
                "externalTokens5h": (s.kf_e.max(0.0) as i64),
                "nEff":       round2(s.n_eff),
                "confidence": s.confidence,
                "lastPollAgeSeconds": s.last_poll_age_seconds.map(|v| v as i64),
            })
        })
        .collect();

    accounts.sort_by(|a, b| {
        a["name"]
            .as_str()
            .unwrap_or("")
            .cmp(b["name"].as_str().unwrap_or(""))
    });

    Json(json!({
        "computedAt":     chrono::Utc::now().to_rfc3339(),
        "cBaseEstimate":  c_base_estimate as i64,
        "cBaseSource":    c_base_source,
        "accounts":       accounts,
    }))
    .into_response()
}

// ---------------------------------------------------------------------------
// GET /api/analytics/value
// ---------------------------------------------------------------------------

/// Returns per-account value analysis: CPM, break-even, ROI vs PAYG.
pub async fn get_value(State(state): State<Arc<AppState>>) -> Response {
    let Some(cache) = state.xfactor_cache::<XFactorCache>() else {
        return Json(json!({ "error": "xfactor cache not available" })).into_response();
    };
    let Some(pool) = state.db_pool::<DbPool>() else {
        return Json(json!({ "error": "database not available" })).into_response();
    };

    let now_ms = chrono::Utc::now().timestamp_millis();
    let since_30d_ms = now_ms - 30 * 86_400_000_i64;
    let since_7d_ms = now_ms - 7 * 86_400_000_i64;

    let conn = match pool.get() {
        Ok(c) => c,
        Err(e) => {
            warn!("get_value: failed to get DB connection: {e}");
            return Json(json!({ "error": "db connection failed" })).into_response();
        }
    };

    let aggregates =
        match bccf_database::repositories::xfactor::value_aggregates(&conn, since_30d_ms) {
            Ok(v) => v,
            Err(e) => {
                warn!("get_value: value_aggregates query failed: {e}");
                return Json(json!({ "error": "query failed" })).into_response();
            }
        };

    let rl_counts = bccf_database::repositories::xfactor::rate_limit_counts_7d(&conn, since_7d_ms)
        .unwrap_or_default()
        .into_iter()
        .collect::<std::collections::HashMap<String, i64>>();

    let snap = cache.snapshot();

    let mut account_values: Vec<serde_json::Value> = Vec::new();

    for agg in &aggregates {
        let Some(xf) = snap.get(&agg.account_id) else {
            continue;
        };

        // Active-time normalization
        let active_start_ms = agg.first_ts_ms.max(since_30d_ms);
        let active_days = ((now_ms - active_start_ms) as f64 / 86_400_000.0).max(0.0);

        let (tokens_30d_equiv, payg_30d_equiv, note) = if active_days >= 3.0 {
            let factor = 30.0 / active_days;
            (
                agg.raw_tokens * factor,
                agg.payg_cost_usd * factor,
                "normalized_to_30d",
            )
        } else {
            (agg.raw_tokens, agg.payg_cost_usd, "insufficient_data")
        };

        // Cost inference
        let monthly_cost_usd_db =
            bccf_database::repositories::xfactor::get_monthly_cost_usd(&conn, &agg.account_id)
                .unwrap_or(0.0);

        let (monthly_cost_usd, cost_source) = if monthly_cost_usd_db > 0.0 {
            (monthly_cost_usd_db, "explicit")
        } else if let Some(inferred) = infer_subscription_cost(xf.subscription_tier.as_deref()) {
            (inferred, "inferred_from_tier")
        } else {
            (0.0, "unknown")
        };

        // CPM variants (only compute if we have cost)
        let cpm_actual = if cost_source != "unknown" && tokens_30d_equiv > 0.0 {
            Some(monthly_cost_usd / (tokens_30d_equiv / 1_000_000.0))
        } else {
            None
        };

        let cpm_payg_equiv = if tokens_30d_equiv > 0.0 {
            Some(payg_30d_equiv / (tokens_30d_equiv / 1_000_000.0))
        } else {
            None
        };

        let max_tokens_per_month = max_tokens_per_month(xf.c_estimate);
        let cpm_theoretical = if cost_source != "unknown" && max_tokens_per_month > 0.0 {
            Some(monthly_cost_usd / (max_tokens_per_month / 1_000_000.0))
        } else {
            None
        };

        let realized_pct = if max_tokens_per_month > 0.0 {
            Some((tokens_30d_equiv / max_tokens_per_month * 100.0).min(100.0))
        } else {
            None
        };

        // Break-even monthly tokens
        let break_even_tokens = if monthly_cost_usd > 0.0 {
            Some((monthly_cost_usd / PAYG_BLENDED_USD_PER_M * 1_000_000.0) as i64)
        } else {
            None
        };

        // Recommendations
        let mut recommendations: Vec<&str> = Vec::new();
        let rl_7d = rl_counts.get(&agg.account_id).copied().unwrap_or(0);

        if xf.n_eff < 1.0 {
            recommendations.push("LOW_CONFIDENCE");
        }
        if rl_7d >= 10 {
            recommendations.push("FREQUENT_ERRORS");
        }
        if xf.should_stop_assign {
            recommendations.push("NEAR_EXHAUSTION_5H");
        }
        if xf.suspected_shared {
            recommendations.push("SUSPECTED_SHARED");
        }
        if cost_source != "unknown" && active_days >= 14.0 {
            if let Some(cpm) = cpm_actual {
                if cpm > 1.25 * PAYG_BLENDED_USD_PER_M {
                    recommendations.push("EXPENSIVE_VS_PAYG");
                } else if cpm < 0.8 * PAYG_BLENDED_USD_PER_M {
                    recommendations.push("GOOD_VALUE");
                }
            }
            if let Some(rpct) = realized_pct {
                if rpct < 20.0 && active_days >= 21.0 {
                    recommendations.push("UNDERUTILIZED");
                }
            }
        }

        account_values.push(json!({
            "id": xf.account_id,
            "name": xf.account_name,
            "subscriptionTier": xf.subscription_tier,
            "isShared": xf.is_shared,
            "activeDays": round2(active_days),
            "note": note,
            "tokens30dEquiv": (tokens_30d_equiv as i64).max(0),
            "requestCount": agg.request_count,
            "monthlyCostUsd":    monthly_cost_usd,
            "costSource":        cost_source,
            "paygCost30dEquiv":  round2(payg_30d_equiv),
            "breakEvenTokens":   break_even_tokens,
            "cpmActual":         cpm_actual.map(round4),
            "cpmPaygEquiv":      cpm_payg_equiv.map(round4),
            "cpmTheoretical":    cpm_theoretical.map(round4),
            "realizedPct":       realized_pct.map(round2),
            "capacityEstimate5h": (xf.c_estimate as i64).max(0),
            "maxTokensPerMonth": (max_tokens_per_month as i64).max(0),
            "xFactorMid":        round3(xf.x_factor_mid),
            "confidence":        xf.confidence,
            "nEff":              round2(xf.n_eff),
            "rateLimitedCount7d": rl_7d,
            "recommendations":   recommendations,
        }));
    }

    // Sort by tokens_30d_equiv descending
    account_values.sort_by(|a, b| {
        let at = a["tokens30dEquiv"].as_i64().unwrap_or(0);
        let bt = b["tokens30dEquiv"].as_i64().unwrap_or(0);
        bt.cmp(&at)
    });

    // Pool-level recommendation
    let total_payg_30d: f64 = aggregates.iter().map(|a| a.payg_cost_usd).sum();
    let total_cost: f64 = account_values
        .iter()
        .map(|v| v["monthlyCostUsd"].as_f64().unwrap_or(0.0))
        .sum();

    let pool_savings = if total_payg_30d > total_cost {
        total_payg_30d - total_cost
    } else {
        0.0
    };

    Json(json!({
        "computedAt":    chrono::Utc::now().to_rfc3339(),
        "paygBlendedRateUsdPerM": PAYG_BLENDED_USD_PER_M,
        "paygRates": {
            "inputUsdPerM":       PAYG_INPUT_USD_PER_M,
            "outputUsdPerM":      PAYG_OUTPUT_USD_PER_M,
            "cacheCreateUsdPerM": PAYG_CACHE_CREATE_USD_PER_M,
            "cacheReadUsdPerM":   PAYG_CACHE_READ_USD_PER_M,
        },
        "accounts": account_values,
        "poolSummary": {
            "totalMonthlyCostUsd":   round2(total_cost),
            "totalPaygEquiv30dUsd":  round2(total_payg_30d),
            "estimatedSavingsUsd":   round2(pool_savings),
        }
    }))
    .into_response()
}

// ---------------------------------------------------------------------------
// GET /api/accounts/{id}/xfactor
// ---------------------------------------------------------------------------

/// Returns the full X-factor detail for a single account.
pub async fn get_account_xfactor(
    State(state): State<Arc<AppState>>,
    Path(account_id): Path<String>,
) -> Response {
    let Some(cache) = state.xfactor_cache::<XFactorCache>() else {
        return Json(json!({ "error": "xfactor cache not available" })).into_response();
    };

    let Some(snap) = cache.get_snapshot(&account_id) else {
        return (
            axum::http::StatusCode::NOT_FOUND,
            Json(json!({ "error": "account not found" })),
        )
            .into_response();
    };

    let (r_p, r_e, r_o) = (
        snap.remaining_pessimistic,
        snap.remaining_expected,
        snap.remaining_optimistic,
    );

    let tte = if snap.tte_minutes.is_infinite() {
        serde_json::Value::Null
    } else {
        json!(round2(snap.tte_minutes))
    };
    let tte_recovery = if snap.tte_minutes_with_recovery.is_infinite() {
        serde_json::Value::Null
    } else {
        json!(round2(snap.tte_minutes_with_recovery))
    };

    Json(json!({
        "id":              snap.account_id,
        "name":            snap.account_name,
        "subscriptionTier": snap.subscription_tier,
        "isShared":        snap.is_shared,
        "suspectedShared": snap.suspected_shared,
        "sharedScore":     round3(snap.shared_score),
        "xFactor": {
            "lo":  round3(snap.x_factor_lo),
            "mid": round3(snap.x_factor_mid),
            "hi":  round3(snap.x_factor_hi),
        },
        "capacityEstimate5h": (snap.c_estimate as i64).max(0),
        "remaining5h": {
            "pessimistic": (r_p as i64).max(0),
            "expected":    (r_e as i64).max(0),
            "optimistic":  (r_o as i64).max(0),
        },
        "proxyTokens5hWeighted": (snap.proxy_tokens_5h_weighted as i64).max(0),
        "externalTokens5h": (snap.kf_e.max(0.0) as i64),
        "externalRateTokensPerSec": round2(snap.kf_e_dot),
        "utilization5hPct": round2(snap.utilization_pct),
        "tteMinutes": {
            "current": tte,
            "withWindowRecovery": tte_recovery,
        },
        "windowRecovery15minTokens": (snap.window_recovery_15min as i64).max(0),
        "shouldStopAssign":    snap.should_stop_assign,
        "emaProxyRateTokensPerSec": round2(snap.ema_proxy_rate),
        "nEff":       round2(snap.n_eff),
        "confidence": snap.confidence,
        "lastPollAgeSeconds": snap.last_poll_age_seconds.map(|v| v as i64),
        "computedAt": chrono::Utc::now().to_rfc3339(),
    }))
    .into_response()
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

fn round3(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}

fn round4(v: f64) -> f64 {
    (v * 10_000.0).round() / 10_000.0
}
