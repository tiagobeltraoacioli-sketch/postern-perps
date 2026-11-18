# Records N consecutive oracle rounds from the public venue tickers of SOURCES.md, concurrently, in the fixture
# format read by `perps-oracle replay` ({venue, url, sentMs, receivedMs, httpDateMs, error, body}).
# Public, anonymous, read-only GETs with a generic client name. Usage: python3 record-rounds.py OUTDIR N
import email.utils, json, os, sys, threading, time, urllib.request

out = sys.argv[1]
n = int(sys.argv[2])
S = []
for b in ["BTC", "ETH", "SOL"]:
    l = b.lower()
    kb = "XBT" if b == "BTC" else b
    S += [
        (f"coinbase-{b}-USD", f"https://api.exchange.coinbase.com/products/{b}-USD/ticker"),
        (f"kraken-{b}-USD", f"https://api.kraken.com/0/public/Ticker?pair={kb}USD"),
        (f"bitstamp-{b}-USD", f"https://www.bitstamp.net/api/v2/ticker/{l}usd/"),
        (f"gemini-{b}-USD", f"https://api.gemini.com/v1/pubticker/{l}usd"),
        (f"bitfinex-{b}-USD", f"https://api-pub.bitfinex.com/v2/ticker/t{b}USD"),
        (f"okx-{b}-USDT", f"https://www.okx.com/api/v5/market/ticker?instId={b}-USDT"),
        (f"binance-{b}-USDT", f"https://api.binance.com/api/v3/ticker/bookTicker?symbol={b}USDT"),
    ]
S += [
    ("coinbase-USDT-USD", "https://api.exchange.coinbase.com/products/USDT-USD/ticker"),
    ("kraken-USDT-USD", "https://api.kraken.com/0/public/Ticker?pair=USDTUSD"),
    ("bitstamp-USDT-USD", "https://www.bitstamp.net/api/v2/ticker/usdtusd/"),
    ("gemini-USDT-USD", "https://api.gemini.com/v1/pubticker/usdtusd"),
]


def ms():
    return int(time.time() * 1000)


def fetch(name, url, sent, d):
    rec = {"venue": name, "url": url, "sentMs": sent, "httpDateMs": None, "error": None, "body": ""}
    try:
        req = urllib.request.Request(url, headers={"User-Agent": "perps-oracle/0.1"})
        with urllib.request.urlopen(req, timeout=1.5) as r:
            rec["body"] = r.read(65536).decode()
            dt = r.headers.get("Date")
            if dt:
                rec["httpDateMs"] = int(email.utils.parsedate_to_datetime(dt).timestamp() * 1000)
    except Exception as e:  # recorded as a failed fetch
        rec["error"] = type(e).__name__ + ": " + str(e)[:200]
    rec["receivedMs"] = ms()
    with open(os.path.join(d, name + ".json"), "w") as f:
        json.dump(rec, f)


for _ in range(n):
    t = ms()
    time.sleep(((t // 3000 + 1) * 3000 - t) / 1000)
    sent = ms()
    d = os.path.join(out, f"round-{sent // 3000}")
    os.makedirs(d, exist_ok=True)
    th = [threading.Thread(target=fetch, args=(a, b, sent, d)) for a, b in S]
    for x in th:
        x.start()
    for x in th:
        x.join()
    errs = sum(1 for f in os.listdir(d) if json.load(open(os.path.join(d, f)))["error"])
    print(d, errs, "errors")
