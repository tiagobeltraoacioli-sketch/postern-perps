# Oracle sources (venues) per launch market

UNAUDITED PROTOTYPE. Read 2026-11-17 (UTC) by agent 394; round 2 (2026-11-18, agent 432): Kraken batched, request
budget restated, one continuous live hour measured (section "Live hour"). Every endpoint below is a public, anonymous,
read-only REST ticker; no API key exists in this repository or is needed. Response formats were verified on the
recorded responses in `fixtures/recorded/` and `fixtures/rounds/` (recorded 2026-11-17 04:06 and 04:32 UTC).

## Launch markets and venues

Markets from `node/fixtures/markets.json` (389, provisional until 392's `markets.json`): BTC-USD (id 1),
ETH-USD (id 2), SOL-USD (id 3). Each market has seven configured readings from seven different operators; the
feeder needs `minSources = 3` agreeing readings and a strict majority of the valid ones (rule F5).

| Source | Operator | Quote | Endpoint (pair placeholder) | Venue time used | Pair echoed and checked |
|---|---|---|---|---|---|
| coinbase | Coinbase (Coinbase Exchange) | USD | `https://api.exchange.coinbase.com/products/{BASE}-USD/ticker` | last trade `time` (RFC 3339) | no |
| kraken | Payward (Kraken) | USD | `https://api.kraken.com/0/public/Ticker?pair=USDTUSD,XBTUSD,ETHUSD,SOLUSD` (ONE batched request per round for the four pairs, round 2; each source reads its own result key) | none | yes (result key; a missing, foreign or doubled key refuses the body) |
| bitstamp | Bitstamp | USD | `https://www.bitstamp.net/api/v2/ticker/{base}usd/` | snapshot `timestamp` (s) | no |
| gemini | Gemini | USD | `https://api.gemini.com/v1/pubticker/{base}usd` | none (`volume.timestamp` is the volume minute, not the quote time) | no |
| bitfinex | iFinex (Bitfinex) | USD | `https://api-pub.bitfinex.com/v2/ticker/t{BASE}USD` | none | no |
| okx | OKX | USDT | `https://www.okx.com/api/v5/market/ticker?instId={BASE}-USDT` | snapshot `ts` (ms) | yes (`instId`) |
| binance | Binance | USDT | `https://api.binance.com/api/v3/ticker/bookTicker?symbol={BASE}USDT` | none | yes (`symbol`) |

USDT/USD conversion rate (rule F3): coinbase `USDT-USD`, kraken `USDTUSD`, bitstamp `usdtusd`, gemini `usdtusd`
(four USD venues, `minSources = 3`, rate refused beyond 500 bps from 1).

Decided (technical):

- Five USD-quoted venues per market, so a market keeps a price (3 of 5) with every USDT venue dropped; USDT venues
  add depth and operator diversity but are never needed for quorum. Reason: a USDT depeg must not halt or move a
  USD index.
- Bitfinex is not a source of the USDT/USD rate: its operator shares ownership with the issuer of USDT, so it is
  not an independent judge of USDT's own peg. It stays a source of BTC/ETH/SOL in USD. (Relationship to be
  confirmed by counsel from the companies' own filings; the rule is conservative either way.)
- Coinbase International, Bybit and other venues that restrict US or other users were not added: no advantage for
  an index over the seven above, and a further dependency on venues the compliance work (392/397) may exclude.
- The Bitfinex ticker array has 11 fields; the 11th is `FIRST_TRADE` (a timestamp, per Bitfinex's reference page
  read 2026-11-17); the adapter uses fields 0 (BID), 2 (ASK), 6 (LAST_PRICE) only.
- Independence: seven different operators. Known correlations, stated for the reviewer: OKX and Binance are both
  USDT-quoted (the rate is common to both); Bitstamp's ownership changed in 2025 (acquisition by Robinhood
  announced; not verified here from a primary source: for counsel). No two configured sources share an operator.

## Request budget (per feeder, per 3 s round)

Twenty-two requests per round (round 1: 25): 18 market requests to six operators, 3 USDT/USD rate requests, and
ONE Kraken request for its four pairs. Per operator per round: Coinbase 4, Bitstamp 4, Gemini 4, Bitfinex 3, OKX 3,
Binance 3, Kraken 1. Client name sent: `ticker-client/1.0` (generic; nothing names an operator or a project).

| Operator | Public limit (source, read date) | Our use | Status |
|---|---|---|---|
| Bitfinex | 90 requests/min for the ticker endpoint (Bitfinex REST reference, 2026-11-17) | 60/min | inside |
| Kraken | not stated on the ticker reference page (2026-11-17) | 20/min (was 80/min before batching) | not hit in the live hour; the limit itself NOT PROVEN |
| Coinbase, Bitstamp, Gemini | not re-read | 80/min each | not hit in the live hour; NOT PROVEN |
| OKX, Binance | not re-read | 60/min each | not hit in the live hour; NOT PROVEN; batching endpoints exist (OKX `/market/tickers`, Binance `symbols=[...]`) and are not used |

A venue that answers 429, 418 or 403 is dropped at once for 20 rounds, doubling up to an hour (README 7a, D2): the
feeder backs off instead of retrying every 3 s. Seven feeders of seven validators on seven hosts each have their
own budget; several feeders behind ONE address would multiply the use per address (an operations rule: one feeder
per public address).

## Live hour (2026-11-18 23:23 to 2026-11-19 00:28 UTC; full tables in `calibration/CALIBRATION.md` section 4)

One feeder on one host, 1,300 consecutive rounds, 28,600 anonymous read-only requests.

| Operator | requests | answered 200 | other status | no answer in 1.5 s | latency p50 / p99 (ms) | readings used (BTC / ETH / SOL of 1,300) |
|---|---|---|---|---|---|---|
| Coinbase | 5,200 | 5,200 | 0 | 0 | 17 / 112 | 1300 / 1300 / 1300 |
| Kraken (one batched request) | 1,300 | 1,300 | 0 | 0 | 39 / 60 | 1300 / 1300 / 1300 |
| Bitstamp | 5,200 | 5,194 | 0 | 6 | 7 / 43 | 1298 / 1299 / 1297 |
| Gemini | 5,200 | 5,145 | 0 | 55 | 408 / 1,514 | 1287 / 1289 / 1289 |
| Bitfinex | 3,900 | 3,900 | 0 | 0 | 19 / 50 | 1300 / 1300 / 1300 |
| OKX | 3,900 | 3,893 | 0 | 7 | 264 / 344 | 1297 / 1298 / 1298 |
| Binance | 3,900 | 3,900 | 0 | 0 | 243 / 683 | 1300 / 1300 / 1300 |

No rate-limit answer (429, 418, 403) from any venue, no drop, no reading refused by an adapter, no outlier,
frozen, stale or wide reading. Every round priced every market from 6 or 7 sources.

**R432-3 (Info; Demonstration: the live hour, `calibration/results/live-hour-20261118-stats.json` L3).** Bitfinex's
USD prices stood 12.6 to 13.1 bps above the feeder's observation at the median on BTC, ETH and SOL, while the
other six sources stood within 3 bps of it; in the same hour USDT/USD was 0.99862 to 0.99882 (12 to 14 bps under
the dollar). Bitfinex's "USD" book priced like an unconverted USDT book. Consequences, stated: (a) the median is
not moved (one source of seven, inside `maxSourceDeviationBps`); (b) the round-1 claim "five USD-quoted venues per
market, so USDT never decides quorum" should be read as FOUR venues that tracked the dollar in this hour (Coinbase,
Kraken, Bitstamp, Gemini), still at or above `minSources` = 3 without any USDT-correlated source; (c) in a USDT
depeg Bitfinex would drift with USDT and, beyond 200 bps, be cut as an outlier; (d) it supports round 1's decision
to keep Bitfinex out of the USDT/USD rate. Not changed: converting Bitfinex with the USDT rate would be a claim
about that venue's dollar that one hour does not prove; a longer measurement is listed below.

## Terms of use (read attempts, 2026-11-17, anonymous)

The feeder redistributes nothing raw: it derives one median price per round, signs it and publishes the median
(and, in the node's `OracleStatus`, the per-venue prices it used). Whether each venue's terms permit that use
(commercial derived index, display of per-venue prices) is a legal question: **for counsel**. What was read:

| Operator | Page tried | Result |
|---|---|---|
| Kraken | `https://www.kraken.com/legal/api-terms` | HTTP 404 (page moved); the API reference page for the ticker names no terms |
| Coinbase | `https://www.coinbase.com/legal/developer-platform/terms-of-service` | HTTP 403 to an anonymous client |
| Bitstamp | `https://www.bitstamp.net/terms-of-use/` | empty to an anonymous client (script-rendered) |
| Gemini | `https://www.gemini.com/legal/user-agreement` | landing page dated 2026-02-19 that points to two entity agreements; no market-data clause on the page itself |
| Bitfinex, OKX, Binance | not read | to do |

Owner / counsel input: obtain and file each venue's current API and market-data terms (the pages above, read in a
browser), and confirm that a derived, signed median index and the display of per-venue prices are permitted; if a
venue forbids it, remove that source (the rules keep working with five or more sources per market).

## Not done / not proven

- Terms of use not confirmed for any venue (above); rate limits confirmed for Bitfinex only. "Not hit in one hour
  from one address" is an observation, not a limit.
- OKX and Binance batching not implemented (three requests each per round).
- R432-3: whether Bitfinex's USD prices track USDT persistently (one hour measured); if they do, the venue should
  be counted, and perhaps converted, as USDT-quoted.
- Venue liveness measured for one hour on one day from one host; no venue's historical outage or bad-print
  behaviour was studied beyond the minute candles of `calibration/CALIBRATION.md`; depth was not measured.
- The frozen, spread, staleness and drop rules are the only defences against a venue's own malfunction.
