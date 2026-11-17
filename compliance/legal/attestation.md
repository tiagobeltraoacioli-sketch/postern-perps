# PosternDEX Perpetuals: Restricted-Persons Attestation

Document id: `ATTEST-0.1-20261117`. Status: DRAFT FOR COUNSEL. Not legal advice. UNAUDITED prototype.

## Text the user reads and signs (exact; machine-readable copy in `notices.json` field `attestation`)

The text signed is the `attestation.text` string of `compliance/legal/notices.json`, byte for byte; its SHA-256 is
`attestation.sha256` in the same file (checked by the tests).

## How it is signed (Decided, technical, agent 392; the record format is agent 397's)

- Signed as EIP-712 typed data in the engine's signing domain of SPEC 2.1 (`{name "PosternPerps", version "1",
  chainId 700770, verifyingContract = the PerpsVault address pinned at build time}`; so a staging attestation
  never verifies against production, R395-2), primary type
  `Attestation(address account,string documentId,bytes32 documentHash,string country,uint64 issuedAt)`, where
  `documentHash` = SHA-256 of the exact UTF-8 text, `country` = the ISO code the Interface detected (or `XX`),
  `issuedAt` = seconds since the epoch. The wallet shows the document id and the country; the text itself is shown
  in full by the Interface before the signature (a wallet cannot display a long text in typed data reliably).
- Reason for a signature instead of a click: the record must show that the key holder made the statement; a
  click proves only that a browser did. Reason for typed data instead of `personal_sign` of the whole text:
  hardware wallets display typed-data fields; long personal messages are often shown as a hash only.
- This is not a trading action: the engine never sees it and it moves nothing. It must not be accepted as any
  action type (different primary type; 388 and 389 to confirm that no action type is named `Attestation`).
- A new document id requires a new signature before the next order (the Interface checks its stored version).

## Not done / not proven

- Whether a signed self-declaration has any legal value in each jurisdiction: for counsel.
- 397 owns the stored record (fields, retention, where it is kept); this file proposes the signed content only.
