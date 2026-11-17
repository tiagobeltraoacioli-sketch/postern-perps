# PosternDEX Perpetuals: Privacy Notice

Document id: `PRIV-0.1-20261117`. Status: DRAFT FOR COUNSEL. Not legal advice. UNAUDITED prototype: nothing is live.
Retention periods and the legal basis of each processing are proposals for counsel (`TBD-counsel`); the technical
logging design is agent 397's (`compliance/geoblock/`), and this notice must match it before use.

## 1. Controller

Bloch Inc (Panama), operator of the Interface, `TBD-owner` (address, contact for privacy requests).
`TBD-counsel`: whether an EU/UK representative or a Brazilian encarregado (LGPD) is needed given that these
jurisdictions are restricted.

## 2. What is public by design

The exchange is transparent: every order, trade, position, balance, deposit and withdrawal is linked to your wallet
address and is public on the exchange's input log and on the Bloch L2. Anyone can read them. We cannot delete them.

## 3. What the Interface processes

| Data | Why | Kept (proposal) |
|---|---|---|
| IP address and the country / region derived from it; VPN, proxy, Tor and hosting flags | to enforce the restricted-jurisdiction rules | IP not stored in clear: a keyed hash with the decision, 90 days, then aggregated (`TBD-counsel`) |
| Wallet address checked against sanctions lists | sanctions screening | the address and the list version, for as long as the account exists (`TBD-counsel`) |
| Attestation record (`ATTEST-0.1-20261117`): address, document id and hash, signature, time, detected country | evidence of the user's statement | for as long as the account exists plus `TBD-counsel` years |
| Technical logs (errors, request timing) | operating the service | 14 days |
| Local browser storage (theme, acknowledgements) | convenience | on your device only |

No advertising trackers and no third-party analytics are used. `TBD-owner`: hosting and CDN providers that see
requests (name each).

## 4. Sharing

With service providers that host the Interface (`TBD-owner`), and with authorities where the law requires it
(`TBD-counsel`).

## 5. Your rights

`TBD-counsel`: rights by jurisdiction and how to exercise them; what cannot be erased (section 2).

## Not done / not proven

- The proposed retention periods are not reconciled with 397's logging implementation.
- No data-protection assessment has been made.
