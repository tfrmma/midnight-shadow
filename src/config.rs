use clap::Parser;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "midnight-shadow")]
#[command(about = "Shadow LTV monitor for Morpho Midnight — quantifies latent bad debt before oracle crystallization")]
pub struct Args {
    /// Path to markets config file
    #[arg(short, long, default_value = "config/markets.toml")]
    pub markets: PathBuf,

    /// Binance trading pair (live mode only)
    #[arg(short, long, default_value = "ETHUSDC")]
    pub pair: String,

    /// Simulation mode — programmatic crash scenario, no RPC or WebSocket needed
    #[arg(short, long, default_value_t = true)]
    pub sim: bool,

    /// Mean oracle lag in seconds (stochastic: actual fire time +-40%)
    #[arg(long, default_value_t = 180)]
    pub oracle_lag_s: u64,

    /// Ethereum RPC URL (ignored in sim mode)
    #[arg(long, default_value = "https://eth-mainnet.g.alchemy.com/v2/YOUR_KEY")]
    pub rpc: String,
}
