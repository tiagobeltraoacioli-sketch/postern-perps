/**
 * Deposits and withdrawals. UNAUDITED prototype.
 *
 * Deposit is an L2 transaction to the vault (docs/VAULT.md, agent 390). Until the vault address and its ABI are
 * committed in config/production.json the deposit form renders "not live", builds nothing and asks for nothing
 * (fail closed). Withdraw is a signed action (main key only) followed by the committee's certificate and the vault
 * payment; every intermediate state of docs/API.md 4.1 is named with what the user can do.
 */
import { useState } from 'react';
import { feed } from '../state/session';
import { useStore } from '../lib/store';
import { useActions } from '../state/actions';
import { config } from '../lib/config';
import { formatDateTime, formatDuration, formatUsd, shortAddress, DASH } from '../lib/format';
import { USDC_DECIMALS } from '../lib/fixed';
import { parseAt } from '../trade/order-builder';
import { validAddress } from '../wallet/wallet';
import type { DepositStatus, WithdrawalStatus } from '../lib/types';
import { useNow } from '../trade/hooks';

export const WITHDRAWAL_STEPS: [WithdrawalStatus, string, string][] = [
  ['requested', 'Requested', 'The exchange accepted your signed request.'],
  ['included', 'Debited', 'The engine took the amount from your trading account.'],
  ['finalized', 'In a final checkpoint', 'Validators replayed the block and its state root is final.'],
  ['certified', 'Certified', 'Enough validators signed the payment. Anyone may now submit it to the vault; the relayer does by default.'],
  ['submitted', 'Submitted', 'The vault payment transaction is on the Bloch L2.'],
  ['delayed', 'Held by the vault', 'A large amount waits out the vault delay before it is paid.'],
  ['paid', 'Paid', 'The vault paid the recipient on the Bloch L2.'],
];
export const DEPOSIT_WORDS: Record<DepositStatus, string> = {
  seen: 'Seen in an L2 block that is not final yet (the Bloch L2 finalizes a block per proof, minutes apart).',
  l2Finalized: 'Final on the L2; waiting to be credited by the exchange.',
  credited: 'Credited to your trading account.',
};

export function Transfers() {
  const { account, request } = useActions();
  const state = useStore(feed.account);
  const status = useStore(feed.status);
  const now = useNow(5000);
  const [amount, setAmount] = useState('');
  const [other, setOther] = useState(false);
  const [recipient, setRecipient] = useState('');
  const [checked, setChecked] = useState(false);
  const parsed = parseAt(amount, USDC_DECIMALS, USDC_DECIMALS);
  const withdrawable = state?.account.withdrawable ?? 0n;
  const to = other ? recipient.trim() : (account ?? '');
  const recipientOk = validAddress(to) && (!other || checked);
  const lastFee = state?.withdrawals.find((w) => w.fee > 0n)?.fee ?? null;
  const tooMuch = parsed.value !== null && parsed.value > withdrawable;
  const canWithdraw = !!account && parsed.value !== null && parsed.value > 0n && !tooMuch && recipientOk;
  const vaultMode = status?.vaultMode ?? 'unknown';

  return (
    <div className="page">
      <header className="page-head">
        <p className="eyebrow">Transfers</p>
        <h1 className="display">Deposit and withdraw</h1>
        <p className="lead">Collateral is bUSDC on the Bloch L2 (chain {config.l2ChainId}), held by the exchange's vault. Only your main wallet can withdraw; an agent key never can.</p>
      </header>
      {vaultMode !== 'normal' && (
        <p className={`callout ${vaultMode === 'escape' ? 'callout-danger' : 'callout-warn'}`} role="alert">
          {vaultMode === 'paused' && 'The vault is paused by a guardian: withdrawals are not paid until it resumes. Guardians can only pause; they cannot move funds.'}
          {vaultMode === 'escape' && 'The vault is in escape mode: no checkpoint was finalized in time. Withdraw your last proven balance directly from the vault with a Merkle proof (docs/VAULT.md).'}
          {vaultMode === 'unknown' && 'The vault state could not be read. Do not rely on a withdrawal being paid until it is.'}
        </p>
      )}
      <div className="two-col">
        <section className="panel" aria-label="Deposit">
          <header className="panel-head">
            <h2>Deposit</h2>
          </header>
          {config.vault ? (
            <p className="callout callout-warn">The vault interface (docs/VAULT.md) is not yet pinned in this build. Deposits stay off until it is.</p>
          ) : (
            <div className="not-live">
              <strong>Not live</strong>
              <p>The vault is not deployed. This page builds no transaction and asks your wallet for nothing.</p>
              <p className="fine">When live: approve bUSDC to the vault and call its deposit on the Bloch L2. The exchange credits a deposit only after the L2 block is final and two L2 endpoints agree.</p>
            </div>
          )}
          <h3>Your deposits</h3>
          <ul className="records">
            {(state?.deposits ?? []).map((d) => (
              <li key={d.id}>
                <span className="num rec-amount">{formatUsd(d.amount)} USDC</span>
                <span className={`badge status-${d.status}`}>{d.status === 'l2Finalized' ? 'L2 final' : d.status}</span>
                <span className="fine">{DEPOSIT_WORDS[d.status]}</span>
                <span className="num fine">
                  {formatDateTime(d.time)} · L2 block {d.l2Block} · {shortAddress(d.txHash)}
                </span>
              </li>
            ))}
            {account && state && state.deposits.length === 0 && <li className="empty">No deposits yet.</li>}
            {!account && <li className="empty">Connect a wallet to see deposits.</li>}
          </ul>
        </section>

        <section className="panel" aria-label="Withdraw">
          <header className="panel-head">
            <h2>Withdraw</h2>
          </header>
          <div className="amount">
            <div className="amount-label">
              <label htmlFor="w-amount">Amount</label>
              <button type="button" className="link-button" disabled={!state} onClick={() => setAmount(formatUsd(withdrawable, { dp: 6, rounding: 'floor', grouping: false }))}>
                Max {state ? formatUsd(withdrawable, { rounding: 'floor' }) : DASH}
              </button>
            </div>
            <div className="amount-box">
              <input id="w-amount" className="num" inputMode="decimal" value={amount} onChange={(e) => setAmount(e.target.value.replace(/[^\d.]/g, ''))} placeholder="0.00" />
              <span className="amount-unit">USDC</span>
            </div>
          </div>
          {parsed.error && <p className="msg-danger">Amount {parsed.error}.</p>}
          {tooMuch && <p className="msg-danger">Above your withdrawable balance (collateral not backing positions or orders).</p>}
          <fieldset className="radio-cards">
            <legend>Recipient on the Bloch L2</legend>
            <label className={!other ? 'selected' : ''}>
              <input type="radio" checked={!other} onChange={() => setOther(false)} />
              <strong>This wallet</strong>
              <span className="num">{account ?? 'Connect a wallet'}</span>
            </label>
            <label className={other ? 'selected' : ''}>
              <input type="radio" checked={other} onChange={() => setOther(true)} />
              <strong>Another address</strong>
              <span>Funds sent to a wrong address cannot be recovered.</span>
            </label>
          </fieldset>
          {other && (
            <>
              <label className="field">
                <span>Address</span>
                <input className="num" spellCheck={false} autoComplete="off" value={recipient} onChange={(e) => setRecipient(e.target.value)} placeholder="0x…" />
              </label>
              {recipient && !validAddress(recipient.trim()) && <p className="msg-danger">Not an address.</p>}
              <label className="check">
                <input type="checkbox" checked={checked} onChange={(e) => setChecked(e.target.checked)} />
                <span>I checked this address character by character. It is a wallet I control on the Bloch L2, not an exchange deposit address.</span>
              </label>
            </>
          )}
          <dl className="estimate">
            <div>
              <dt>Fee</dt>
              <dd>{lastFee !== null ? `${formatUsd(lastFee)} USDC flat (covers the L2 gas), deducted from the amount` : 'A flat fee covering the L2 gas, shown on the withdrawal record'}</dd>
            </div>
            <div>
              <dt>Time</dt>
              <dd>Minutes: a checkpoint, the committee's signatures, then the vault payment. Large amounts wait out the vault's delay.</dd>
            </div>
          </dl>
          <button
            type="button"
            className="button button-block button-primary"
            disabled={!canWithdraw}
            onClick={() =>
              request('Withdraw', (nonce, acct) => ({ account: acct, nonce: nonce.toString(), amount: parsed.value!.toString(), recipient: other ? recipient.trim() : acct }), (o) => o.ok && setAmount(''))
            }
          >
            Review withdrawal
          </button>
          <h3>Your withdrawals</h3>
          <ul className="records">
            {(state?.withdrawals ?? []).map((w) => {
              const at = WITHDRAWAL_STEPS.findIndex(([s]) => s === w.status);
              return (
                <li key={w.id}>
                  <span className="num rec-amount">{formatUsd(w.amount)} USDC</span>
                  <span className={`badge status-${w.status}`}>{w.status === 'rejected' ? 'rejected' : (WITHDRAWAL_STEPS[at]?.[1] ?? w.status)}</span>
                  <ol className="steps" aria-label={`Withdrawal ${w.id} progress`}>
                    {WITHDRAWAL_STEPS.filter(([s]) => s !== 'delayed' || w.status === 'delayed').map(([s, label], i) => (
                      <li key={s} className={i <= at ? 'done' : ''} title={label}>
                        <span className="sr-only">{label}</span>
                      </li>
                    ))}
                  </ol>
                  <span className="fine">
                    {w.status === 'rejected' ? 'The exchange rejected this withdrawal; nothing left your account.' : WITHDRAWAL_STEPS[at]?.[2]}
                    {w.status === 'delayed' && w.availableAt ? ` Payable in ${formatDuration(w.availableAt - now)} (${formatDateTime(w.availableAt)} UTC).` : ''}
                  </span>
                  <span className="num fine">
                    #{w.id} · {formatDateTime(w.time)} · to {shortAddress(w.recipient)} · fee {formatUsd(w.fee)}
                  </span>
                </li>
              );
            })}
            {account && state && state.withdrawals.length === 0 && <li className="empty">No withdrawals yet.</li>}
          </ul>
        </section>
      </div>
    </div>
  );
}
