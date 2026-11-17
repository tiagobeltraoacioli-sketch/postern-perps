# PosternDEX Perpetuals: Terms of Use

Document id: `TOU-0.2-20261117`. Status: DRAFT FOR COUNSEL. Not legal advice. UNAUDITED prototype: nothing is live.
This is a structure with the clauses the system needs; every clause is for counsel to accept, rewrite or remove.
Placeholders: `TBD-owner` (facts), `TBD-counsel` (legal choices).

## 1. Who we are

"PosternDEX Perpetuals" is software by Bloch Inc. The web interface at posterndex.com (the "Interface") is
operated by Bloch Inc (Panama), `TBD-owner` (registered name, registration number, address, contact). The
exchange engine is run by a sequencer operated by `TBD-owner` and a committee of independent validators; custody is
a smart contract (the "Vault") on the Bloch L2 (chain id 700770) that no single party controls. `TBD-counsel`:
how the roles of Bloch Inc, the sequencer operator and the validators are described and allocated.

## 2. Acceptance

By connecting a wallet and signing the attestation (section 4) you accept these Terms (`TOU-0.2-20261117`) and the
Risk Disclosure (`RISK-0.2-20261117`). A new version is shown to you and must be accepted again before you place a
new order.

## 3. Eligibility and restricted persons

You may not use the Interface to open or increase positions or to deposit if you are a resident of, located in, or
incorporated or established in a jurisdiction listed as restricted (the current list is published with its version
at `TBD-owner` and includes the United States and its territories, Canada, the United Kingdom, Brazil, mainland
China, Australia, Singapore, Cuba, Iran, North Korea, Syria, Russia, Belarus, Myanmar and the Crimea, Sevastopol, Donetsk, Luhansk,
Zaporizhzhia and Kherson regions of Ukraine), or if you are on a sanctions list or acting for such a person. The
Interface uses your network location and refuses access from VPNs, proxies and Tor. If you are restricted, you may
still cancel orders, reduce or close positions and withdraw, except where `TBD-counsel` decides otherwise for
sanctioned jurisdictions (Q-1). Addresses on a sanctions list are refused for every action. `TBD-counsel`: age
limit, professional/retail distinction, any further eligibility conditions.

## 4. Attestation

Before your first order you sign a statement (`ATTEST-0.1-20261117`) with your wallet. It is recorded with its
version, your address, the time and the country the Interface detected. Giving a false statement is a breach of
these Terms.

## 5. The service

- Orders, cancellations, leverage changes, transfers between margins, withdrawals and agent approvals are actions
  you sign. The engine executes them by the published rules (`docs/SPEC.md`); there is no discretion to execute an
  action differently from its signed content.
- Fees: as published in the fee schedule (`docs/MARKETS.md`), at the time of execution. Trading fees and the
  withdrawal fee are protocol revenue paid to the address `0x775a993F09Bd80aeCF8F8c2ADe0C84158E3a0544` on the Bloch
  L2. Liquidation fees go to the insurance fund.
- Market parameters (leverage, tiers, caps) are fixed at launch; changing them requires a new published version.
  `TBD-counsel`: notice period for changes.
- The service may halt a market automatically (oracle rules). Bloch Inc may suspend the Interface; this does not
  stop the forced-action path or escape mode of the Vault, which do not need the Interface.

## 6. No advice, no custody by Bloch Inc

Nothing on the Interface is investment, legal or tax advice. Bloch Inc does not hold your funds; the Vault does.
`TBD-counsel`: wording and effect of this clause.

## 7. Your responsibilities

You keep your keys safe; you are responsible for the actions signed by your keys and by agent wallets you approve;
you comply with the laws that apply to you, including tax; you do not manipulate markets, abuse the oracle, attack
the software or use the service for unlawful purposes.

## 8. Risks

You accept the risks described in the Risk Disclosure, including total loss, liquidation, auto-deleveraging,
halts, bridge and collateral risk and software errors.

## 9. Liability

`TBD-counsel`: limitation of liability, exclusions, indemnity; what is owed if the software errs.

## 10. Privacy

The Privacy Notice (`PRIV-0.1-20261117`) applies.

## 11. Changes and termination

`TBD-counsel`.

## 12. Law and disputes

`TBD-counsel`: governing law, forum or arbitration, language of the binding version.

## Not done / not proven

- Every bracketed placeholder; no clause has been reviewed by counsel.
- Whether a frontend operator may define the rules of a venue it does not run alone is itself a question for
  counsel (section 1).
