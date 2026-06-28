use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::RwLock;

use crate::types::{AppState, Position, ShadowAnalysis};

const CHAINLINK_DEVIATION: f64 = 0.005;
const GAS_COST_USD: f64 = 18.0;
const DUTCH_WINDOW_SECS: f64 = 900.0; // 15 minutes per whitepaper §4.4

pub struct ShadowEngine;

impl Default for ShadowEngine {
    fn default() -> Self {
        Self
    }
}

impl ShadowEngine {
    pub async fn run(&self, state: Arc<RwLock<AppState>>) {
        loop {
            tokio::time::sleep(Duration::from_millis(100)).await;
            self.tick(&state).await;
        }
    }

    async fn tick(&self, state: &Arc<RwLock<AppState>>) {
        let (positions, cex, oracle) = {
            let s = state.read().await;
            (s.positions.clone(), s.cex.clone(), s.oracle.clone())
        };

        let (Some(cex), Some(oracle)) = (cex, oracle) else { return };

        let analyses = positions
            .iter()
            .map(|p| self.analyze(p, cex.mid, oracle.price))
            .collect();

        state.write().await.analyses = analyses;
    }

    fn analyze(&self, pos: &Position, cex_eth: f64, oracle_eth: f64) -> ShadowAnalysis {
        let oracle_ltv  = pos.health_ltv(oracle_eth);
        let shadow_ltv  = pos.health_ltv(cex_eth);
        let worst_lag   = pos.worst_lag_pct(oracle_eth, cex_eth);
        let bad_debt    = pos.bad_debt(cex_eth);
        let lif         = blended_lif(pos, cex_eth);
        let (min_sz, full_liq) = seizure_params(pos, cex_eth, bad_debt, lif);

        ShadowAnalysis {
            market_id: pos.market_id.clone(),
            oracle_ltv,
            shadow_ltv,
            worst_lag_pct: worst_lag,
            latent_bad_debt: bad_debt,
            min_seizure: min_sz,
            first_touch_mev: (min_sz * (lif - 1.0) - GAS_COST_USD).max(0.0),
            blended_lif: lif,
            cliff_imminent: worst_lag >= CHAINLINK_DEVIATION && shadow_ltv > 1.0,
            full_liq_required: full_liq,
            overdue: pos.is_overdue(),
            dutch_lif: post_maturity_lif(pos.maturity_ts, lif),
            lltv_tier: pos.max_lltv_tier(),
        }
    }
}

// Blended LIF weighted by shadow collateral value contribution.
// This is the effective liquidation incentive a liquidator would receive
// assuming proportional seizure across legs.
fn blended_lif(pos: &Position, shadow_eth: f64) -> f64 {
    let total = pos.total_collateral(shadow_eth);
    if total <= 0.0 { return 1.0; }
    pos.legs.iter()
        .map(|l| (l.collateral_value(shadow_eth) / total) * l.lif_max())
        .sum()
}

// Minimum seizure Δ to restore health, using blended multi-collateral values.
//
// maxDebt after seizure: Σ LLTVᵢ·(cᵢ - seized_i) = max_debt - blended_L × total_seized_value
// Health restored when: D - Δ ≤ max_debt_shadow - blended_L × Δ × LIF
//
// Solving: Δ_min = (D_remaining - max_debt_shadow) / (1 - blended_L × LIF)
//
// Infeasible (full liq required) when:
//   (a) Δ_min >= D_remaining — partial seizure can't restore health
//   (b) residual collateral after Δ_min < rcf_threshold — dust position, full liq allowed
fn seizure_params(pos: &Position, shadow_eth: f64, bad_debt: f64, lif: f64) -> (f64, bool) {
    let max_d  = pos.max_debt(shadow_eth);
    let total_c = pos.total_collateral(shadow_eth);
    let d_rem  = pos.debt - bad_debt;

    if d_rem <= max_d {
        return (0.0, false); // healthy post-crystallization
    }

    let bl    = max_d / total_c; // blended LLTV
    let denom = 1.0 - bl * lif;

    if denom <= 0.0 {
        return (d_rem, true); // LLTV × LIF ≥ 1: rare config, needs full liq
    }

    let delta = (d_rem - max_d) / denom;

    if delta >= d_rem {
        return (d_rem, true); // infeasible partial
    }

    // dust check: if residual collateral after seizure < rcf_threshold, full liq allowed
    let residual_col = total_c - delta * lif;
    let dust_triggered = residual_col < pos.rcf_threshold;

    (delta.max(0.0), dust_triggered)
}

// Dutch auction: after maturity, LIF starts at 1.0 and ramps to LIFmax over 15 minutes.
// Returns None if not yet overdue.
fn post_maturity_lif(maturity_ts: u64, lif_max: f64) -> Option<f64> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    if now <= maturity_ts { return None; }

    let elapsed = (now - maturity_ts) as f64;
    let progress = (elapsed / DUTCH_WINDOW_SECS).min(1.0);
    Some(1.0 + (lif_max - 1.0) * progress)
}
