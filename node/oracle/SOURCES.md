# Oracle sources (venues) per launch market

UNAUDITED PROTOTYPE. Read 2026-11-17 (UTC). Owned by agent 394. Every endpoint below is a public, anonymous,
read-only REST ticker; no API key exists in this repository or is needed. Response formats were verified on the
recorded responses in `fixtures/recorded/` and `fixtures/rounds/` (recorded 2026-11-17 04:06 and 04:32 UTC).

## Launch markets and venues

Markets from `node/fixtures/markets.json` (389, provisional until 392's `markets.json`): BTC-USD (id 1),
ETH-USD (id 2), SOL-USD (id 3). Each market has seven configured readings from seven different operators; the
feeder needs `minSources = 3` agreeing readings and a strict majority of the valid ones (rule F5).

| Source | Operator | Quote | Endpoint (pair placeholder) | Venue time used | Pair echoed and checked |
|---|---|---|---|---|---|
| coinbase | Coinbase (Coinbase Exchange) | USD | `https://api.exchange.coinbase.com/products/{BASE}-USD/ticker` | last trade `time` (RFC 3339) | no |
| kraken | Payward (Kraken) | USD | `https://api.kraken.com/0/public/Ticker?pair={XBT|ETH|SOL}USD` | none | yes (result key) |
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

## Request budget (per feeder, one request per source per 3 s round)

Twenty-five requests per round in total (21 market sources + 4 rate sources), i.e. about 8.3 per second per
feeder spread over seven operators. Per operator: Coinbase 4 per round, Kraken 4, Bitstamp 4, Gemini 4,
Bitfinex 3, OKX 3, Binance 3.

| Operator | Public limit (source, read date) | Our use | Status |
|---|---|---|---|
| Bitfinex | 90 requests/min for the ticker endpoint (Bitfinex REST reference, 2026-11-17) | 60/min | inside |
| Kraken | not stated on the ticker reference page (2026-11-17); Kraken documents a per-IP public-endpoint limit elsewhere (not re-read here) | 80/min | NOT PROVEN: batch the four pairs into one request (`pair=XBTUSD,ETHUSD,SOLUSD,USDTUSD`) before production |
| Coinbase, Bitstamp, Gemini, OKX, Binance | not re-read on 2026-11-17 | 60-80/min each | NOT PROVEN: read each venue's limit before production; batching endpoints exist at OKX (`/market/tickers`) and Binance (`symbols=[...]`) |

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

- Terms of use not confirmed for any venue (above); rate limits confirmed for Bitfinex only.
- Kraken per-IP limit: the 3 s cadence over four separate pair requests may exceed it; batching not implemented.
- Venue liveness and depth were not measured over time (only 6 consecutive rounds and one sequential snapshot were
  recorded).
- No venue's historical outage or bad-print behaviour was studied; the frozen, spread and staleness rules are the
  only defences against a venue's own malfunction.
