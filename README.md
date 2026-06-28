# midnight-shadow

> **Work in progress.** Architecture and math will be revised as the Midnight protocol matures toward mainnet. Formulas are derived from the current whitepaper: implementation details may change before launch. Track open issues for known limitations.

Real-time shadow LTV monitor for [Morpho Midnight](https://morpho.org/midnight) markets.

Quantifies positions that are liquidatable in CEX reality but invisible to the protocol: the gap between what oracles report and what markets are actually pricing, before crystallization occurs.

```
MIDNIGHT SHADOW MONITOR  :  latent bad debt quantifier  :  Morpho Midnight
 CEX (Binance)               Oracle (Chainlink sim) : NO staleness check on-chain
  mid   $2,651.82              price  $3,200.00
  bid   $2,651.56              age    214s
  ask   $2,652.08              round  #1
  sprd  1.9 bps                eta    OVERDUE, fire imminent

 Shadow Position Analysis  [h-LTV = debt / maxDebt = debt / Σ cᵢ·pᵢ·LLTVᵢ]
 Market                Tier    LIF     Oracle h-LTV  Shadow h-LTV  Lag↓    MEV Est.     Status
 weETH/USDC-Sep26      0.860  1.111x  86.01%        103.85%       17.13%  $1,891        CLIFF, LIQ PENDING
 wstETH/USDC-Sep26     0.800  1.111x  87.87%        106.47%       17.13%  $5,614        CLIFF, LIQ PENDING
 ETH+wstETH/USDC-Sep26 0.860  1.099x  88.20%        106.70%       17.13%  $9,218        CLIFF, LIQ PENDING
```

Oracle is overdue. Three positions are liquidatable in shadow reality, $16.7K in first-touch MEV on the table, but the protocol sees none of it. When the oracle fires, this window is measured in blocks.

---

## The core problem

Morpho Midnight has **no staleness check on oracle prices**. The protocol calls the oracle and uses whatever is returned, with no validation of when the price was last updated. This is a deliberate design choice: Midnight outsources all risk parameterization to market creators and curators.

The consequence: during a fast move, the protocol's view of collateral value can be materially stale for minutes. Two parallel realities coexist:

```
oracle_ltv:   debt / Σ cᵢ · oracle_price_i · LLTV_i   <- what the protocol enforces
shadow_ltv:   debt / Σ cᵢ · cex_price_i · LLTV_i      <- economic reality
```

When `shadow_ltv > 1.0` and `oracle_ltv <= 1.0`, a position is liquidatable right now in reality but the contract refuses to act on it. This tool quantifies how much of that gap exists across a market at any given moment.

---

## Crystallization

The moment a liquidator first touches an underwater position in Midnight, the full excess debt is **immediately and proportionally distributed** across all lenders in that market. No gradual realization, no exit window: the loss moves from unrealized to realized in a single transaction, triggered by a third party.

This is the P&L equivalent of moving from mark-to-model to mark-to-market: the loss existed before, but now it's booked.

The first-touch transaction is therefore disproportionately valuable: it triggers crystallization and locks in the seizure discount before any competitor can act.

---

## Math

**Health LTV** (the metric Midnight actually uses, not a simple debt/collateral ratio):

```
maxDebt   = Σ cᵢ · pᵢ · LLTVᵢ           (whitepaper eq.3)
h-LTV     = debt / maxDebt
liquidatable when h-LTV > 1.0
```

**Liquidation Incentive Factor** (whitepaper eq.4):

```
LIFmax(LLTVᵢ, γᵢ) = 1 / (1 - γᵢ · (1 - LLTVᵢ))

γ ∈ {0.25, 0.50}: liquidation cursor, per collateral leg

Examples:
  LLTV=0.86, γ=0.50  ->  LIF = 1/(1-0.5×0.14) = 1.111  (11.1% bonus)
  LLTV=0.80, γ=0.50  ->  LIF = 1/(1-0.5×0.20) = 1.111  (11.1% bonus)
  LLTV=0.945, γ=0.25 ->  LIF = 1/(1-0.25×0.055) = 1.014 (1.4% bonus)
```

Multi-collateral positions use a blended LIF weighted by shadow collateral value contribution. This assumes proportional seizure across legs: in practice, liquidators target the highest-LIF leg first. See open issues.

**Minimum seizure** to restore health (post-crystallization):

```
delta_min       = (D_remaining - maxDebt_shadow) / (1 - blended_LLTV · LIF)
first_touch_mev = delta_min · (LIF - 1) - gas
```

**Infeasibility:** when `delta_min >= D_remaining` or residual collateral after seizure falls below `rcfThreshold` (dust floor), full liquidation is allowed per whitepaper §4.3. These positions show `FULL` in the MEV column.

**Oracle lag direction:** only downward price moves create cliff risk for lenders. If the oracle lags behind a rally, collateral is worth more than the protocol knows: no risk. The `Lag↓` column shows downward divergence only.

**Dutch auction:** after maturity, any position with outstanding debt is liquidatable regardless of health. LIF starts at 1.0 and ramps linearly to `LIFmax` over 15 minutes (whitepaper §4.4). Positions in this window show `DUTCH Nx` in the MEV column.

**Stochastic oracle model:** Chainlink nodes sample and aggregate asynchronously. The sim models oracle fire time as `Uniform[0.6×lag, 1.4×lag]` after the deviation threshold is crossed, which is more realistic than a fixed cliff. The ETA countdown reflects this uncertainty.

---

## Architecture

```
sim mode: programmatic crash scenario ETH $3,200 -> $2,650 over 60s (5-min cycle)
live mode: Binance bookTicker WebSocket
     |
     v
CexFeed --------------------------------------------------------------+
                                                                      |
OracleFeed                                                            |
 sim:  fixed at $3,200, stochastic fire after deviation threshold     |
 live: stub (Midnight not yet on mainnet)                             |
     |                                                                |
     +----------------------------+-----------------------------------+
                                  v
                        AppState (Arc<RwLock<_>>)
                                  |
                    ShadowEngine (100ms tick)
                                  |
              per position:
                oracle_ltv, shadow_ltv, worst_lag_pct,
                latent_bad_debt, delta_min, first_touch_mev,
                blended_lif, cliff_imminent,
                full_liq_required, dutch_lif, lltv_tier
                                  |
                                  v
                         ratatui TUI dashboard
```

---

## Sim positions

Three positions calibrated at oracle base $3,200. At crash bottom ($2,650):

| Market | Collateral | LLTV | γ | LIF | Oracle h-LTV | Shadow h-LTV |
|---|---|---|---|---|---|---|
| weETH/USDC-Sep26 | 20 weETH (x1.001) | 0.86 | 0.50 | 1.111 | ~86% | ~104% |
| wstETH/USDC-Sep26 | 60 wstETH (x1.07) | 0.80 | 0.50 | 1.111 | ~88% | ~106% |
| ETH+wstETH/USDC-Sep26 | 30 ETH + 20 wstETH | 0.86/0.80 | 0.50 | ~1.099 | ~88% | ~107% |

The third position demonstrates multi-collateral blended LIF. All use LLTV tiers from Morpho Blue's fixed set.

---

## Usage

```bash
# sim mode: crash scenario runs automatically
cargo run -- --sim

# adjust mean oracle lag (actual fire is stochastic +-40%)
cargo run -- --sim --oracle-lag-s 180

# live mode (Midnight not yet on mainnet)
cargo run -- --rpc https://eth-mainnet.g.alchemy.com/v2/YOUR_KEY --pair ETHUSDC
```

Keys: `up/down` navigate, `q` quit.

---

## Known limitations

- Gas cost hardcoded at $18 USDC. Should be dynamic via `eth_gasPrice`.
- Sim positions treat weETH/wstETH exchange rates as static. In production each has its own on-chain oracle: stale exchange rates were the direct attack vector in the rsETH/Aave contagion (April 2026).
- Live oracle feed is a stub. Will use alloy + `AggregatorV3Interface` once Midnight has mainnet market addresses.
- MEV estimates assume no competition. In a live cliff event, the first-touch block will be a private bundle race via Flashbots/MEV-Boost.

Research and monitoring tool, not a production liquidation bot.

---

## References

- [Morpho Midnight Whitepaper](https://morpho.org/whitepapers/midnight-whitepaper.pdf)
- [morpho-org/midnight](https://github.com/morpho-org/midnight)
