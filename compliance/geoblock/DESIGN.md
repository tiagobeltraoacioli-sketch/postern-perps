# Geoblocking and sanctions screening: design (PosternDEX Perpetuals)

UNAUDITED prototype (agent 397, 2026-11-17). Nothing here is legal advice. Which places are restricted, what a
restriction means, whether exits stay open, what to do on a sanctions hit, what to record and for how long: all of
that is **for counsel**. This module only applies agent 392's list (`compliance/geoblock.json`) and the official
OFAC publication, and refuses whenever it cannot apply them. Not deployed anywhere.

## 1. Where the control sits

```
 browser / bot ──► edge module (edge/worker.mjs) ──► node admission (Rust, agent 389) ──► engine
                   filter: answers 451 itself,        AUTHORITY: re-runs the same decision
                   strips client geo headers,         with the recovered signer and the
                   adds an HMAC geo assertion         attestation records it holds
 frontend (391): reads GET /v1/geo and shows the notices; it enforces nothing
 L2 forced-action path (vault): not geoblocked and cannot be (a contract anyone can call); see docs/API.md §6
```

One decision function (`src/decide.mjs`), one list format, one set of conformance vectors
(`vectors/decision-vectors.json`, `vectors/list-vectors.json`, `vectors/geo-assertion-vectors.json`,
`vectors/attestation-vectors.json`). The node's Rust port (`rust/`) must pass the same files: two implementations
that disagree on any vector is a release blocker.

## 2. The decision

Evaluated in this order; the first refusal wins and every applicable reason is listed in `reasons`.

| Step | Condition | Result |
|---|---|---|
| 1 | request malformed: unknown mode/kind/surface/action type, `reduceOnly` not a boolean, an address that is not 20-byte hex or has a wrong EIP-55 checksum, an action naming no address | 400 `MALFORMED` |
| 2 | any screened address (account, signer, recipient, agent, depositor) on the sanctions list (even a stale one) | 451 `SANCTIONED` (everything, exits included) |
| 2 | sanctions list missing or older than 48 h (live) | 503 `SANCTIONS_UNAVAILABLE`, except CancelOrder, CancelAll, RevokeAgent |
| 3 | kind `read` | allowed (public reads are not geoblocked, API.md §6) |
| 3 | kind `deposit` (a vault event; no IP exists) | allowed unless sanctioned |
| — | geoblock list missing or refused by the loader | 503 `LIST_UNAVAILABLE`, except CancelOrder, CancelAll, RevokeAgent |
| 4 | geography: untrusted source (live), unknown or reserved country, country not `allowed`, subdivision listed, subdivision unknown in a country with listed subdivisions, subdivision of another country, declared residence denied, Tor, VPN, hosting (per `anonymizers`), signals unknown (live) | geo-denied |
| 5 | geo-denied and the action is in `exit_only_actions` (policy `exit_only`) | 200 `OK_EXIT_ONLY` |
| 5 | geo-denied otherwise | 451 `GEO_RESTRICTED` |
| 6 | geo-allowed, live, action opens or changes exposure, no valid attestation | 403 `ATTESTATION_REQUIRED` |
| — | otherwise | 200 `OK` |

Exit actions (never need an attestation): CancelOrder, CancelAll, reduce-only PlaceOrder, Withdraw, RevokeAgent.

## 3. Decided (technical), with the reason

1. **Strict list loader** (`src/list.mjs`). A list this code does not fully understand is refused, and a refused
   list behaves as no list (only no-value actions pass). Refused: unknown top-level keys other than the
   informational ones, a major version other than 0, tier effects other than blocked/restricted-review = deny and
   allowed = allow, an allowing default tier or unknown-country effect, an allowing reserved code (XX, T1, ZZ, A1,
   A2, O1, EU, AP), an allowing subdivision, unknown anonymizer kinds or values, an exit-only action outside the
   closed exit set. Reason: an error in the list must only ever close access.
2. **Unknown region in a country with restricted regions = denied** (`SUBDIVISION_UNKNOWN`). 392's rule says "its
   subdivision (when known) is not listed"; with an unknown region the occupied regions of Ukraine cannot be told
   apart, so I deny. Strictly stronger than the rule as written. Cost: every Ukrainian user whose region the
   geolocation does not give (all of them on the local DB-IP Lite fallback, which has no region) is denied until
   the edge provides the region. Reported to 392.
3. **A declared residence the list denies turns the user geo-denied** (`RESIDENCE_DECLARED_RESTRICTED`), whatever
   the IP says. A declaration can only add a refusal.
4. **Hosting IPs on the API** (`deny-ui-allow-api-with-attested-account`): allowed only with a valid attestation of
   an allowed residence that has no restricted regions, AND the datacenter's own country allowed. On the UI: denied.
5. **Signals unknown in live mode = denied** (`SIGNALS_UNAVAILABLE`): the operator must keep the Tor, VPN and
   datacenter sources loaded; a missing source is never read as "not a VPN". In dev mode unknown = false.
6. **Sanctions before geography, honoured even when stale**: staleness can only add refusals. Stale or missing in
   live mode: only actions that move no value pass (cancel, revoke), so a screening outage never strands open
   orders, and never lets value move unscreened. Withdrawals wait (the L2 forced-withdraw path remains).
7. **Every EVM-shaped address on the SDN list is screened, whatever symbol it is listed under**: an EVM key controls
   the same address on every EVM chain, including the Bloch L2 (700770). Addresses are compared lowercase; input
   with a wrong EIP-55 checksum is refused as malformed rather than guessed.
8. **The builder parses both OFAC formats of one publication** (SDN.XML and SDN_ADVANCED.XML) and refuses unless they
   name exactly the same (uid, currency, address) triples on the same publication date; refuses a truncated file
   (entry count below `Record_Count`), an empty result, an older publication than the previous build, and any
   address of the previous build that disappeared unless the operator confirms it (`--accept-removal`, after
   reading OFAC's recent actions). Output written then renamed.
9. **Edge to node: an HMAC assertion, not a trusted-proxy header** (`src/geo-assertion.mjs`). A header alone is
   forgeable by anyone reaching the origin; the MAC (HMAC-SHA256, 32-byte minimum secret, domain-separated, fixed
   field order, 30 s window, 5 s future tolerance, request-id replay cache that refuses when full) makes it
   unforgeable without the secret. The node still re-decides.
10. **Attestation as an EIP-712 signature under the engine's SPEC 0.1.1 domain** (`PosternPerps`, version 1,
    chainId 700770, verifyingContract = the PerpsVault). Changed on the coordinator's instruction (2026-11-17): the
    first version used its own domain on chain 7007701, which wallets refuse because it is not the active chain
    (391's report). Separation from engine actions now rests on the primary type `Attestation` (its type hash
    starts every hashStruct; no action type shares it; 388 asked to reserve the name) and verifyingContract binds
    it to one deployment. With no vault configured every function refuses (no typed data, no digest, no
    verification). It binds the exact statement text (keccak256; 392's `compliance/legal/notices.json`
    `attestation.statementId` / `statementHash`, checked against its own text at load), the declared residence and
    a validity period (at most 400 days). A text revision retires older ids. Verified in process (65 bytes, v 27/28,
    low-s, r and s in range, recovered = account). Whether a signature is the right legal form is for counsel.
11. **No dependencies.** keccak256, SHA-256, HMAC, secp256k1 recovery and the XML reader are small, in this
    directory, and checked against independent implementations (`@noble` 1.8.0 for keccak and signatures,
    `node:crypto` for HMAC, NIST and RFC 4231 answers, EIP-55's own vectors). Recovery uses only public data, so
    constant time is not required; the MAC comparison is constant time.

## 4. IP data: choice, licences, update procedure

Read on 2026-11-17 from the publishers' own pages.

| Source | Use | Licence (publisher's words) | Updates | Account |
|---|---|---|---|---|
| Cloudflare `request.cf.country` / `CF-IPCountry`, `request.cf.regionCode` | primary, at the edge | Cloudflare service terms (owner's account) | Cloudflare's own | the owner's Cloudflare account |
| DB-IP "IP to Country Lite" (CSV `ip_start,ip_end,country`) | local fallback for a node not behind the edge, and cross-check | "Creative Commons Attribution 4.0 International License"; web applications "must include a link back to DB-IP.com on pages that display or use results" | monthly | none |
| IPinfo Lite (country + ASN) | optional second country source | "Creative Commons Attribution-ShareAlike 4.0 International License" | every 24 h | token for download (owner) |
| Tor Project bulk exit list `check.torproject.org/torbulkexitlist` | Tor signal (plus Cloudflare's `T1`) | Tor Project public list | continuous | none |
| X4BNet `lists_vpn` (`output/vpn/ipv4.txt`, `output/datacenter/ipv4.txt`, IPv6 files) | VPN and datacenter signals | "scripts, curated inputs, and generated lists are licensed under MIT"; upstream IPLocate data CC BY-SA 4.0 | "rebuilt from the source data regularly" | none |

Cloudflare's HTTP headers reference: "XX - Used for clients without country code data." "T1 - Used for clients
using the Tor network." Both are treated as unknown (denied), T1 also as Tor.

Choice: Cloudflare at the edge (it gives the region, which the Ukraine entries need); DB-IP Lite as the local
source (no account, permissive licence; the attribution link goes on the site's about page: frontend 391); IPinfo
Lite as the optional second source once the owner has a token: two sources that disagree give "unknown" (denied).
IP2Proxy LITE was read and not chosen (registration required; the LITE page describes only open proxies). MaxMind
GeoLite2 was not evaluated today.

Update procedure (operator, daily; nothing is fetched by the node or the edge at run time):
1. Download into a new empty directory: the DB-IP CSV of the month, the Tor exit list, the four X4BNet files.
2. Record each file's URL, size and SHA-256 in the update log.
3. Load each with the strict loaders (`src/ipdb.mjs`); a refusal stops the update and the previous set stays.
4. Publish the set to the edge binding and the node together, then reload.

Measured on the 2026-11 files: DB-IP country 732,796 ranges (362,998 IPv4, 369,798 IPv6), JS load 22 s, lookup
about 13 µs, heap about 200 MB; Tor list 1,269 exits (619 of them inside the X4BNet datacenter list).

## 5. VPN, hosting and Tor: false-positive cost

Measured coverage of the 2026-11-17 X4BNet lists: VPN list 6,896 merged IPv4 ranges, 3,328,110 addresses (0.08 % of
IPv4); datacenter list 34,003 merged IPv4 ranges, 408,006,871 addresses (9.5 % of IPv4). Who is refused wrongly
(no measurement of real users exists; these are the classes, for counsel and the owner):

- People on a corporate VPN or a privacy relay whose exit is in the datacenter list (UI denied; API with attestation
  allowed). Cost: they must switch the VPN off; some cannot.
- Travellers: an allowed-country resident abroad in a denied country is denied (exit-only). Intended.
- Residents of an allowed country whose IP the database places elsewhere (stale ranges, mobile carriers routing via
  a neighbour). DB-IP and IPinfo disagreeing gives "unknown": denied.
- Ukraine: every user without a known region (decision 3.2).
- Tor users: always denied for trading (exit-only remains).

False negatives (a restricted user let through) remain possible with a residential proxy or a VPN not in any list;
geolocation is an access-point control, not proof of location. The attestation is the second layer; what further
checks a regulator expects is for counsel.

## 6. Sanctions list: source and update procedure

Source: U.S. Treasury OFAC Sanctions List Service, SDN.XML and SDN_ADVANCED.XML (URLs in `src/sanctions.mjs`).
OFAC FAQ 563: address listings "will always begin with 'Digital Currency Address', followed by a dash and the
digital currency's symbol". FAQ 562: they "are not likely to be exhaustive".

Build of 2026-11-17 (`data/sanctions-evm.json`): publication 2026-11-13, 19,565 entries, 1,080 listed
digital-currency addresses across 20 symbols, 127 EVM-shaped addresses screened; SDN.XML
`55badef6928044db9fd5e798e6279802c1fa2019fa82a580a2e0d29a741ec9ae` (29,558,472 bytes), SDN_ADVANCED.XML
`45ee21dbfa764b353d81e6ac97863dc4ce0b77feec3c9581b3a786d85635e13d` (128,641,259 bytes); one triple repeated
inside SDN.XML (uid 37070, counted once); no EVM address with an invalid checksum.

Procedure (operator, daily):
```
node tools/build-sanctions.mjs --sdn SDN.XML --advanced SDN_ADVANCED.XML --out sanctions-evm.json --previous sanctions-evm.json
```
Exit 1 writes nothing and the previous list stays; after 48 h without a successful build the decision refuses
value-moving actions (decision 3.6). What is screened: the action's account, the recovered signer (node), a
withdrawal recipient, an agent being approved or revoked, a depositor (node, from the vault event).

Not covered (for counsel): other lists (EU, UN, UK OFSI, local lists), SDN entities' unlisted addresses, indirect
exposure (funds that passed through a listed address), the 50 % rule for owned entities, name screening.

## 7. Logging and retention: notes for counsel

What each decision can log (the decision object carries all of it): time, decision code, every reason, list
version and SHA-256, sanctions version and content hash, country, subdivision, the three signals, surface, action
type, the screened addresses and roles. The IP address is not needed for the decision record; when one must be kept
(abuse, a regulator's request), keep `HMAC-SHA256(ipKey, ip)` under a key id rotated monthly and destroy old keys on
a schedule, so old records cannot be linked back to an address once the key is gone. The attestation record
(`attestationRecord()`) keeps the signed message and signature (re-verifiable by anyone), the verification result
and the access context, with the IP only as that keyed hash.

Questions for counsel: retention period of decision logs and attestation records (AML record-keeping versus data
minimisation; Panama, Brazil LGPD, EU GDPR where users connect from); whether a keyed IP hash is personal data for
each regime; whether to keep refused requests at all; who may read the logs; what to tell a refused user (the
451 text is in `src/decide.mjs` `MESSAGES`); what to do with funds of an address that becomes sanctioned (the engine
refuses everything; the list says "held for counsel"); whether exit-only may stay open for comprehensively
sanctioned countries (392's note).

## 8. Owner inputs

- The edge-to-node secret (32+ random bytes), generated by the owner on the operator host, never by an agent:
  `TBD-owner`.
- A Cloudflare account and Worker deployment (not done; not allowed this round).
- An IPinfo token if the second country source is wanted.
- Counsel's answers to section 7 and to 392's list.

## 9. Not done / not proven

- Not deployed; the Worker entry point has never run on Cloudflare (local `Request`/`Response` only).
- The Rust port for the node: see `rust/README.md` for its state.
- No measurement of false positives on real users (none exist).
- The attestation statement is 392's ATTEST-0.1-20261117 (draft for counsel).
- The legal adequacy of IP geolocation plus attestation is not assessed (for counsel).
- No independent review of this directory yet (395).
