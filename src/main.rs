mod config;
mod feeds;
mod markets;
mod shadow;
mod types;
mod ui;

use anyhow::Result;
use clap::Parser;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::info;

use config::Args;
use feeds::{cex::CexFeed, oracle::OracleFeed};
use shadow::engine::ShadowEngine;
use types::AppState;
use ui::dashboard::run_dashboard;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter("midnight_shadow=info")
        .init();

    let args = Args::parse();

    let cfg = markets::load(&args.markets)?;
    let positions = markets::into_positions(&cfg);

    info!(
        "midnight-shadow | markets={} sim={} oracle_lag_mean={}s",
        positions.len(), args.sim, args.oracle_lag_s
    );

    let state = Arc::new(RwLock::new(AppState::new(positions)));

    let cex    = CexFeed::new(args.pair.clone(), args.sim, cfg.sim.clone());
    let oracle = OracleFeed::new(args.sim, args.oracle_lag_s);
    let engine = ShadowEngine::new(cfg.engine.gas_cost_usd);

    let h1 = tokio::spawn({ let s = state.clone(); async move { cex.run(s).await } });
    let h2 = tokio::spawn({ let s = state.clone(); async move { oracle.run(s).await } });
    let h3 = tokio::spawn({ let s = state.clone(); async move { engine.run(s).await } });

    run_dashboard(state).await?;

    h1.abort();
    h2.abort();
    h3.abort();

    Ok(())
}
