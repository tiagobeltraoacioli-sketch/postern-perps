# Oracle parameters: calibration on public data, and the live hour (agent 432, 2026-11-18)

UNAUDITED PROTOTYPE. Nothing here is deployed. This file gives the numbers behind every oracle parameter of
`../README.md` section 8, the method, the sources, and what the numbers do NOT show. Proposals only widen or keep a
bound; none is loosened.

## 1. Data and method

- Source: public, anonymous, read-only REST candle endpoints, fetched 2026-11-18 from one host with a generic client
  name and about three requests per second in total (`fetch_candles.py`): Coinbase Exchange
  (`/products/{BASE}-USD/candles?granularity=60`), Bitstamp (`/api/v2/ohlc/{base}usd/?step=60`), Bitfinex
  (`/v2/candles/trade:1m:t{BASE}USD/hist`), Binance (`/api/v3/klines?symbol={BASE}USDT&interval=1m`, converted to
  USD with Coinbase's USDT-USD minute close). Kraken and Gemini serve only recent minute candles and OKX 100 per
  request: not used here (they are in the live hour of section 4).
- Days (UTC): the worst days named in `docs/MARKETS.md` section 4 (2025-10-10, 2026-01-31, 2026-02-05, 2026-11-15),
  the day after the largest (2025-10-11), and the 30 days 2026-10-19 to 2026-11-17: 34 days, 48,960 minutes per
  market, 442 files (`results/candles-manifest.sha256`; the raw files stay on the build host, 45.77, in the
  agent's scratch, and can be fetched again with the script).
- Reference series: per minute, the median over the venues that traded of the close, of the high and of the low
  (`analyse_candles.py`, whose header states every statistic). Results: `results/candles-20261118.json`; the tables
  below are printed from that file by `tables.py`.
- Limits of minute candles, stated once: (a) an oracle round is 3 s; a minute's range bounds any 3 s move inside
  it from above but does not resolve it; (b) minute closes of different venues are last trades at different
  instants, so the dispersion of T2 overstates the dispersion of simultaneous quotes; (c) four venues, not the
  seven of the feeder; (d) 34 days chosen to contain the worst days on record in MARKETS.md, not a random sample;
  the percentiles describe these days only. Floating point is used for these statistics; nothing here is
  consensus arithmetic.

## 2. Tables

### T1 minute moves of the four-venue median (bps)

| Market | set | minutes | 1m return p50 | p99.9 | p99.99 | max | 1m range max | minutes with range > 300 / 500 / 800 / 1000 |
|---|---|---|---|---|---|---|---|---|
| BTC | last30d | 43200 | 2.4 | 42.6 | 67.9 | 146 | 189 | 0 / 0 / 0 / 0 |
| BTC | worst | 7198 | 4.4 | 125 | 230 | 230 | 361 | 4 / 0 / 0 / 0 |
| BTC | 2025-10-10 | 1440 | 3.0 | 191 | 230 | 230 | 361 | 4 / 0 / 0 / 0 |
| BTC | 2026-01-31 | 1440 | 3.3 | 198 | 229 | 229 | 251 | 0 / 0 / 0 / 0 |
| BTC | 2026-02-05 | 1440 | 9.4 | 105 | 132 | 132 | 132 | 0 / 0 / 0 / 0 |
| BTC | 2026-11-15 | 1441 | 4.9 | 68.6 | 78.2 | 78.2 | 91.3 | 0 / 0 / 0 / 0 |
| ETH | last30d | 43200 | 3.2 | 60.9 | 127 | 201 | 462 | 2 / 0 / 0 / 0 |
| ETH | worst | 7198 | 6.5 | 198 | 475 | 475 | 510 | 20 / 1 / 0 / 0 |
| ETH | 2025-10-10 | 1440 | 5.0 | 380 | 462 | 462 | 510 | 18 / 1 / 0 / 0 |
| ETH | 2026-01-31 | 1440 | 5.3 | 380 | 475 | 475 | 497 | 2 / 0 / 0 / 0 |
| ETH | 2026-02-05 | 1440 | 12.0 | 137 | 140 | 140 | 157 | 0 / 0 / 0 / 0 |
| ETH | 2026-11-15 | 1441 | 5.8 | 80.2 | 84.3 | 84.3 | 110 | 0 / 0 / 0 / 0 |
| SOL | last30d | 43200 | 4.0 | 61.2 | 109 | 167 | 306 | 1 / 0 / 0 / 0 |
| SOL | worst | 7198 | 8.1 | 257 | 459 | 459 | 1045 | 36 / 12 / 4 / 1 |
| SOL | 2025-10-10 | 1440 | 6.8 | 378 | 422 | 422 | 1045 | 33 / 11 / 4 / 1 |
| SOL | 2026-01-31 | 1440 | 5.7 | 334 | 459 | 459 | 517 | 3 / 1 / 0 / 0 |
| SOL | 2026-02-05 | 1440 | 12.5 | 194 | 212 | 212 | 246 | 0 / 0 / 0 / 0 |
| SOL | 2026-11-15 | 1441 | 7.2 | 77.1 | 89.4 | 89.4 | 102 | 0 / 0 / 0 / 0 |

### T2 dispersion between venues, per minute (largest distance of a venue's close from the median, bps)

| Market | set | minutes | p50 | p99 | p99.9 | max | minutes > 50 / 100 / 200 | minutes the feeder rule F5 would abstain |
|---|---|---|---|---|---|---|---|---|
| BTC | last30d | 43200 | 13.3 | 23.1 | 28.2 | 30.5 | 0 / 0 / 0 | 0 |
| BTC | worst | 7200 | 15.4 | 42.6 | 217 | 328 | 51 / 31 / 10 | 1 |
| ETH | last30d | 43200 | 12.8 | 23.6 | 29.5 | 139 | 2 / 1 / 0 | 0 |
| ETH | worst | 7200 | 14.8 | 75.6 | 207 | 409 | 141 / 46 / 8 | 1 |
| SOL | last30d | 43200 | 12.5 | 23.2 | 29.0 | 42.0 | 0 / 0 / 0 | 0 |
| SOL | worst | 7200 | 12.1 | 249 | 784 | 1087 | 164 / 107 / 85 | 36 |

### T3 largest move away from the price at t during the next m minutes (bps), against the exit band

| Market | m (min) | p99.9 (all 35 days) | max (last 30 d) | max (worst days) | band now: jumpBps + 100 m | minutes over it | band proposed | proposed / max |
|---|---|---|---|---|---|---|---|---|
| BTC | 1 | 82.2 | 182 | 275 | 600 | 0 | 600 | 2.18 |
| BTC | 2 | 115 | 199 | 352 | 700 | 0 | 700 | 1.99 |
| BTC | 5 | 199 | 232 | 531 | 1000 | 0 | 1000 | 1.88 |
| BTC | 10 | 274 | 247 | 778 | 1500 | 0 | 1500 | 1.93 |
| BTC | 15 | 330 | 295 | 835 | 2000 | 0 | 2000 | 2.40 |
| BTC | 30 | 424 | 392 | 942 | 3500 | 0 | 3500 | 3.72 |
| BTC | 60 | 786 | 480 | 995 | 5000 | 0 | 5000 | 5.03 |
| ETH | 1 | 133 | 430 | 496 | 600 | 0 | 800 | 1.61 |
| ETH | 2 | 199 | 524 | 605 | 700 | 0 | 900 | 1.49 |
| ETH | 5 | 315 | 648 | 718 | 1000 | 0 | 1200 | 1.67 |
| ETH | 10 | 463 | 677 | 1074 | 1500 | 0 | 1700 | 1.58 |
| ETH | 15 | 595 | 704 | 1126 | 2000 | 0 | 2200 | 1.95 |
| ETH | 30 | 711 | 711 | 1304 | 3500 | 0 | 3700 | 2.84 |
| ETH | 60 | 1110 | 717 | 1414 | 5000 | 0 | 5000 | 3.54 |
| SOL | 1 | 188 | 249 | 750 | 900 | 0 | 1400 | 1.87 |
| SOL | 2 | 249 | 263 | 1003 | 1000 | 1 | 1500 | 1.50 |
| SOL | 5 | 422 | 333 | 1366 | 1300 | 3 | 1800 | 1.32 |
| SOL | 10 | 551 | 394 | 1452 | 1800 | 0 | 2300 | 1.58 |
| SOL | 15 | 649 | 461 | 1452 | 2300 | 0 | 2800 | 1.93 |
| SOL | 30 | 829 | 517 | 1676 | 3800 | 0 | 4300 | 2.57 |
| SOL | 60 | 1345 | 646 | 1692 | 5000 | 0 | 5000 | 2.96 |

### T4 error of a settlement at the pre-halt index after 60 minutes without a committee price (bps)

| Market | set | p50 | p99 | p99.9 | max |
|---|---|---|---|---|---|
| BTC | last30d | 18.8 | 173 | 354 | 459 |
| BTC | worst | 34.0 | 402 | 726 | 883 |
| ETH | last30d | 24.7 | 255 | 505 | 577 |
| ETH | worst | 50.8 | 550 | 1037 | 1232 |
| SOL | last30d | 31.3 | 251 | 473 | 640 |
| SOL | worst | 66.6 | 559 | 1283 | 1587 |

### Coverage

| Market | minutes with a reference | minutes with fewer than 2 venues | venue minutes |
|---|---|---|---|
| BTC | 48960 | 0 | coinbase 48956, bitstamp 48960, bitfinex 48773, binance 48960 |
| ETH | 48960 | 0 | coinbase 48955, bitstamp 48960, bitfinex 48531, binance 48960 |
| SOL | 48960 | 0 | coinbase 48952, bitstamp 48960, bitfinex 47300, binance 48960 |

## 3. What the numbers say, parameter by parameter

| Parameter | Now | Finding | Proposal |
|---|---|---|---|
| `jumpBps` (largest step of the index per 3 s round; larger = JUMP halt until 3 agreeing rounds) | BTC 500, ETH 500, SOL 800 | T1: a whole MINUTE moved more than this in 0 (BTC), 1 (ETH) and 4 (SOL) of 48,960 minutes, all on 2025-10-10; in the last 30 days never. A genuine move therefore almost never trips it, and a jump that does trip it is a move of crash size inside 3 s, where waiting three rounds (9 s) is the designed behaviour | KEEP. Tightening (BTC to 300 would have tripped in at most 4 minutes of the 34 days) would need sub-minute data to rule out false halts in fast markets: not proven, not proposed |
| `agreeBps` (band in which `q` signers must agree) | 100 | Signers read the same venues in the same round; they differ by sampling time inside the round. Live hour (section 4): the 3 s move of one feeder's observation is far below 100 bps at every percentile measured. In a crash minute (T1: up to 361 / 510 / 1,045 bps inside one minute) two feeders 3 s apart can differ by more: the round fails C4, nothing moves, and the market goes STALE after 10 s (fail closed) | KEEP |
| `maxSourceDeviationBps` (feeder outlier cut, F4) | 200 | T2: in the 30 calm days no venue was ever 200 bps from the median (max 30 / 139 / 42 bps); on the worst days a venue was beyond 200 bps in 10 / 8 / 85 minutes and the feeder would have abstained (fewer than 3 agreeing or no strict majority of 4 venues) in 1 / 1 / 36 minutes. With asynchronous minute closes this overstates it | KEEP. Stated consequence: in a venue dislocation of the 2025-10-10 kind SOL would have had no index for tens of minutes (STALE halt), by design |
| `maxStalenessMs` | 10,000 | Live hour (section 4): rounds without a price and the longest run of them are reported there. 10 s tolerates two lost rounds in a row; the third halts | KEEP |
| `exitHaircutBps` (half-width of the exit band at the moment of the halt; must be at least `jumpBps`) | = `jumpBps`: 500, 500, 800 | T3: the band must contain where the price can really be after m minutes of darkness, or leaving at the band is still better than staying (R436-10) and a withdrawal during a halt can take collateral a real move has already consumed. BTC: covered at every horizon with a factor 1.9 or more. ETH: covered, but by a factor of only 1.16 at 2 minutes (605 against 700). SOL: NOT covered: 1,003 bps in 2 minutes against 1,000 (1 minute of the sample) and 1,366 in 5 minutes against 1,300 (3 minutes) | WIDEN: ETH 700, SOL 1300; BTC stays 500. Criterion: the band is at least 1.3 times the largest recorded move at every horizon of T3 (last column) |
| `exitHaircutPerMinBps` | 100 | T3: between 10 and 60 minutes the largest recorded move grows by 217 (BTC), 340 (ETH), 240 (SOL) bps while the band grows by 3,500 | KEEP |
| `exitMaxHaircutBps` | 5000 | T3: largest 60-minute move 995 / 1,414 / 1,692 bps | KEEP |
| `maxHaltMs` (X3: settle the market after this long without a good index) | 3,600,000 | T4: settling at the pre-halt index after an hour with no committee price is wrong by 19 to 31 bps at the median, 173 to 255 bps at the 99th percentile in calm days, and by up to 883 / 1,232 / 1,587 bps on the worst days. That error is a transfer between longs and shorts chosen by whoever caused the halt | KEEP the hour, and change the rule instead (proposal to 430, README 6a): X3 settles only at a price the committee signed since the halt; with none, the market stays halted and single-account exits use the band |
| `fetchTimeoutMs`, drop rule | 1500; 5 fails, 20 rounds | Live hour (section 4) | KEEP |

Values to carry into genesis (`compliance/markets.json`, per market `oracle` object; 435 holds that directory, 430
the converter): `exitHaircutBps` BTC 500, ETH 700, SOL 1300; `exitHaircutPerMinBps` 100; `exitMaxHaircutBps` 5000;
`maxHaltMs` 3600000; the other oracle fields unchanged.

## 4. The live hour

Running at the time of this commit (started 2026-11-18 23:23:45 UTC on the build host, 1,300 rounds); this section is
written when it ends.
