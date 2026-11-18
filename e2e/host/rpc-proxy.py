#!/usr/bin/env python3
"""Second L2 endpoint of the e2e stack: a pass-through JSON-RPC proxy in front of the local anvil.

The node's watcher needs TWO endpoints that agree, pinned to one block hash. Locally both read one anvil, so the
agreement itself proves nothing; what this proxy adds is the ability to make ONE endpoint lie, so the rehearsal can
show that the node then credits nothing (fail closed):

  POST /__fault  {"mode": "none" | "down" | "amount" | "hash" | "drop"}
    down    every request answered 503
    amount  the amount word of every Deposit log is raised by 1 unit (a deposit that the other endpoint does not show)
    hash    the hash of every block header returned is altered (the block-hash pin must fail)
    drop    eth_getLogs returns no logs (an endpoint that hides a deposit)
  GET /__fault   current mode and counters

Loopback only. Development and tests only. UNAUDITED prototype. Copyright (c) 2026 Bloch Inc. All rights reserved.
"""
import json
import sys
import threading
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

UP = sys.argv[1]
PORT = int(sys.argv[2])
STATE = {"mode": "none", "requests": 0, "altered": 0}
LOCK = threading.Lock()
MODES = ("none", "down", "amount", "hash", "drop")


def alter(method, result, mode):
    """Returns (result, changed)."""
    if mode == "amount" and method == "eth_getLogs" and isinstance(result, list):
        out, n = [], 0
        for log in result:
            data = log.get("data", "0x")
            if len(data) >= 66:
                word = int(data[2:66], 16) + 1
                log = dict(log, data="0x" + format(word, "064x") + data[66:])
                n += 1
            out.append(log)
        return out, n
    if mode == "drop" and method == "eth_getLogs" and isinstance(result, list):
        return [], len(result)
    if mode == "hash" and method == "eth_getBlockByNumber" and isinstance(result, dict) and result.get("hash"):
        h = result["hash"]
        flipped = h[:-1] + ("0" if h[-1] != "0" else "1")
        return dict(result, hash=flipped), 1
    return result, 0


class H(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *a):
        pass

    def send(self, code, body):
        b = json.dumps(body).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(b)))
        self.send_header("Connection", "close")
        self.end_headers()
        self.wfile.write(b)
        self.close_connection = True

    def do_GET(self):
        if self.path == "/__fault":
            with LOCK:
                return self.send(200, dict(STATE))
        self.send(404, {"error": "not found"})

    def do_POST(self):
        n = int(self.headers.get("Content-Length", "0"))
        raw = self.rfile.read(n)
        if self.path == "/__fault":
            try:
                mode = json.loads(raw)["mode"]
                assert mode in MODES
            except Exception:
                return self.send(400, {"error": "mode must be one of " + ", ".join(MODES)})
            with LOCK:
                STATE["mode"] = mode
                return self.send(200, dict(STATE))
        with LOCK:
            STATE["requests"] += 1
            mode = STATE["mode"]
        if mode == "down":
            return self.send(503, {"error": "proxy fault: down"})
        try:
            req = urllib.request.Request(UP, data=raw, headers={"Content-Type": "application/json"})
            with urllib.request.urlopen(req, timeout=10) as r:
                body = json.loads(r.read())
        except Exception as e:  # upstream down: say so, never invent an answer
            return self.send(502, {"error": "upstream: " + str(e)})
        if mode != "none":
            try:
                reqs = json.loads(raw)
                pairs = zip(reqs, body) if isinstance(reqs, list) else [(reqs, body)]
                changed = 0
                for rq, rs in pairs:
                    if isinstance(rs, dict) and "result" in rs:
                        rs["result"], c = alter(rq.get("method"), rs["result"], mode)
                        changed += c
                with LOCK:
                    STATE["altered"] += changed
            except Exception as e:
                return self.send(502, {"error": "proxy: " + str(e)})
        self.send(200, body)


if __name__ == "__main__":
    ThreadingHTTPServer(("127.0.0.1", PORT), H).serve_forever()
