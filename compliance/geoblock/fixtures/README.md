# Test fixtures (not for production use)

| File | Source | Licence / status |
|---|---|---|
| `geoblock.fixture.json` | copy of `../../geoblock.json` 0.1.1 (agent 392) with PA, CH, UA marked "allowed" FOR TESTS ONLY | not a statement about any country |
| `sdn-excerpt.xml`, `sdn-advanced-excerpt.xml` | 4 entries (uids 36, 45314, 48432, 50641) cut verbatim by `tools/make-sdn-excerpt.mjs` from OFAC's SDN.XML and SDN_ADVANCED.XML of 2026-11-13; the only edit: `Record_Count` set to 4 | U.S. Government publication |
| `sanctions-fixture.json` | `tools/build-sanctions.mjs` over the two excerpts, `--built-utc 2026-11-17T00:00:00Z` | derived |
| `dbip-country-excerpt.csv` | 9 lines of DB-IP "IP to Country Lite" 2026-11 | CC BY 4.0, IP Geolocation by DB-IP (https://db-ip.com) |
| `tor-excerpt.txt` | first 3 lines of the Tor Project bulk exit list, 2026-11-17 | public list |
| `vpn-excerpt.txt`, `datacenter-excerpt.txt` | first lines of X4BNet lists_vpn `output/vpn/*` and `output/datacenter/*`, 2026-11-17 | MIT (upstream IPLocate CC BY-SA 4.0) |
