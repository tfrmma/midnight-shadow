# Contributing

## License note

The Morpho Midnight contracts (`src/Midnight.sol`) are licensed under BUSL-1.1. This monitor is an independent tool that reads on-chain state and does not incorporate any Morpho contract code. The interfaces and libraries under `src/interfaces` and `src/libraries` in the official repo are dual-licensed under GPL-2.0-or-later.

## Running locally

```bash
cargo run -- --sim
cargo test
```

`markets.toml` must exist at `config/markets.toml` (default) or pass `--markets <path>`.

---

## Adding a market

Edit `config/markets.toml`. Each market needs at least one collateral leg:

```toml
[[markets]]
market_id     = "wBTC/USDC-Dec26"
loan_token    = "USDC"
debt          = 500000.0
maturity_ts   = 1767225600
rcf_threshold = 100.0

[[markets.legs]]
token         = "wBTC"
amount        = 5.0
lltv          = 0.77
cursor        = 0.50
exchange_rate = 30.0    # wBTC/ETH ratio — update this when adding live oracle support
```

**Constraints enforced at startup:**
- `lltv` must be one of the 9 Morpho Blue tiers: `{0.385, 0.5, 0.625, 0.77, 0.86, 0.915, 0.945, 0.965, 0.98}`
- `cursor` must be `0.25` or `0.50`
- `amount` and `exchange_rate` must be positive
- `debt` must be positive

The tool fails hard on invalid config — no silent fallback.

---

## Adding a collateral token type

`exchange_rate` is currently a static multiplier relative to the ETH price feed. This is a known limitation — in production, each token has its own oracle.

When adding a new token in sim mode, set `exchange_rate` to the approximate current ratio vs ETH and note it in a comment. When live oracle support lands (#11), this will be replaced by per-token oracle snapshots.

---

## Connecting the live oracle (when Midnight deploys on mainnet)

The live oracle feed in `src/feeds/oracle.rs` is currently a stub. To implement it:

1. Add `alloy` to `Cargo.toml`
2. In `OracleFeed::run_live()`, initialize an alloy provider with the RPC URL from `Args`
3. For each token in the market config, call `AggregatorV3Interface.latestRoundData()` using the Chainlink feed address for that token
4. Store results in a `HashMap<String, OracleSnapshot>` in `AppState` (tracked in issue #11)
5. Update `ShadowEngine::analyze()` to pull per-token oracle prices from the map instead of a single ETH price

Chainlink feed addresses for mainnet will be available in the Morpho Midnight documentation once the protocol deploys.

---

## Connecting the position indexer (when Midnight deploys on mainnet)

Currently `AppState` is seeded from `markets.toml`. For live monitoring:

1. Subscribe to Midnight contract events: `PositionCreated`, `Supply`, `Borrow`, `Repay`
2. Build and maintain a position book from those events
3. Replace `AppState::new(positions)` with a feed that updates positions dynamically

This is tracked in issue #10.

---

## Running tests

```bash
cargo test
```

19 tests covering `lif_max`, `blended_lif`, `seizure_params`, `post_maturity_lif`, and regression tests that verify sim positions are healthy at oracle price and liquidatable at crash bottom.

If you change `crash_top_px` or `crash_bottom_px` in `markets.toml`, update the regression test constants in `src/types.rs` to match.

---

## Math reference

All formulas are derived from the [Morpho Midnight whitepaper](https://morpho.org/whitepapers/midnight-whitepaper.pdf).

```
maxDebt   = Σ cᵢ · pᵢ · LLTVᵢ                     (eq.3)
LIFmax    = 1 / (1 - γ · (1 - LLTV))               (eq.4)
delta_min = (D_rem - maxDebt_shadow) / (1 - L · LIF)
```

If the whitepaper is updated before mainnet, the formulas in `src/shadow/engine.rs` and `src/types.rs` need to be reviewed against the new spec.
