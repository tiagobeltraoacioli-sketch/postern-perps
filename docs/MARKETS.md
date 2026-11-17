# PosternDEX Perpetuals: markets and risk parameters (v0.2.1, 2026-11-17)

UNAUDITED prototype work by agent 392 (continuing 354). Nothing here is legal advice; whether any market may be
offered anywhere is for counsel. Machine-readable files: `compliance/markets.json` (per market) and
`compliance/risk.json` (global, fee tiers, insurance fund, backstop, ADL, rules R-1..R-9). The rules are checked by
`compliance/tools/check_params.py` (24 tests: `python3 -I -m unittest discover -s compliance/tools -p 'test_*.py'`).

Units and names follow `docs/SPEC.md` v0.1 (agent 388): fixed scales (price 1e-8 USD, size 1e-8 base, USDC 1e-6),
fractions in ppm, funding rates in `RATE_SCALE` 1e9, field names equal to SPEC section 10 (Genesis). Values are
decimal strings converted exactly (S-1.2); `priceDecimals` / `sizeDecimals` are display only and equal the digits
of `tick` / `step` (checked, R-7).

## 1. Launch markets

| Market | id | tick | step | min size (~USD) | max order | OI cap (base / ~USD) | max leverage |
|---|---|---|---|---|---|---|---|
| BTC-USD | 1 | 0.5 | 0.0001 | 0.0002 (12.97) | 10 | 30 / 1,945,745 | 20 |
| ETH-USD | 2 | 0.05 | 0.001 | 0.006 (10.97) | 200 | 500 / 914,210 | 12 |
| SOL-USD | 3 | 0.005 | 0.01 | 0.14 (10.19) | 2500 | 6500 / 472,940 | 10 |

USD figures use the reference prices of 2026-11-17 04:00 UTC (newest Coinbase 1h close in the recorded data:
BTC 64,858.16; ETH 1,828.42; SOL 72.76). Decided (technical): ids, tick and step are those of 351's fixture
(`node/fixtures/markets.json`) and 391's pinned table (`frontend/config/markets.json`), so no signed field moves;
minimum size is the smallest multiple of `step` worth at least 10 USD (spam bound; benchmark: Hyperliquid's 10 USD
minimum order is not re-read today, so it is not cited as a source). Changed from the fixture (NOT pinned fields
for signing, but 391 pins `minOrderSize`, `maxOrderSize`, `maxLeverage` and the tiers: re-pin from this file):
min size, max order, OI cap, leverage and tiers.

Why only three markets: the oracle (394) needs at least three independent USD spot venues per market; BTC, ETH and
SOL have them (394's `SOURCES.md`). Any further market is a new listing with its own stress measurement (section 4).

## 2. Margin tiers

Continuous schedule (SPEC 6.2): each tier applies to the part of the position notional inside it.
Decided (technical): maintenance = half of initial (R-2), the convention of Hyperliquid ("maintenance_margin_rate
(tier = n) = (Initial Margin Rate at Maximum leverage at tier n) / 2", source S-HL-MT).

| Market | tier floor (USDC) | IMF | MMF | leverage in tier |
|---|---|---|---|---|
| BTC-USD | 0 / 500,000 / 2,000,000 | 5% / 10% / 20% | 2.5% / 5% / 10% | 20 / 10 / 5 |
| ETH-USD | 0 / 250,000 / 1,000,000 | 8.3333% / 10% / 20% | 4.1667% / 5% / 10% | 12 / 10 / 5 |
| SOL-USD | 0 / 100,000 / 400,000 | 10% / 20% / 33.3334% | 5% / 10% / 16.6667% | 10 / 5 / 3 |

Benchmarks (read 2026-11-17): Hyperliquid BTC 40x to 150M, ETH 25x to 100M, SOL 20x to 70M (S-HL-MT). We are
deliberately lower: a new venue has a thin book, a small insurance fund and a backstop of its own capital only, and
SPEC S-4.10 stops liquidations while a market is halted by the oracle's jump rule (394 section 6: a move above
`jumpBps` in one round halts for at least `confirmRounds` rounds). The tier-0 maintenance fraction is sized by
rule R-6 from the measured one-hour stress (section 4): ETH's measured 6.89% one-hour adverse move (Kraken,
2026-11-08 21:00 UTC) is why ETH is 12x, not 20x.

## 3. Fees and funding

| Item | Value | Benchmarks (read 2026-11-17) |
|---|---|---|
| Taker / maker, tier 0 | 4.5 bps / 1.5 bps (450 / 150 ppm) | Hyperliquid base 0.045% / 0.015% (S-HL-FEES); dYdX tier 1 5.0 / 1.0 bps (S-DYDX-FEES) |
| Volume tiers (14-day) | >5M 4.0/1.2; >25M 3.5/0.8; >100M 3.0/0.4; >500M 2.8/0 | Hyperliquid tiers 1 to 4 identical (S-HL-FEES); dYdX 4.5/1.0 at 1M down to 2.5/-1.1 at 200M |
| Maker rebates | none | Hyperliquid base-rate maker 0.000% from tier 4 (>500M), no negative base rate in the table (S-HL-FEES); dYdX rebates from 100M (S-DYDX-FEES) |
| Deposit fee | 0 | BRIEF-PERPS |
| Withdrawal fee | 1 USDC flat | covers the vault claim's L2 gas; the L2 gas cost in USD is NOT measured (the L2 is not live): owner input |
| Liquidation fee | 0.5% of liquidated notional (5000 ppm), to the insurance fund, from remaining equity only | Hyperliquid: none through the book, the backstop keeps the maintenance margin (S-HL-LIQ) |
| Funding interval | 1 hour | Hyperliquid "paid every hour" (S-HL-FUND) |
| Interest component | 0.00125% per hour (`interest` 12500) | Hyperliquid 0.01% per 8 h = 0.00125% per hour (S-HL-FUND) |
| Clamp of (interest - premium) | +-0.00625% per hour (`fundingClamp` 62500) | Hyperliquid clamp 0.05% on the 8-hour rate, paid 1/8 per hour (S-HL-FUND) = 0.00625% per hour |
| Funding cap | 0.05% per hour (`maxFundingRate` 500,000 = SPEC v0.1 default) | Hyperliquid 0.5% per hour (S-HL-FUND) |

Middle ground on revenue (owner rule): the base tier equals the leading benchmark, the volume tiers follow it, no
rebates (a rebate is protocol revenue paid out), and every protocol fee accrues to the `PROTOCOL_FEE` account swept
daily (when at least 100 USDC) to `0x775a993F09Bd80aeCF8F8c2ADe0C84158E3a0544` on chain 700770 (SPEC S-9.3; R-9).

Decided (technical), the funding cap: rule R-5, `24 * maxFundingRate < IMF - MMF` of tier 0, so a position opened
at full leverage cannot be pushed from its initial to its maintenance requirement by capped funding alone in less
than one day (0.05%/h satisfies it with a margin of two: 1.2% per day against BTC's 2.5%); Hyperliquid's 0.5%/h
would do it in 5 hours at our BTC tiers. The cost: in a persistent premium the funding cap is reached sooner and the
premium closes more slowly.

Revision 0.2.1 (same day): the first push (`1da5df7`) proposed 0.1%/h. Reviewer 395 (cross-area 04:31 UTC)
objected that R395-16 (premium samples read from a book the sequencer can quote risk-free) scales with the cap, so
the cap must not be looser than SPEC's default until the sampling point is fixed. Accepted: 0.05%/h; raising it is
a later change that needs R395-16 closed.

Request to 388 (SPEC): (a) SPEC S-7.2 uses `c = 500000` per hour; the benchmark's clamp is on the 8-hour rate, so
the per-hour equivalent is 62500: please read `fundingClamp` per market from genesis; (b) add the fee tiers of
`risk.json` (open item O-4); (c) enforce `launchGate` of `risk.json` (markets halted until the insurance fund and the
backstop hold their seeds).

## 4. Stress measurements (two independent venues)

`compliance/research/stress.py` over recorded public candles (`compliance/research/marketdata/20261117/`, SHA-256
in `SHA256SUMS`; Kraken `/0/public/OHLC` and Coinbase Exchange `/products/<id>/candles`, read anonymously
2026-11-17 04:07 UTC), integer arithmetic, basis points rounded up. Output: `compliance/research/stress-20261117.json`
(reproduced byte for byte by the tests).

| Market | worst 1h adverse, Kraken (30 d) | worst 1h adverse, Coinbase (14.5 d) | worst 1d adverse, Kraken (2 y) | worst 1d adverse, Coinbase (350 d) |
|---|---|---|---|---|
| BTC-USD | long 3.56%, short 3.98% | long 3.55%, short 4.04% | long 17.84% (2025-10-10) | long 14.82% (2026-02-05) |
| ETH-USD | long 6.89% (2026-11-08) | long 6.07%, short 5.28% | long 25.85% (2025-02-03) | short 21.78% (2026-08-19) |
| SOL-USD | short 5.42% | short 5.49% (2026-11-15) | long 29.69% (2025-10-10) | long 17.81% (2026-01-31) |

"Adverse" = (open - low) / open for a long, (high - open) / open for a short, within one candle. The hourly series
are short (30 and 14.5 days: the venues' public endpoints return at most 720 and 350 candles); the daily series
show that the hourly window does not contain the worst historical events. Stated limits: a one-hour candle is an
upper bound on a move between two 3-second oracle rounds only when the engine liquidates continuously; it does not
bound a halt (section 2), and the daily worst cases (up to 29.69%) are NOT covered by the insurance-fund seed below.

## 5. Insurance fund, backstop, ADL

- Seed of the insurance fund (R-6 complement): `sum over markets of oiCapUsd * max(0, worst1hBps - mmfBps) / 10000`
  (rounded up) = BTC 29,965 + ETH 24,958 + SOL 2,318 = 57,241 USDC; `seedRequired` = 60,000 USDC (rounded up for
  price drift). Meaning: if every long (or short) at the OI cap sat exactly at maintenance and the worst measured
  one-hour move happened as a gap, the fund covers the deficit. Raising an OI cap requires the fund to cover the
  same formula at the new cap. Funder: TBD-owner (money).
- Backstop (SPEC 6.3 step 3): proposed seed 25,000 USDC (25% of the BTC OI cap notional at 5% IMF); takes at most
  25% of a market's OI cap; unwinds by reduce-only limits within the mark band; profits stay in it. TBD-owner.
- Launch gate: markets stay halted until both seeds are held (request to 388).
- ADL: last resort only (SPEC 6.4), disclosed in the risk disclosure.
- No owner withdrawal path from the insurance fund or the backstop in v0.2 (a change needs a new genesis): it is
  user protection, not revenue.

## 6. Oracle parameters

Per 394's proposal (`node/oracle/README.md` section 8), unchanged: `minSources` 3, `agreeBps` 100, `jumpBps` BTC 500,
ETH 500, SOL 800, `maxStalenessMs` 10,000, `confirmRounds` 3. Checked against section 4: the worst one-hour moves
(4.04%, 6.89%, 5.49%) exceed BTC's and ETH's 5% jump bound only if they happened within one 3-second round;
whether any did is NOT measured (no minute or second resolution data recorded): not proven.

## 7. Sources (read 2026-11-17; exact sentences in `compliance/research/sources.json`)

- S-HL-FEES: Hyperliquid Docs, "Fees", https://hyperliquid.gitbook.io/hyperliquid-docs/trading/fees
- S-HL-FUND: Hyperliquid Docs, "Funding", https://hyperliquid.gitbook.io/hyperliquid-docs/trading/funding
- S-HL-MT: Hyperliquid Docs, "Margin tiers", https://hyperliquid.gitbook.io/hyperliquid-docs/trading/margin-tiers
- S-HL-LIQ: Hyperliquid Docs, "Liquidations", https://hyperliquid.gitbook.io/hyperliquid-docs/trading/liquidations
- S-HL-MARK: Hyperliquid Docs, "Robust price indices", https://hyperliquid.gitbook.io/hyperliquid-docs/trading/robust-price-indices
- S-DYDX-FEES: dYdX Docs, "Rewards, Fees and Parameters", https://docs.dydx.xyz/concepts/trading/rewards
- S-KRAKEN-OHLC: Kraken public REST `https://api.kraken.com/0/public/OHLC`
- S-CB-CANDLES: Coinbase Exchange public REST `https://api.exchange.coinbase.com/products/<id>/candles`

## 8. Not done / not proven

- No centralised-exchange fee table could be read anonymously today (Binance, OKX pages returned no table): the
  benchmark is two decentralised venues only.
- Stress at minute resolution and over the worst historical days at hourly resolution: not measured.
- The L2 gas cost of a withdrawal claim in USD: not measured (L2 not live); the 1 USDC fee is a placeholder.
- Insurance and backstop seeds are money: owner inputs. The engine does not yet enforce the launch gate, fee tiers
  or `fundingClamp` (requests to 388).
- Whether offering these markets with this leverage is lawful anywhere: for counsel.
