use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::error;

use crate::types::{AppState, CexSnapshot};

const WS_BASE: &str = "wss://stream.binance.com:9443/ws";
const RECONNECT_MS: u64 = 2_000;

// crash scenario constants — ETH stable at $3,200, drops to $2,650 over 60s.
// positions are calibrated at $3,200 oracle price, so at bottom they're in
// 91-95% shadow LTV range: liquidatable in CEX reality, invisible on-chain.
const PRE_CRASH_PX: f64 = 3_200.0;
const CRASH_BOTTOM_PX: f64 = 2_650.0;
const CYCLE_SECS: f64 = 300.0;

#[derive(Deserialize)]
struct BookTicker {
    #[serde(rename = "b")]
    bid: String,
    #[serde(rename = "a")]
    ask: String,
}

pub struct CexFeed {
    pair: String,
    sim: bool,
}

impl CexFeed {
    pub fn new(pair: String, sim: bool) -> Self {
        Self { pair: pair.to_lowercase(), sim }
    }

    pub async fn run(&self, state: Arc<RwLock<AppState>>) {
        if self.sim {
            self.run_crash_scenario(state).await;
        } else {
            loop {
                if let Err(e) = self.stream(&state).await {
                    error!("cex ws dropped ({e}), retrying in {RECONNECT_MS}ms");
                }
                tokio::time::sleep(Duration::from_millis(RECONNECT_MS)).await;
            }
        }
    }

    async fn run_crash_scenario(&self, state: Arc<RwLock<AppState>>) {
        let start = Instant::now();
        loop {
            let price = crash_price(start.elapsed().as_secs_f64());
            state.write().await.cex = Some(CexSnapshot {
                bid: price * (1.0 - 0.00008),
                ask: price * (1.0 + 0.00008),
                mid: price,
                ts: Instant::now(),
            });
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    async fn stream(&self, state: &Arc<RwLock<AppState>>) -> Result<()> {
        let url = format!("{WS_BASE}/{}@bookTicker", self.pair);
        let (mut ws, _) = connect_async(&url).await.context("ws handshake failed")?;

        while let Some(raw) = ws.next().await {
            match raw {
                Ok(Message::Text(txt)) => {
                    let Ok(t) = serde_json::from_str::<BookTicker>(&txt) else { continue };
                    let (bid, ask): (f64, f64) = match (t.bid.parse(), t.ask.parse()) {
                        (Ok(b), Ok(a)) if b > 0.0 && a > 0.0 => (b, a),
                        _ => continue,
                    };
                    state.write().await.cex = Some(CexSnapshot {
                        bid,
                        ask,
                        mid: (bid + ask) / 2.0,
                        ts: Instant::now(),
                    });
                }
                Ok(Message::Ping(p)) => { let _ = ws.send(Message::Pong(p)).await; }
                Ok(Message::Close(_)) | Err(_) => break,
                _ => {}
            }
        }
        Ok(())
    }
}

fn crash_price(t: f64) -> f64 {
    let phase = t % CYCLE_SECS;
    if phase < 20.0 {
        PRE_CRASH_PX
    } else if phase < 80.0 {
        let p = (phase - 20.0) / 60.0;
        PRE_CRASH_PX - (PRE_CRASH_PX - CRASH_BOTTOM_PX) * p
    } else if phase < 240.0 {
        // partial recovery — still deep in cliff territory
        let p = (phase - 80.0) / 160.0;
        CRASH_BOTTOM_PX + (PRE_CRASH_PX - CRASH_BOTTOM_PX) * 0.12 * p
    } else {
        CRASH_BOTTOM_PX + (PRE_CRASH_PX - CRASH_BOTTOM_PX) * 0.12
    }
}
