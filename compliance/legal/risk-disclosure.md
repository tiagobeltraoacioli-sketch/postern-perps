# PosternDEX Perpetuals: Risk Disclosure

Document id: `RISK-0.2-20261117`. Status: DRAFT FOR COUNSEL. Not legal advice. UNAUDITED prototype: nothing is
live, no real funds. Prepared by agent 392 from the system design (`docs/SPEC.md` v0.1, `docs/MARKETS.md` v0.2.1,
the node and vault designs); every mechanism named here must be re-checked against the final code before use.
Placeholders: `TBD-owner` (facts only the owner can give), `TBD-counsel` (legal choices).

Read this before you deposit. You can lose all the funds you deposit, and in some cases more quickly than you
expect.

## 1. What you are trading

A perpetual future is a contract with no expiry that follows the price of an asset (BTC, ETH, SOL) against the US
dollar. You never own the asset. Your profit or loss is settled in bUSDC, a token on the Bloch L2.

## 2. Leverage and liquidation

- Leverage multiplies gains and losses. At 20x a 5% move against you consumes your whole initial margin.
- Your position is liquidated when your account value falls below the maintenance requirement (half of the initial
  requirement at the maximum leverage of each tier). Liquidation closes the position through the order book at the
  prices available, never worse than the price at which your account would reach zero; a liquidation fee of 0.5%
  of the closed size goes to the insurance fund.
- In cross margin, every position shares your whole balance: a loss in one market can liquidate positions in
  others. In isolated margin, only the margin you assigned to that position is at risk.
- Requirements grow with position size (margin tiers). Large positions have lower maximum leverage.
- Liquidation uses the mark price, not the last trade. The mark can differ from the price you see elsewhere; it is
  bounded to 0.5% around the oracle index.

## 3. Gaps, halts and the oracle

- Prices come from an oracle: a committee of independent signers, each reading at least three public exchanges.
  If the oracle is stale or moves more than a bound in one round, the market halts. While a market is halted,
  only cancellations are accepted: you cannot open, close or reduce, and no liquidation, stop-loss or take-profit
  runs. When it resumes, the price may be far from where it halted; stops may execute at much worse prices.
- If the price jumps past the point where your account reaches zero, you lose your whole margin; the insurance
  fund covers the deficit, and if it is exhausted, auto-deleveraging applies (section 4).

## 4. Insurance fund, backstop and auto-deleveraging

When a liquidation cannot be completed through the book, a backstop account of the venue takes the remainder.
If neither the book, the backstop nor the insurance fund can absorb a loss, auto-deleveraging (ADL) reduces the
positions of traders on the profitable side, ranked by profit and leverage, at the bankrupt account's price. If you
hold a profitable leveraged position, ADL can close part or all of it without your instruction.

## 5. Funding

Every hour, longs pay shorts or shorts pay longs, depending on the difference between the contract price and the
index. Funding is capped at 0.05% of position value per hour (up to 1.2% per day) and can make a position lose money
even if the price does not move.

## 6. How the exchange runs, and its limits

- One sequencer, operated by `TBD-owner`, orders all actions into blocks. It cannot sign an action for you (every
  action carries your signature or that of an agent wallet you approved), and it cannot move funds out of the
  vault (withdrawals need the signatures of a committee of independent validators over the replayed state). It
  CAN delay, reorder or refuse to include your actions for a time. A forced-action path on the Bloch L2 lets you
  request a cancel, a close or a withdrawal that must be included within a deadline; if it is not, the vault
  enters escape mode.
- Escape mode: if the committee stops finalising the exchange state, you can withdraw your last proven balance
  directly from the vault with a proof, with positions valued at the last mark price by a fixed rule. This takes
  time and may give you less than your positions would be worth at current prices.
- The validator committee is set once. If a threshold of its members colluded, they could sign a false state.
- Large withdrawals are delayed, and withdrawals per time window are limited. A guardian can pause the vault but
  cannot move funds.

## 7. Collateral, bridge and network risks

- bUSDC is a token issued by the Bloch bridge on the Bloch L2 against USDC held on Ethereum. It depends on the
  bridge's signers and contracts, on the Bloch L2 and its finality, and on USDC itself. Circle (the USDC issuer)
  can freeze USDC; a bridge pause or failure can make bUSDC unredeemable or worth less than USDC.
- Deposits are credited only after the Bloch L2 finalises them; this can take minutes or longer.
- A withdrawal pays a flat fee of 1 bUSDC.

## 8. Software risk

The engine, node, vault and interface are an unaudited prototype. Errors in any of them can lose funds. No
external audit has been completed.

## 9. Market and liquidity risk

The order books are new and may be thin. Large orders can move the price; market orders fill within a worst price
you sign (and never beyond 5% from the mark). Open interest is capped per market; at the cap, orders that would
increase it are not filled.

## 10. Legal and regulatory risk

The service is not available in many jurisdictions, including the United States, Canada, the United Kingdom and
Brazil, and it is not available to persons on sanctions lists. If you are or become located in a restricted
jurisdiction, you may only cancel, reduce or close, and withdraw. Laws change; the service may become unavailable
to you. Tax treatment is your responsibility. `TBD-counsel`: whether any statement here must be adapted per
jurisdiction.

## 11. Keys and agents

Your wallet's key controls your account. Agent (API) wallets you approve can trade for you until they expire or
you revoke them, but cannot withdraw. Nobody can recover a lost key.

## Not done / not proven

- Every number above is from `docs/MARKETS.md` v0.2.1 and must equal the deployed genesis; no tool checks that yet.
- The escape-mode valuation rule, delays and limits are described from designs not yet final (`docs/VAULT.md`).
- Counsel has not reviewed any of this text.
