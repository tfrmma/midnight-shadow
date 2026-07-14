use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tracing::warn;

use crate::types::{AppState, OracleSnapshot};

const SIM_BASE_PRICE: f64 = 3_200.0;
const DEVIATION_THRESHOLD: f64 = 0.005;

// Chainlink nodes don't fire deterministically they sample at their own cadence
// and aggregate. Actual update timing is stochastic within [threshold_age, heartbeat].
// We model this as: once deviation threshold is crossed, fire in Uniform[lag_min, lag_max]
// where lag_min = 0.6×lag_secs, lag_max = 1.4×lag_secs.
// This is crude but it's closer to reality than a fixed 180s cliff.
fn stochastic_fire_delay(lag_secs: u64, rng_seed: u64) -> Duration {
    let base = lag_secs as f64;
    let jitter = ((rng_seed % 1000) as f64 / 1000.0 - 0.5) * 0.8 * base;
    Duration::from_secs_f64((base + jitter).max(10.0))
}

pub struct OracleFeed {
    sim: bool,
    lag_secs: u64,
}

impl OracleFeed {
    pub fn new(sim: bool, lag_secs: u64) -> Self {
        Self { sim, lag_secs }
    }

    pub async fn run(self, state: Arc<RwLock<AppState>>) {
        if self.sim {
            self.run_sim(state).await;
        } else {
            self.run_live(state).await;
        }
    }

    async fn run_sim(&self, state: Arc<RwLock<AppState>>) {
        let mut oracle_px = SIM_BASE_PRICE;
        let mut last_update = Instant::now();
        let mut round_id = 1u64;
        let mut threshold_crossed_at: Option<Instant> = None;
        let mut fire_delay: Option<Duration> = None;

        // Seed immediately so dashboard has something on first frame
        state.write().await.oracle = Some(OracleSnapshot {
            price: oracle_px,
            updated_at: last_update,
            round_id,
            eta_secs: None,
        });

        loop {
            tokio::time::sleep(Duration::from_millis(200)).await;

            let cex_px = match state.read().await.cex.as_ref().map(|c| c.mid) {
                Some(p) => p,
                None => continue,
            };

            // Direction-aware: only downward moves create cliff risk for lenders
            let lag_pct = if oracle_px > cex_px {
                (oracle_px - cex_px) / oracle_px
            } else {
                0.0
            };

            // Track when threshold was first breached and pick a stochastic delay
            if lag_pct >= DEVIATION_THRESHOLD && threshold_crossed_at.is_none() {
                threshold_crossed_at = Some(Instant::now());
                let seed = round_id ^ (oracle_px as u64) ^ (cex_px as u64);
                fire_delay = Some(stochastic_fire_delay(self.lag_secs, seed));
            }

            // Price recovered above threshold reset
            if lag_pct < DEVIATION_THRESHOLD * 0.8 && threshold_crossed_at.is_some() {
                threshold_crossed_at = None;
                fire_delay = None;
            }

            let eta = threshold_crossed_at.and_then(|t| {
                let delay = fire_delay?;
                let elapsed = t.elapsed();
                if elapsed >= delay {
                    None // fire is overdue oracle update imminent
                } else {
                    Some((delay - elapsed).as_secs_f64())
                }
            });

            // Fire the oracle update
            let should_fire = threshold_crossed_at
                .zip(fire_delay)
                .map(|(t, d)| t.elapsed() >= d)
                .unwrap_or(false);

            if should_fire {
                oracle_px = cex_px;
                last_update = Instant::now();
                round_id += 1;
                threshold_crossed_at = None;
                fire_delay = None;
            }

            state.write().await.oracle = Some(OracleSnapshot {
                price: oracle_px,
                updated_at: last_update,
                round_id,
                eta_secs: if threshold_crossed_at.is_some() { eta } else { None },
            });
        }
    }

    // TODO: alloy client → AggregatorV3Interface.latestRoundData() per collateral token.
    // Need deployed Midnight market addresses first.
    async fn run_live(&self, _state: Arc<RwLock<AppState>>) {
        warn!("live oracle feed not implemented — pass --sim");
        loop {
            tokio::time::sleep(Duration::from_secs(60)).await;
        }
    }
}
