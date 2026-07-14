use std::time::{Instant, SystemTime, UNIX_EPOCH};

// Liquidation cursor γ per whitepaper: {0.25, 0.50}
#[derive(Debug, Clone)]
pub struct CollateralLeg {
    pub token: String,
    pub amount: f64,
    pub lltv: f64,
    pub cursor: f64,        // γ sets LIFmax per whitepaper eq.4
    pub exchange_rate: f64, // price = eth_px * exchange_rate (1.0 for ETH, 1.07 for wstETH, etc.)
}

impl CollateralLeg {
    /// Official Midnight formula: LIFmax = 1 / (1 − γ·(1 − LLTV))
    pub fn lif_max(&self) -> f64 {
        1.0 / (1.0 - self.cursor * (1.0 - self.lltv))
    }

    pub fn price(&self, eth_px: f64) -> f64 {
        eth_px * self.exchange_rate
    }

    // LLTV-weighted capacity contribution (feeds into Σ cᵢ·pᵢ·LLTVᵢ)
    pub fn max_debt(&self, eth_px: f64) -> f64 {
        self.amount * self.price(eth_px) * self.lltv
    }

    pub fn collateral_value(&self, eth_px: f64) -> f64 {
        self.amount * self.price(eth_px)
    }

    // Downside lag only upward drift doesn't hurt lenders
    pub fn lag_pct(&self, oracle_eth: f64, shadow_eth: f64) -> f64 {
        let op = self.price(oracle_eth);
        let sp = self.price(shadow_eth);
        if sp < op { (op - sp) / op } else { 0.0 }
    }
}

#[derive(Debug, Clone)]
pub struct Position {
    pub market_id: String,
    pub loan_token: String,
    pub debt: f64,
    pub legs: Vec<CollateralLeg>,
    pub maturity_ts: u64,
    pub rcf_threshold: f64, // dust floor in loan token below this, full liq allowed per whitepaper §4.3
}

impl Position {
    // maxDebt = Σ cᵢ·pᵢ·LLTVᵢ  (whitepaper eq.3)
    pub fn max_debt(&self, eth_px: f64) -> f64 {
        self.legs.iter().map(|l| l.max_debt(eth_px)).sum()
    }

    pub fn total_collateral(&self, eth_px: f64) -> f64 {
        self.legs.iter().map(|l| l.collateral_value(eth_px)).sum()
    }

    // health LTV = debt / maxDebt; > 1.0 → liquidatable
    pub fn health_ltv(&self, eth_px: f64) -> f64 {
        let md = self.max_debt(eth_px);
        if md <= 0.0 { f64::INFINITY } else { self.debt / md }
    }

    // bad debt only when collateral value < debt (not just maxDebt)
    pub fn bad_debt(&self, shadow_eth: f64) -> f64 {
        (self.debt - self.total_collateral(shadow_eth)).max(0.0)
    }

    // worst downside lag across all legs the oracle that matters is the one driving the cliff
    pub fn worst_lag_pct(&self, oracle_eth: f64, shadow_eth: f64) -> f64 {
        self.legs.iter()
            .map(|l| l.lag_pct(oracle_eth, shadow_eth))
            .fold(0.0_f64, f64::max)
    }

    pub fn is_overdue(&self) -> bool {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        now > self.maturity_ts
    }

    pub fn max_lltv_tier(&self) -> f64 {
        self.legs.iter().map(|l| l.lltv).fold(0.0_f64, f64::max)
    }
}

#[derive(Debug, Clone)]
pub struct OracleSnapshot {
    pub price: f64,
    pub updated_at: Instant,
    pub round_id: u64,
    pub eta_secs: Option<f64>, // countdown to next stochastic fire; None = monitoring
}

impl OracleSnapshot {
    pub fn age_secs(&self) -> f64 {
        self.updated_at.elapsed().as_secs_f64()
    }
}

#[derive(Debug, Clone)]
pub struct CexSnapshot {
    pub bid: f64,
    pub ask: f64,
    pub mid: f64,
    pub ts: Instant,
}

impl CexSnapshot {
    pub fn spread_bps(&self) -> f64 {
        (self.ask - self.bid) / self.mid * 10_000.0
    }
}

#[derive(Debug, Clone)]
pub struct ShadowAnalysis {
    pub market_id: String,
    pub oracle_ltv: f64,       // debt / maxDebt(oracle) — > 1.0 = liquidatable
    pub shadow_ltv: f64,       // debt / maxDebt(cex)    — > 1.0 = liquidatable in reality
    pub worst_lag_pct: f64,    // max downside oracle lag across legs
    pub latent_bad_debt: f64,  // debt - total_collateral(shadow); only > 0 when truly underwater
    pub min_seizure: f64,      // Δ_min to restore health; may be clipped at remaining_debt
    pub first_touch_mev: f64,  // Δ_min × (LIF - 1) - gas
    pub blended_lif: f64,      // collateral-weighted LIF across legs
    pub cliff_imminent: bool,  // lag > deviation threshold AND shadow liquidatable
    pub full_liq_required: bool,
    pub overdue: bool,
    pub dutch_lif: Option<f64>,  // current LIF in Dutch auction window (post-maturity)
    pub dutch_mev: Option<f64>,  // capturable MEV at current dutch_lif, None if not overdue
    pub lltv_tier: f64,         // max LLTV across legs for risk display
}

#[derive(Debug, Clone, Default)]
pub struct AppState {
    pub cex: Option<CexSnapshot>,
    pub oracle: Option<OracleSnapshot>,
    pub positions: Vec<Position>,
    pub analyses: Vec<ShadowAnalysis>,
}

impl AppState {
    pub fn new(positions: Vec<Position>) -> Self {
        Self { positions, ..Default::default() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leg(lltv: f64, cursor: f64) -> CollateralLeg {
        CollateralLeg { token: "ETH".into(), amount: 1.0, lltv, cursor, exchange_rate: 1.0 }
    }

    // LIFmax = 1 / (1 - γ·(1 - LLTV)) whitepaper eq.4
    #[test]
    fn lif_max_086_050() {
        // 1 / (1 - 0.5 × 0.14) = 1 / 0.93 ≈ 1.07527
        assert!((leg(0.86, 0.50).lif_max() - 1.075_269).abs() < 1e-4);
    }

    #[test]
    fn lif_max_080_050() {
        // 1 / (1 - 0.5 × 0.20) = 1 / 0.90 ≈ 1.11111
        assert!((leg(0.80, 0.50).lif_max() - 1.111_111).abs() < 1e-4);
    }

    #[test]
    fn lif_max_077_025() {
        // 1 / (1 - 0.25 × 0.23) = 1 / 0.9425 ≈ 1.06101
        assert!((leg(0.77, 0.25).lif_max() - 1.061_007).abs() < 1e-4);
    }

    #[test]
    fn lag_pct_downside_only() {
        let l = leg(0.80, 0.50);
        // Price dropped: oracle $3200, cex $2650 → lag = 550/3200
        assert!((l.lag_pct(3200.0, 2650.0) - 550.0 / 3200.0).abs() < 1e-6);
        // Price rose: no cliff risk for lenders
        assert_eq!(l.lag_pct(3200.0, 3400.0), 0.0);
    }

    // ---------- sim position helpers ----------

    fn weeth_pos() -> Position {
        const B: f64 = 3_200.0;
        Position {
            market_id: "test".into(), loan_token: "USDC".into(),
            debt: 20.0 * B * 1.001 * 0.740,
            legs: vec![CollateralLeg { token: "weETH".into(), amount: 20.0, lltv: 0.86, cursor: 0.50, exchange_rate: 1.001 }],
            maturity_ts: 1_759_276_800, rcf_threshold: 100.0,
        }
    }

    fn wsteth_pos() -> Position {
        const B: f64 = 3_200.0;
        Position {
            market_id: "test".into(), loan_token: "USDC".into(),
            debt: 60.0 * B * 1.07 * 0.703,
            legs: vec![CollateralLeg { token: "wstETH".into(), amount: 60.0, lltv: 0.80, cursor: 0.50, exchange_rate: 1.07 }],
            maturity_ts: 1_759_276_800, rcf_threshold: 100.0,
        }
    }

    fn multi_pos() -> Position {
        const B: f64 = 3_200.0;
        Position {
            market_id: "test".into(), loan_token: "USDC".into(),
            debt: (30.0 * B * 0.86 + 20.0 * B * 1.07 * 0.80) * 0.866,
            legs: vec![
                CollateralLeg { token: "ETH".into(),    amount: 30.0, lltv: 0.86, cursor: 0.50, exchange_rate: 1.0  },
                CollateralLeg { token: "wstETH".into(), amount: 20.0, lltv: 0.80, cursor: 0.50, exchange_rate: 1.07 },
            ],
            maturity_ts: 1_759_276_800, rcf_threshold: 100.0,
        }
    }

    // ---------- regression: sim positions at calibration prices ----------

    #[test]
    fn weeth_healthy_at_oracle_price() {
        assert!(weeth_pos().health_ltv(3_200.0) < 1.0);
    }

    #[test]
    fn weeth_liquidatable_at_crash_bottom() {
        assert!(weeth_pos().health_ltv(2_650.0) > 1.0);
    }

    #[test]
    fn weeth_no_bad_debt_at_crash() {
        // Collateral still covers debt at $2,650 liquidatable but not underwater
        assert_eq!(weeth_pos().bad_debt(2_650.0), 0.0);
    }

    #[test]
    fn wsteth_healthy_at_oracle_price() {
        assert!(wsteth_pos().health_ltv(3_200.0) < 1.0);
    }

    #[test]
    fn wsteth_liquidatable_at_crash_bottom() {
        assert!(wsteth_pos().health_ltv(2_650.0) > 1.0);
    }

    #[test]
    fn multi_healthy_at_oracle_price() {
        assert!(multi_pos().health_ltv(3_200.0) < 1.0);
    }

    #[test]
    fn multi_liquidatable_at_crash_bottom() {
        assert!(multi_pos().health_ltv(2_650.0) > 1.0);
    }
}
