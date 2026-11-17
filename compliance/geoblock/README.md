# compliance/geoblock: geoblocking and sanctions screening

UNAUDITED prototype (agent 397). Not legal advice. Not deployed. Design, decisions, sources, false-positive cost,
procedures and notes for counsel: [DESIGN.md](DESIGN.md).

| Path | What |
|---|---|
| `src/decide.mjs` | the one access decision (edge, node, frontend) and `GET /v1/geo` |
| `src/list.mjs` | strict loader of `../geoblock.json` (agent 392's list) |
| `src/sanctions.mjs`, `src/xml.mjs`, `tools/build-sanctions.mjs` | OFAC SDN digital-currency list: two-format build, run-time matcher |
| `data/sanctions-evm.json` | build of the 2026-11-13 OFAC publication (127 EVM addresses) |
| `src/attestation.mjs` | user eligibility attestation (EIP-712, own domain), verification, record |
| `src/geo-assertion.mjs` | edge-to-node HMAC geo assertion and replay cache |
| `src/ipdb.mjs` | local country / Tor / VPN / datacenter lookups |
| `edge/worker.mjs` | Worker-style edge module |
| `rust/` | the node's port (agent 389 links it), held to the same vectors |
| `vectors/` | conformance vectors every implementation runs |
| `fixtures/` | test inputs (see fixtures/README.md for their sources and licences) |

Run the tests (Node 20+, no install step): `node --test test/*.test.mjs`.
