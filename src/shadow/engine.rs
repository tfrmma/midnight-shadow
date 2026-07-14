use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::RwLock;

use crate::types::{AppState, Position, ShadowAnalysis};

const CHAINLINK_DEVIATION: f64 = 0.005;
const DUTCH_WINDOW_SECS: f64 = 900.0; // 15 minutes per whitepaper §4.4

pub struct ShadowEngine {
    gas_cost_usd: f64,
}

impl ShadowEngine {
    pub fn new(gas_cost_usd: f64) -> Self {
        Self { gas_cost_usd }
    }
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
        let total_col   = pos.total_collateral(cex_eth);
        let (min_sz, full_liq) = seizure_params(pos, cex_eth, bad_debt, lif);

        // When full liq is required, the liquidator can't repay more debt than
        // the collateral can cover at the bonus rate: effective_seizure = min(d_rem, total_col / LIF).
        // Using d_rem directly overestimates MEV when d_rem > total_col / LIF.
        let effective_seizure = if full_liq && lif > 0.0 {
            min_sz.min(total_col / lif)
        } else {
            min_sz
        };

        let dutch_lif_val = post_maturity_lif(pos.maturity_ts, lif);
        let dutch_mev = dutch_lif_val.map(|dl| {
            if dl <= 1.0 || total_col <= 0.0 { return 0.0; }
            let d_rem = pos.debt - bad_debt;
            let dutch_available = d_rem.min(total_col / dl);
            (dutch_available * (dl - 1.0) - self.gas_cost_usd).max(0.0)
        });

        ShadowAnalysis {
            market_id: pos.market_id.clone(),
            oracle_ltv,
            shadow_ltv,
            worst_lag_pct: worst_lag,
            latent_bad_debt: bad_debt,
            min_seizure: min_sz,
            first_touch_mev: (effective_seizure * (lif - 1.0) - self.gas_cost_usd).max(0.0),
            blended_lif: lif,
            cliff_imminent: worst_lag >= CHAINLINK_DEVIATION && shadow_ltv > 1.0,
            full_liq_required: full_liq,
            overdue: pos.is_overdue(),
            dutch_lif: dutch_lif_val,
            dutch_mev,
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
//   (a) Δ_min >= D_remaining partial seizure can't restore health
//   (b) residual collateral after Δ_min < rcf_threshold dust position, full liq allowed
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{CollateralLeg, Position};

    fn eth_pos(amount: f64, debt: f64, lltv: f64, cursor: f64) -> Position {
        Position {
            market_id: "test".into(), loan_token: "USDC".into(), debt,
            legs: vec![CollateralLeg { token: "ETH".into(), amount, lltv, cursor, exchange_rate: 1.0 }],
            maturity_ts: u64::MAX, rcf_threshold: 0.0,
        }
    }

    #[test]
    fn blended_lif_single_leg() {
        let pos = eth_pos(100.0, 50_000.0, 0.86, 0.50);
        // LIFmax(0.86, 0.50) = 1/(1-0.5×0.14) ≈ 1.07527
        assert!((blended_lif(&pos, 3_200.0) - 1.075_27).abs() < 1e-3);
    }

    #[test]
    fn blended_lif_multi_leg_weighted() {
        let pos = Position {
            market_id: "test".into(), loan_token: "USDC".into(), debt: 100_000.0,
            legs: vec![
                CollateralLeg { token: "ETH".into(),    amount: 30.0, lltv: 0.86, cursor: 0.50, exchange_rate: 1.0  },
                CollateralLeg { token: "wstETH".into(), amount: 20.0, lltv: 0.80, cursor: 0.50, exchange_rate: 1.07 },
            ],
            maturity_ts: u64::MAX, rcf_threshold: 0.0,
        };
        // ETH:    30 × 2650 = 79,500  LIF = 1.07527
        // wstETH: 20 × 2650 × 1.07 = 56,710  LIF = 1.11111
        // total = 136,210
        // blended = (79500/136210)×1.07527 + (56710/136210)×1.11111 ≈ 1.090
        assert!((blended_lif(&pos, 2_650.0) - 1.090).abs() < 0.001);
    }

    #[test]
    fn seizure_zero_when_healthy() {
        // maxDebt = 100 × 1000 × 0.80 = 80_000 > debt 50_000
        let pos = eth_pos(100.0, 50_000.0, 0.80, 0.50);
        let lif = blended_lif(&pos, 1_000.0);
        let (delta, full_liq) = seizure_params(&pos, 1_000.0, 0.0, lif);
        assert_eq!(delta, 0.0);
        assert!(!full_liq);
    }

    #[test]
    fn seizure_partial_restores_health() {
        // D/C = 0.85, in the feasible window (LLTV=0.80 < 0.85 < 1/LIF=0.90)
        let pos = eth_pos(100.0, 85_000.0, 0.80, 0.50);
        let lif = blended_lif(&pos, 1_000.0);
        let (delta, full_liq) = seizure_params(&pos, 1_000.0, 0.0, lif);

        assert!(!full_liq);
        assert!(delta > 0.0 && delta < pos.debt);

        // post-seizure health check
        let new_debt    = pos.debt - delta;
        let new_col_val = 100.0 * 1_000.0 - delta * lif;
        let new_max_debt = new_col_val * 0.80;
        assert!(new_debt <= new_max_debt + 1.0);
    }

    #[test]
    fn seizure_full_liq_when_infeasible() {
        // D/C = 0.95 > 1/LIF ≈ 0.90 → partial liq worsens LTV
        let pos = eth_pos(100.0, 95_000.0, 0.80, 0.50);
        let lif = blended_lif(&pos, 1_000.0);
        let (_delta, full_liq) = seizure_params(&pos, 1_000.0, 0.0, lif);
        assert!(full_liq);
    }

    #[test]
    fn post_maturity_none_before_maturity() {
        assert!(post_maturity_lif(u64::MAX, 1.111).is_none());
    }

    #[test]
    fn post_maturity_full_window_returns_lif_max() {
        use std::time::{SystemTime, UNIX_EPOCH};
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        // 20 minutes past maturity — beyond the 15min window
        let lif = post_maturity_lif(now - 1_200, 1.111).unwrap();
        assert!((lif - 1.111).abs() < 1e-4);
    }

    #[test]
    fn post_maturity_halfway_linear_ramp() {
        use std::time::{SystemTime, UNIX_EPOCH};
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        // 450s = 7.5 min = halfway through 15min window
        // expected: 1.0 + (1.111 - 1.0) × 0.5 = 1.0555
        let lif = post_maturity_lif(now - 450, 1.111).unwrap();
        assert!((lif - 1.0555).abs() < 0.005);
    }
}
