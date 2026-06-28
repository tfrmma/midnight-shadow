use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::path::Path;

use crate::types::{CollateralLeg, Position};

const VALID_LLTV_TIERS: &[f64] = &[0.385, 0.5, 0.625, 0.77, 0.86, 0.915, 0.945, 0.965, 0.98];
const LLTV_EPS: f64 = 0.001;

#[derive(Debug, Deserialize, Clone)]
pub struct Config {
    pub sim: SimConfig,
    pub engine: EngineConfig,
    pub markets: Vec<MarketEntry>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct SimConfig {
    pub crash_top_px: f64,
    pub crash_bottom_px: f64,
    pub cycle_secs: f64,
}

#[derive(Debug, Deserialize, Clone)]
pub struct EngineConfig {
    pub gas_cost_usd: f64,
}

#[derive(Debug, Deserialize, Clone)]
pub struct MarketEntry {
    pub market_id: String,
    pub loan_token: String,
    pub debt: f64,
    pub maturity_ts: u64,
    pub rcf_threshold: f64,
    pub legs: Vec<LegEntry>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct LegEntry {
    pub token: String,
    pub amount: f64,
    pub lltv: f64,
    pub cursor: f64,
    pub exchange_rate: f64,
}

pub fn load(path: &Path) -> Result<Config> {
    let raw = std::fs::read_to_string(path).with_context(|| {
        format!(
            "markets config not found at '{}'\n\
             Create one based on the example in the repository or pass --markets <path>.",
            path.display()
        )
    })?;

    let cfg: Config = toml::from_str(&raw).context("failed to parse markets config")?;
    validate(&cfg)?;
    Ok(cfg)
}

pub fn into_positions(cfg: &Config) -> Vec<Position> {
    cfg.markets
        .iter()
        .map(|m| Position {
            market_id: m.market_id.clone(),
            loan_token: m.loan_token.clone(),
            debt: m.debt,
            legs: m
                .legs
                .iter()
                .map(|l| CollateralLeg {
                    token: l.token.clone(),
                    amount: l.amount,
                    lltv: l.lltv,
                    cursor: l.cursor,
                    exchange_rate: l.exchange_rate,
                })
                .collect(),
            maturity_ts: m.maturity_ts,
            rcf_threshold: m.rcf_threshold,
        })
        .collect()
}

fn validate(cfg: &Config) -> Result<()> {
    if cfg.markets.is_empty() {
        bail!("markets.toml: no markets defined");
    }

    if cfg.sim.crash_bottom_px >= cfg.sim.crash_top_px {
        bail!("sim.crash_bottom_px must be less than sim.crash_top_px");
    }

    if cfg.sim.cycle_secs <= 0.0 {
        bail!("sim.cycle_secs must be positive");
    }

    if cfg.engine.gas_cost_usd < 0.0 {
        bail!("engine.gas_cost_usd must be non-negative");
    }

    for m in &cfg.markets {
        if m.legs.is_empty() {
            bail!("market '{}': no collateral legs defined", m.market_id);
        }
        if m.debt <= 0.0 {
            bail!("market '{}': debt must be positive", m.market_id);
        }
        if m.rcf_threshold < 0.0 {
            bail!("market '{}': rcf_threshold must be non-negative", m.market_id);
        }

        for leg in &m.legs {
            let valid_lltv = VALID_LLTV_TIERS
                .iter()
                .any(|&t| (t - leg.lltv).abs() < LLTV_EPS);

            if !valid_lltv {
                bail!(
                    "market '{}', leg '{}': LLTV {:.3} is not a valid Morpho Blue tier\n\
                     valid tiers: {:?}",
                    m.market_id,
                    leg.token,
                    leg.lltv,
                    VALID_LLTV_TIERS
                );
            }

            if (leg.cursor - 0.25).abs() > LLTV_EPS && (leg.cursor - 0.50).abs() > LLTV_EPS {
                bail!(
                    "market '{}', leg '{}': cursor must be 0.25 or 0.50, got {}",
                    m.market_id,
                    leg.token,
                    leg.cursor
                );
            }

            if leg.amount <= 0.0 {
                bail!(
                    "market '{}', leg '{}': amount must be positive",
                    m.market_id,
                    leg.token
                );
            }

            if leg.exchange_rate <= 0.0 {
                bail!(
                    "market '{}', leg '{}': exchange_rate must be positive",
                    m.market_id,
                    leg.token
                );
            }
        }
    }

    Ok(())
}
