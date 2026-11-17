# PosternDEX Perpetuals: Restricted-Persons Attestation

Document id: `ATTEST-0.1-20261117`. Status: DRAFT FOR COUNSEL. Not legal advice. UNAUDITED prototype.

## Text the user reads and signs (exact; machine-readable copy in `notices.json` field `attestation`)

The text signed is `attestation.text` of `compliance/legal/notices.json`, byte for byte (the document id on the
first line, then one line per statement). Its keccak-256 is `attestation.statementHash` and its id for the record
is `attestation.statementId` (lower case, agent 397's id rule); both are checked by the tests.

## How it is signed (the record and its typed data are agent 397's: `compliance/geoblock/src/attestation.mjs`)

- EIP-712 `Attestation(address account,string statementId,bytes32 statementHash,string residenceCountry,uint64
  issuedAt,uint64 expiresAt)` in its own domain name `PosternPerps Attestation` (never accepted as an action or a
  checkpoint), validity at most 400 days, residence declared by the user; a declared residence the list denies
  makes the user exit-only. The Interface shows the full text before the signature (a wallet cannot display a long
  text reliably); the wallet shows the id, the hash and the declared country.
- Why a signature and not a click: the record must show that the key holder made the statement.
- Two points raised with 397 (cross-area, 2026-11-17): (1) its domain's `chainId` 7007701 is the value 391 found
  injected wallets refuse ("Provided chainId must match the active chainId"); SPEC 2.1 moved actions to 700770 for
  that reason; (2) without a `verifyingContract` (or a deployment-specific id) an attestation signed on staging is
  valid on production (the R395-2 class; low impact here since it moves no value, but the record should name the
  deployment).

## Not done / not proven

- Whether a signed self-declaration has any legal value in each jurisdiction: for counsel.
- 397 owns the stored record (fields, retention, where it is kept) and the typed data; this file owns the text.
