/**
 * Funds: deposit, withdraw, and the exits that need no permission from the exchange. UNAUDITED prototype.
 *
 * Deposit: approve the exact amount, then `deposit` on the vault (docs/VAULT.md 4), followed on the L2 until the
 * block is final and the exchange credits it. Withdraw: a signed action (main key only); once the validators certify
 * it the page checks the record against the vault and anyone can collect it. Exit: a forced request on the L2 that
 * the exchange must process, and the escape mode when it does not. Everything about the vault is read from the L2
 * endpoint, not from the exchange API; a build that names no vault shows "Not live" and asks the wallet for nothing.
 */
import { useCallback, useEffect, useState, type ReactNode } from 'react';
import { ArrowDownToLine, ArrowUpFromLine, DoorOpen, ShieldCheck } from 'lucide-react';
import type { Hex } from 'viem';
import { feed, go, geo as geoStore, geoRead } from '../state/session';
import { geoVerdict } from '../compliance/Restricted';
import { useStore } from '../lib/store';
import { useActions } from '../state/actions';
import { deployment, l2, refreshVault, useFunds, useProofKeeper, useVault } from '../state/funds';
import { keptProofFor, proofFile, systemProofsIn } from '../vault/keeper';
import { api } from '../lib/api';
import { config } from '../lib/config';
import { formatDateTime, formatDuration, formatUsd, shortAddress, DASH } from '../lib/format';
import { USDC_DECIMALS } from '../lib/fixed';
import { parseAt } from '../trade/order-builder';
import { validAddress } from '../wallet/wallet';
import type { DepositStatus, Withdrawal, WithdrawalStatus } from '../lib/types';
import { useNow } from '../trade/hooks';
import {
  checkEscapeClaim,
  checkWithdrawalClaim,
  ClaimRefused,
  depositProblems,
  depositsOf,
  escapeAmount,
  escapeKey,
  forcedProblems,
  payoutState,
  phaseOf,
  readPayouts,
  readRefunded,
  acceptedCheckpointFor,
  silenceLeft,
  usdc,
  withdrawalKey,
  type ForcedKind,
  type Payout,
  type VaultDeposit,
  type VaultView,
} from '../vault/vault';
import { L2Error } from '../vault/l2';
import { parseAccountProof, parsePastedProof, parseWithdrawalCertificate, WireError } from '../vault/wire';
import type { AccountLeaf } from '../vault/merkle';

export type FundsTab = 'deposit' | 'withdraw' | 'exit';
export const FUNDS_TABS: FundsTab[] = ['deposit', 'withdraw', 'exit'];

export const WITHDRAWAL_STEPS: [WithdrawalStatus, string, string][] = [
  ['requested', 'Requested', 'The exchange accepted your signed request.'],
  ['included', 'Debited', 'The engine took the amount from your trading account.'],
  ['finalized', 'In a final checkpoint', 'Validators replayed the block and its state root is final.'],
  ['certified', 'Certified', 'Enough validators signed the payment. It can now be collected from the vault: anyone may submit it.'],
  ['submitted', 'Submitted', 'The vault payment transaction is on the Bloch L2.'],
  ['delayed', 'Held by the vault', 'A large amount waits out the vault delay before it is paid.'],
  ['paid', 'Paid', 'The vault paid the recipient on the Bloch L2.'],
];
export const DEPOSIT_WORDS: Record<DepositStatus, string> = {
  seen: 'Seen in an L2 block that is not final yet (the Bloch L2 finalizes a block per proof, minutes apart).',
  l2Finalized: 'Final on the L2; waiting to be credited by the exchange.',
  credited: 'Credited to your trading account.',
};

const seconds = (s: bigint) => formatDuration(Number(s) * 1000);
const l2Time = (s: bigint) => `${formatDateTime(Number(s) * 1000)} UTC`;

const PHASE_WORDS: Record<ReturnType<typeof phaseOf>, [string, string]> = {
  normal: ['Vault open', 'Deposits and certified withdrawals are being processed.'],
  paused: ['Vault paused', 'A guardian paused payments and deposits. Guardians can only pause, for a bounded time; they cannot move funds. Requests and the escape path stay open.'],
  escapeDue: ['Exchange overdue', 'The exchange missed a deadline. Anyone may now switch the vault to escape mode (the Exit tab).'],
  escapeUnproven: ['Escape mode', 'The exchange has stopped for good. One proof is still needed before accounts can claim (the Exit tab).'],
  escape: ['Escape mode', 'The exchange has stopped for good. Every account claims its last proven balance from the vault (the Exit tab).'],
};

export function Funds({ tab }: { tab: FundsTab }) {
  const { account } = useActions();
  const { view, error } = useVault(deployment ? account : null);
  const status = useStore(feed.status);
  const apiMode = status?.vaultMode ?? 'unknown';
  const phase = view ? phaseOf(view) : null;
  return (
    <div className="page funds">
      <header className="page-head">
        <p className="eyebrow">Funds</p>
        <h1 className="display">Deposit, withdraw, exit</h1>
        <p className="lead">
          Collateral is bUSDC on the Bloch L2 (chain {config.l2ChainId}), held by the exchange's vault: a contract with no admin key. Only your main wallet can withdraw; an agent key never can.
        </p>
      </header>
      {deployment ? (
        <section className={`vault-strip vault-${phase ?? 'unread'}`} aria-label="Vault state, read from the Bloch L2">
          <div className="vault-strip-main">
            <ShieldCheck size={18} aria-hidden="true" />
            <strong>{phase ? PHASE_WORDS[phase][0] : error ? 'Vault not readable' : 'Reading the vault…'}</strong>
            <span>{phase ? PHASE_WORDS[phase][1] : (error ?? 'Reading the contract on the Bloch L2.')}</span>
          </div>
          {view && (
            <dl className="vault-facts">
              <Fact label="Held by the vault" value={usdc(view.custody)} />
              <Fact label="Last checkpoint" value={view.lastCheckpointAt === 0n ? 'None yet' : `${seconds(view.now - view.lastCheckpointAt)} ago`} />
              <Fact label="Read at L2 block" value={view.block.toString()} />
            </dl>
          )}
        </section>
      ) : (
        apiMode !== 'normal' && (
          <p className={`callout ${apiMode === 'escape' ? 'callout-danger' : 'callout-warn'}`} role="alert">
            {apiMode === 'paused' && 'The exchange reports the vault paused by a guardian: withdrawals are not paid until it resumes. Guardians can only pause; they cannot move funds.'}
            {apiMode === 'escape' && 'The exchange reports the vault in escape mode: no checkpoint was finalized in time. Each account withdraws its last proven balance directly from the vault.'}
            {apiMode === 'unknown' && 'The vault state could not be read. Do not rely on a withdrawal being paid until it is.'}
          </p>
        )
      )}
      <div className="tabs funds-tabs" role="tablist" aria-label="Funds">
        {(
          [
            ['deposit', 'Deposit', 'Deposit', ArrowDownToLine],
            ['withdraw', 'Withdraw', 'Withdraw', ArrowUpFromLine],
            ['exit', 'Exit without the exchange', 'Exit', DoorOpen],
          ] as const
        ).map(([id, label, short, Icon]) => (
          <button key={id} type="button" role="tab" aria-selected={tab === id} aria-label={label} onClick={() => go({ page: 'transfers', tab: id })}>
            <Icon size={16} aria-hidden="true" />
            <span className="tab-long">{label}</span>
            <span className="tab-short" aria-hidden="true">
              {short}
            </span>
          </button>
        ))}
      </div>
      {tab === 'deposit' && <DepositTab view={view} />}
      {tab === 'withdraw' && <WithdrawTab view={view} />}
      {tab === 'exit' && <ExitTab view={view} />}
    </div>
  );
}

function Fact({ label, value }: { label: string; value: ReactNode }) {
  return (
    <div>
      <dt>{label}</dt>
      <dd className="num">{value}</dd>
    </div>
  );
}

function NotLive({ what }: { what: string }) {
  return (
    <div className="not-live">
      <strong>Not live</strong>
      <p>The vault is not deployed. This page builds no transaction and asks your wallet for nothing.</p>
      <p className="fine">{what}</p>
    </div>
  );
}

/** The account's deposits as the vault's own L2 log records them, re-read when the view moves. */
function useVaultDeposits(view: VaultView | null, account: string | null) {
  const [deposits, setDeposits] = useState<VaultDeposit[] | null>(null);
  const [finalized, setFinalized] = useState<bigint | null>(null);
  const block = view?.block ?? null;
  useEffect(() => {
    if (!deployment || !l2 || !account || block === null) {
      setDeposits(null);
      return;
    }
    let live = true;
    void (async () => {
      try {
        const list = await depositsOf(l2, deployment, account as Hex, BigInt(config.vaultDeployBlock ?? 0), block);
        const fin = await l2.head('finalized').catch(() => null);
        if (!live) return;
        setDeposits(list);
        setFinalized(fin ? fin.number : null);
      } catch {
        if (live) setDeposits(null);
      }
    })();
    return () => {
      live = false;
    };
  }, [account, block]);
  return { deposits, finalized };
}

/* ------------------------------------------------------------------ deposit */

function DepositTab({ view }: { view: VaultView | null }) {
  const { account } = useActions();
  const { requestTx } = useFunds();
  const state = useStore(feed.account);
  const [amount, setAmount] = useState('');
  const parsed = parseAt(amount, USDC_DECIMALS, USDC_DECIMALS);
  const { deposits, finalized } = useVaultDeposits(view, account);
  const value = parsed.value;
  const problems = view ? depositProblems(view, value) : [];
  const allowance = view?.account?.allowance ?? 0n;
  const needsApproval = value !== null && value > 0n && allowance < value;
  // Exit-only regions (docs/API.md 6): no new deposit is offered through this interface. Conservative default; the
  // rule itself is for counsel. Funds already in the vault leave by the Withdraw and Exit tabs.
  const verdict = geoVerdict(useStore(geoStore), useStore(geoRead));
  const regionOpen = verdict === 'open';
  const ready = !!view && !!account && value !== null && problems.length === 0 && regionOpen;
  const step = !ready ? 0 : needsApproval ? 1 : 2;
  return (
    <div className="two-col">
      <section className="panel" aria-label="Deposit">
        <header className="panel-head">
          <h2>Deposit bUSDC</h2>
        </header>
        {!deployment ? (
          <NotLive what="When live: approve bUSDC to the vault and call its deposit on the Bloch L2. The exchange credits a deposit only after the L2 block is final and two L2 endpoints agree." />
        ) : (
          <>
            <div className="amount">
              <div className="amount-label">
                <label htmlFor="d-amount">Amount</label>
                <button type="button" className="link-button" disabled={!view?.account} onClick={() => view?.account && setAmount(formatUsd(view.account.balance, { dp: 6, rounding: 'floor', grouping: false, trim: true, minDp: 2 }))}>
                  In this wallet: {view?.account ? usdc(view.account.balance) : DASH}
                </button>
              </div>
              <div className="amount-box">
                <input id="d-amount" className="num" inputMode="decimal" value={amount} onChange={(e) => setAmount(e.target.value.replace(/[^\d.]/g, ''))} placeholder="0.00" />
                <span className="amount-unit">bUSDC</span>
              </div>
            </div>
            {parsed.error && <p className="msg-danger">Amount {parsed.error}.</p>}
            {amount !== '' && !parsed.error && problems.map((p) => <p key={p} className="msg-danger">{p}</p>)}
            {!account && <p className="msg-warn">Connect a wallet to deposit.</p>}
            {verdict !== 'open' && verdict !== 'pending' && (
              <p className="callout callout-warn" role="status">
                {verdict === 'sanctioned'
                  ? 'This address cannot use PosternDEX Perpetuals.'
                  : verdict === 'unreachable'
                    ? 'The exchange did not answer, so your region is not confirmed and this interface offers no new deposit now. Funds already in the vault can leave: use Withdraw or Exit.'
                    : 'Trading is not available in your region, so this interface offers no new deposit. Funds already in the vault can leave: use Withdraw or Exit.'}
              </p>
            )}
            <ol className="flow" aria-label="Deposit steps">
              <FlowStep n={1} state={step > 1 || (ready && !needsApproval) ? 'done' : step === 1 ? 'now' : 'todo'} title="Allow the vault to take this amount" text="An approval for exactly this amount, never unlimited. Skipped when the vault already has it.">
                {step === 1 && (
                  <button type="button" className="button button-primary" onClick={() => requestTx({ kind: 'approve', amount: value! }, () => void refreshVault(account))}>
                    Review allowance of {usdc(value!)}
                  </button>
                )}
              </FlowStep>
              <FlowStep n={2} state={step === 2 ? 'now' : 'todo'} title="Deposit into the vault" text="The vault takes the bUSDC and records it for your trading account on the L2.">
                {step === 2 && (
                  <button type="button" className="button button-primary" onClick={() => requestTx({ kind: 'deposit', amount: value!, account: account as Hex }, (o) => o.ok && setAmount(''))}>
                    Review deposit of {usdc(value!)}
                  </button>
                )}
              </FlowStep>
              <FlowStep n={3} state="todo" title="The L2 block becomes final" text="The Bloch L2 finalizes a block per proof, minutes apart. Until then the deposit is pending and nothing is credited." />
              <FlowStep n={4} state="todo" title="The exchange credits your trading account" text="Only from a final L2 block that two independent L2 endpoints agree on. If it is never credited, the Exit tab refunds it from the vault." />
            </ol>
            {view && (
              <dl className="estimate">
                <div>
                  <dt>Minimum</dt>
                  <dd className="num">{usdc(view.minDeposit)}</dd>
                </div>
                <div>
                  <dt>Room under the vault cap</dt>
                  <dd className="num">{usdc(view.cap > view.custody ? view.cap - view.custody : 0n)}</dd>
                </div>
                <div>
                  <dt>Deposit fee</dt>
                  <dd>None. You pay the L2 gas in BLCH.</dd>
                </div>
              </dl>
            )}
          </>
        )}
      </section>
      <section className="panel" aria-label="Your deposits">
        <header className="panel-head">
          <h2>Your deposits</h2>
          {deposits && <span className="fine">As the vault recorded them on the L2</span>}
        </header>
        <ul className="records">
          {deposits
            ? [...deposits].reverse().map((d) => {
                const fromApi = state?.deposits.find((x) => x.txHash.toLowerCase() === d.txHash.toLowerCase());
                const credited = fromApi?.status === 'credited' || (view !== null && d.index < view.latest.nextDepositIndex);
                const final = finalized !== null && d.block <= finalized;
                const label = credited ? 'Credited' : final ? 'L2 final' : 'Pending';
                return (
                  <li key={d.index.toString()}>
                    <span className="num rec-amount">{usdc(d.amount)}</span>
                    <span className={`badge status-${credited ? 'credited' : final ? 'l2Finalized' : 'seen'}`}>{label}</span>
                    <ol className="steps" aria-label={`Deposit ${d.index} progress`}>
                      {['In an L2 block', 'L2 final', 'Credited'].map((s, i) => (
                        <li key={s} className={i === 0 || (i === 1 && (final || credited)) || (i === 2 && credited) ? 'done' : ''} title={s}>
                          <span className="sr-only">{s}</span>
                        </li>
                      ))}
                    </ol>
                    <span className="fine">
                      {credited
                        ? fromApi?.status === 'credited'
                          ? DEPOSIT_WORDS.credited
                          : 'Credited: a vault checkpoint signed by the validators acknowledges it.'
                        : final
                          ? DEPOSIT_WORDS.l2Finalized
                          : `In L2 block ${d.block}, not final yet. The Bloch L2 finalizes a block per proof, minutes apart; nothing is credited before that.`}
                    </span>
                    <span className="num fine">
                      Vault deposit #{d.index.toString()} · L2 block {d.block.toString()} · {shortAddress(d.txHash)}
                    </span>
                  </li>
                );
              })
            : (state?.deposits ?? []).map((d) => (
                <li key={d.id}>
                  <span className="num rec-amount">{formatUsd(d.amount)} USDC</span>
                  <span className={`badge status-${d.status}`}>{d.status === 'l2Finalized' ? 'L2 final' : d.status}</span>
                  <span className="fine">{DEPOSIT_WORDS[d.status]}</span>
                  <span className="num fine">
                    {formatDateTime(d.time)} · L2 block {d.l2Block} · {shortAddress(d.txHash)}
                  </span>
                </li>
              ))}
          {account && ((deposits && deposits.length === 0) || (!deposits && state && state.deposits.length === 0)) && <li className="empty">No deposits yet.</li>}
          {!account && <li className="empty">Connect a wallet to see deposits.</li>}
        </ul>
      </section>
    </div>
  );
}

function FlowStep({ n, state, title, text, children }: { n: number; state: 'todo' | 'now' | 'done'; title: string; text: string; children?: ReactNode }) {
  return (
    <li className={`flow-step flow-${state}`} aria-current={state === 'now' ? 'step' : undefined}>
      <span className="flow-n num" aria-hidden="true">
        {state === 'done' ? '✓' : n}
      </span>
      <div>
        <strong>
          {title}
          {state === 'done' && <span className="sr-only"> (done)</span>}
        </strong>
        <p className="fine">{text}</p>
        {children}
      </div>
    </li>
  );
}

/* ------------------------------------------------------------------ withdraw */

function WithdrawTab({ view }: { view: VaultView | null }) {
  const { account, request } = useActions();
  const state = useStore(feed.account);
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
  // What the VAULT holds for each certified record (registered, held, paid), read from the L2 with the view.
  const [payouts, setPayouts] = useState<Record<string, Payout>>({});
  const certified = (state?.withdrawals ?? []).filter((w) => w.certificate !== null && /^\d{1,20}$/.test(w.id));
  const certifiedKey = certified.map((w) => w.id).join(',');
  const block = view?.block ?? null;
  useEffect(() => {
    if (!deployment || !l2 || block === null || certified.length === 0) return;
    let live = true;
    readPayouts(
      l2,
      deployment,
      certified.map((w) => withdrawalKey(BigInt(w.id))),
      block,
    )
      .then((list) => live && setPayouts(Object.fromEntries(certified.map((w, i) => [w.id, list[i]]))))
      .catch(() => {
        /* the records stay without the vault's word */
      });
    return () => {
      live = false;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [certifiedKey, block]);
  return (
    <div className="two-col">
      <section className="panel" aria-label="Withdraw">
        <header className="panel-head">
          <h2>Withdraw to the Bloch L2</h2>
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
            <dd>
              Minutes: a checkpoint, the validators' signatures, then the vault payment.
              {view ? ` ${usdc(view.largeThreshold)} or more waits ${seconds(view.largeDelay)} at the vault first.` : ' Large amounts wait out the vault\'s delay.'}
            </dd>
          </div>
        </dl>
        <button
          type="button"
          className="button button-block button-primary"
          disabled={!canWithdraw}
          onClick={() => request('Withdraw', (nonce, acct) => ({ account: acct, nonce: nonce.toString(), amount: parsed.value!.toString(), recipient: other ? recipient.trim() : acct }), (o) => o.ok && setAmount(''))}
        >
          Review withdrawal
        </button>
        <p className="fine">
          If the exchange does not answer or refuses you, your funds are not stuck:{' '}
          <button type="button" className="link-button" onClick={() => go({ page: 'transfers', tab: 'exit' })}>
            exit without the exchange
          </button>
          .
        </p>
      </section>
      <section className="panel" aria-label="Your withdrawals">
        <header className="panel-head">
          <h2>Your withdrawals</h2>
        </header>
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
                {deployment && view && account && w.certificate !== null && w.status !== 'rejected' && <Collect withdrawal={w} view={view} account={account} payout={payouts[w.id]} />}
              </li>
            );
          })}
          {account && state && state.withdrawals.length === 0 && <li className="empty">No withdrawals yet.</li>}
          {!account && <li className="empty">Connect a wallet to see withdrawals.</li>}
        </ul>
      </section>
    </div>
  );
}

/**
 * Collect a certified withdrawal from the vault. The exchange certifies; the vault pays whoever submits the record
 * (docs/API.md 4.1: the node's relayer submits checkpoints, not withdrawals). The record and its proof are checked
 * against the vault before the wallet is asked; the state shown under the record is the VAULT's, read from the L2.
 */
function Collect({ withdrawal: w, view, account, payout }: { withdrawal: Withdrawal; view: VaultView; account: string; payout: Payout | undefined }) {
  const { requestTx } = useFunds();
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState<string | null>(null);
  const state = payout ? payoutState(payout, view.now) : 'none';
  const collect = async () => {
    if (!deployment || !l2) return;
    setBusy(true);
    setMessage(null);
    const expect = { account, recipient: w.recipient, amount: [w.amount, w.amount - w.fee] };
    try {
      let parsed = parseWithdrawalCertificate(w.certificate);
      let claim;
      try {
        claim = await checkWithdrawalClaim(l2, deployment, view, parsed.record, parsed.siblings, expect);
      } catch (first) {
        // The certificate proves the record against the exchange's newest final checkpoint; the vault may hold an
        // older one. Ask for the proof against the checkpoint the vault did accept, and check that instead.
        const accepted = first instanceof ClaimRefused ? await acceptedCheckpointFor(l2, deployment, view, parsed.record.withdrawalId) : null;
        if (!accepted) throw first;
        parsed = parseWithdrawalCertificate(await api.get<unknown>(`/v1/withdrawals/${parsed.record.withdrawalId}/proof?count=${accepted.count}`));
        claim = await checkWithdrawalClaim(l2, deployment, view, parsed.record, parsed.siblings, expect);
      }
      const now = payoutState(claim.payout, view.now);
      if (now === 'paid') setMessage('The vault has already paid this withdrawal in full.');
      else if (now === 'waiting') setMessage(`Registered at the vault. A large amount is held until ${l2Time(claim.payout.readyAt)}; collect it then.`);
      else if (now === 'none') requestTx({ kind: 'withdraw', seq: claim.seq, record: parsed.record, siblings: claim.siblings, want: parsed.record.amount });
      else requestTx({ kind: 'release', key: withdrawalKey(parsed.record.withdrawalId), want: claim.payout.total - claim.payout.paid });
    } catch (e) {
      setMessage(e instanceof ClaimRefused ? e.message : e instanceof WireError ? `The certificate from the exchange cannot be used: ${e.message}.` : e instanceof L2Error ? e.message : 'The proof could not be read from the exchange or checked against the vault.');
    } finally {
      setBusy(false);
    }
  };
  return (
    <div className="rec-action">
      {state === 'paid' && payout ? (
        <span className="fine vault-word">
          <strong>Paid by the vault:</strong> <span className="num">{usdc(payout.paid)}</span> to <span className="num">{shortAddress(payout.recipient)}</span> (read from the L2).
        </span>
      ) : state === 'waiting' && payout ? (
        <span className="fine vault-word">
          <strong>Registered at the vault.</strong> A large amount is held until {l2Time(payout.readyAt)} (in {seconds(payout.readyAt - view.now)}); collect it then.
        </span>
      ) : (
        <>
          <button type="button" className="button button-small button-primary" onClick={() => void collect()} disabled={busy}>
            {busy ? 'Checking against the vault…' : state === 'partly' || state === 'payable' ? 'Collect the rest from the vault' : 'Collect from the vault'}
          </button>
          <span className="fine">One L2 transaction. Anyone may send it: the vault pays the recipient named in the certified record.</span>
        </>
      )}
      {message && (
        <p className="msg-warn" role="status">
          {message}
        </p>
      )}
    </div>
  );
}

/* ------------------------------------------------------------------ exit */

const FORCED_CHOICES: [ForcedKind, string, string][] = [
  ['withdraw', 'Withdraw', 'The exchange must pay out your free collateral (or the amount you name) to this wallet by the normal certified path.'],
  ['closeAll', 'Close all my positions', 'The exchange must close every position of this account: through the book, or in a halted market at the side of the exit band that is worse for you.'],
  ['cancelAll', 'Cancel all my orders', 'The exchange must cancel every open order of this account.'],
];

function ExitTab({ view }: { view: VaultView | null }) {
  const { account } = useActions();
  const { requestTx } = useFunds();
  const [kind, setKind] = useState<ForcedKind>('withdraw');
  const [amount, setAmount] = useState('');
  if (!deployment)
    return (
      <section className="panel exit-panel" aria-label="Exit without the exchange">
        <ExitExplainer view={null} />
        <NotLive what="When live: this tab posts your request on the Bloch L2, follows its deadline, and claims from the vault in escape mode." />
      </section>
    );
  const phase = view ? phaseOf(view) : null;
  const parsed = parseAt(amount, USDC_DECIMALS, USDC_DECIMALS);
  const arg = kind === 'withdraw' && amount !== '' ? parsed.value : 0n;
  const problems = view && arg !== null ? forcedProblems(view, kind, arg) : [];
  // R436-18: what the exchange reports about halts and this account's positions, read before a forced close is offered.
  const markets = useStore(feed.markets);
  const accountState = useStore(feed.account);
  const haltedAny = markets.filter((m) => m.status === 'halted').map((m) => m.symbol);
  const haltedHere = haltedAny.filter((s) => accountState?.account.positions.some((p) => p.symbol === s && p.size !== 0n));
  const open = view?.account?.forcedOpen ? view.account : null;
  const escaped = phase === 'escape' || phase === 'escapeUnproven';
  return (
    <div className="exit">
      <section className="panel exit-panel" aria-label="How the exit works">
        <ExitExplainer view={view} />
        <KeptProofLine view={view} account={account} />
      </section>
      <div className="two-col">
        <section className="panel" aria-label="Forced request">
          <header className="panel-head">
            <h2>1 · Make the exchange act</h2>
          </header>
          {escaped ? (
            <p className="callout callout-warn">The vault is in escape mode: requests are closed for good. Claim on the right.</p>
          ) : open ? (
            <div className={`callout ${view!.now > open.forcedDeadline! ? 'callout-danger' : 'callout-warn'}`} role="status">
              <strong>Request #{open.forcedIndex!.toString()} is open</strong>
              {view!.now > open.forcedDeadline! ? (
                <p>Its deadline passed on {l2Time(open.forcedDeadline!)} and no checkpoint acknowledged it. The exchange is overdue: escape mode can be started below.</p>
              ) : (
                <p>
                  The exchange must process it by {l2Time(open.forcedDeadline!)} (in {seconds(open.forcedDeadline! - view!.now)}). If it does not, anyone can start escape mode.
                </p>
              )}
              <ForcedWord account={account} index={open.forcedIndex!} />
            </div>
          ) : (
            <>
              <fieldset className="radio-cards">
                <legend>What the exchange must do</legend>
                {FORCED_CHOICES.map(([k, label, text]) => (
                  <label key={k} className={kind === k ? 'selected' : ''}>
                    <input type="radio" checked={kind === k} onChange={() => setKind(k)} />
                    <strong>{label}</strong>
                    <span>{text}</span>
                  </label>
                ))}
              </fieldset>
              {kind === 'withdraw' && (
                <div className="amount">
                  <div className="amount-label">
                    <label htmlFor="f-amount">Amount (leave empty for all free collateral)</label>
                  </div>
                  <div className="amount-box">
                    <input id="f-amount" className="num" inputMode="decimal" value={amount} onChange={(e) => setAmount(e.target.value.replace(/[^\d.]/g, ''))} placeholder="All" />
                    <span className="amount-unit">USDC</span>
                  </div>
                  <p className="fine">Recommended: leave it empty. An exact amount can fail if orders or positions hold part of it.</p>
                </div>
              )}
              {kind === 'withdraw' && amount !== '' && parsed.error && <p className="msg-danger">Amount {parsed.error}.</p>}
              {kind === 'closeAll' && haltedHere.length > 0 && (
                <div className="callout callout-danger" role="alert">
                  <strong>
                    {haltedHere.join(', ')} {haltedHere.length === 1 ? 'is' : 'are'} halted and you hold a position there
                  </strong>
                  <p>
                    A forced close now is not a close at the market: in a halted market it is priced at the side of the exit band that is worse for you, a discount from the last index that grows while the halt lasts. It can cost the whole margin of that position. To take out your free collateral without closing, choose Withdraw.
                  </p>
                </div>
              )}
              {kind === 'closeAll' && haltedHere.length === 0 && haltedAny.length > 0 && <p className="msg-warn">{haltedAny.join(', ')} halted now. A forced close of a position in a halted market is priced against the closer; you hold none there.</p>}
              {problems.map((p) => (
                <p key={p} className="msg-danger">
                  {p}
                </p>
              ))}
              <button type="button" className="button button-block button-primary" disabled={!view || !account || arg === null || problems.length > 0} onClick={() => requestTx({ kind: 'requestForced', forced: kind, arg: arg ?? 0n })}>
                Review request
              </button>
              {view && <p className="fine">The vault gives the exchange {seconds(view.forcedDelay)} from the request's L2 block. The request costs only L2 gas and cannot be blocked by the exchange or by a region rule.</p>}
            </>
          )}
        </section>
        <section className="panel" aria-label="Escape">
          <header className="panel-head">
            <h2>2 · If the exchange has stopped</h2>
          </header>
          {view ? <Escape view={view} account={account} /> : <p className="fine">Reading the vault…</p>}
        </section>
      </div>
    </div>
  );
}

/** What the EXCHANGE says about the open request (docs/API.md 4.1 `ForcedAction`). The deadline above is the vault's. */
function ForcedWord({ account, index }: { account: string | null; index: bigint }) {
  const [word, setWord] = useState<string | null>(null);
  useEffect(() => {
    if (!account) return;
    let live = true;
    api
      .get<{ forced?: { forcedId?: unknown; status?: unknown; outcome?: unknown; reason?: unknown }[] }>(`/v1/forced?account=${account}`)
      .then((r) => {
        const mine = (r.forced ?? []).find((f) => String(f.forcedId) === index.toString());
        if (!live) return;
        if (!mine) return setWord('The exchange does not list this request yet (it reads the L2 only from final blocks).');
        const reason = typeof mine.reason === 'string' && /^[A-Z0-9_]{1,48}$/.test(mine.reason) ? ` (${mine.reason})` : '';
        if (mine.outcome === 'served') setWord('The exchange reports it served. It counts once a vault checkpoint acknowledges it.');
        else if (mine.outcome === 'rejected') setWord(`The exchange reports it rejected${reason}: a reason on your side, such as nothing to withdraw. It counts as processed once a vault checkpoint acknowledges it.`);
        else if (mine.outcome === 'pending') setWord(`The exchange reports it cannot serve it now${reason}, for a reason you do not control. The vault's deadline keeps running.`);
        else setWord(mine.status === 'expired' ? 'The exchange itself reports the request expired.' : 'The exchange has received it and not yet processed it.');
      })
      .catch(() => live && setWord('The exchange API did not answer about this request. The vault\'s deadline does not depend on it.'));
    return () => {
      live = false;
    };
  }, [account, index]);
  return word ? <p className="fine">{word}</p> : null;
}

/** R436-11 (c): the account's own exit proof, kept on this device and offered as a file. */
function KeptProofLine({ view, account }: { view: VaultView | null; account: string | null }) {
  const keeper = useProofKeeper(view, account);
  if (keeper.state === 'off') return null;
  const save = () => {
    if (!keeper.kept) return;
    const file = proofFile(keeper.kept);
    const url = URL.createObjectURL(new Blob([file.text], { type: 'application/json' }));
    const a = document.createElement('a');
    a.href = url;
    a.download = file.name;
    document.body.appendChild(a);
    a.click();
    a.remove();
    setTimeout(() => URL.revokeObjectURL(url), 1000);
  };
  return (
    <div className={`proof-line ${keeper.state === 'missing' ? 'proof-missing' : ''}`} role="status">
      <strong>Your exit proof</strong>
      {keeper.state === 'kept' && keeper.kept && (
        <>
          <span>
            Saved on this device for the vault's latest checkpoint (exchange height <span className="num">{keeper.kept.height}</span>), checked against the vault.{' '}
            {keeper.kept.system
              ? 'It includes the insurance and backstop proofs the vault needs once before any claim: with this you can exit even if the exchange\'s API is gone.'
              : 'The insurance and backstop proofs the vault needs once before any claim could not be saved with it: in escape mode someone must still supply those two.'}
          </span>
          <button type="button" className="button button-small" onClick={save}>
            Save it as a file
          </button>
        </>
      )}
      {keeper.state === 'none' && <span>Nothing to keep yet: the proof exists once this account has deposited and the vault has accepted a checkpoint.</span>}
      {keeper.state === 'missing' && (
        <span>
          The exchange gave no usable proof for the vault's latest checkpoint{view ? ` (exchange height ${view.latest.height})` : ''}. Without one, an escape claim needs a proof from a validator or an archive.{' '}
          {keeper.older
            ? `The newest proof on this device is for an OLDER checkpoint (exchange height ${keeper.older.height}, saved ${formatDateTime(keeper.older.savedAt)} UTC) and cannot be used for the latest. `
            : ''}
          This page tries again at every checkpoint the vault accepts.
        </span>
      )}
    </div>
  );
}

function ExitExplainer({ view }: { view: VaultView | null }) {
  const forced = view ? seconds(view.forcedDelay) : '2 d';
  const silence = view ? seconds(view.escapeDelay) : '7 d';
  return (
    <div className="exit-explainer">
      <h2 className="exit-title">Your funds can always leave, even if the exchange is silent</h2>
      <p className="lead">
        One sequencer orders trades and could ignore you. It cannot keep your collateral: the vault on the Bloch L2 gives you two exits that need nobody's permission.
      </p>
      <ol className="exit-stages">
        <li>
          <span className="exit-stage-n num">1</span>
          <strong>Ask on the L2</strong>
          <p>Post a request to the vault: withdraw, close positions or cancel orders. The exchange must process it within {forced}.</p>
        </li>
        <li>
          <span className="exit-stage-n num">2</span>
          <strong>The exchange answers</strong>
          <p>Processed in time: your withdrawal is certified and paid like any other. Nothing else to do.</p>
        </li>
        <li>
          <span className="exit-stage-n num">3</span>
          <strong>Or the vault takes over</strong>
          <p>
            Ignored past the deadline, or no checkpoint at all for {silence}: anyone can switch the vault to escape mode. It is one way: the exchange stops for good.
          </p>
        </li>
        <li>
          <span className="exit-stage-n num">4</span>
          <strong>Everyone claims</strong>
          <p>Each account takes its last proven balance (positions closed at the last mark price) straight from the vault. Users are paid before the insurance fund.</p>
        </li>
      </ol>
    </div>
  );
}

const SYSTEM = { insurance: `0x${'0'.repeat(39)}2`, backstop: `0x${'0'.repeat(39)}3` } as const;

function Escape({ view, account }: { view: VaultView; account: string | null }) {
  const { requestTx } = useFunds();
  const phase = phaseOf(view);
  const left = silenceLeft(view);
  const [claim, setClaim] = useState<{ leaf: AccountLeaf; siblings: Hex[] } | null>(null);
  const [message, setMessage] = useState<string | null>(null);
  const [pasted, setPasted] = useState('');
  const [busy, setBusy] = useState(false);
  const { deposits } = useVaultDeposits(view, account);
  const [refunded, setRefunded] = useState<Record<string, boolean>>({});
  const uncredited = (deposits ?? []).filter((d) => d.index >= view.latest.nextDepositIndex);
  const uncreditedKey = uncredited.map((d) => d.index.toString()).join(',');
  const escaped = phase === 'escape' || phase === 'escapeUnproven';
  useEffect(() => {
    if (!escaped || !deployment || !l2 || uncredited.length === 0) return;
    let live = true;
    void readRefunded(
      l2,
      deployment,
      uncredited.map((d) => d.index),
      view.block,
    )
      .then((marks) => live && setRefunded(Object.fromEntries(uncredited.map((d, i) => [d.index.toString(), marks[i]]))))
      .catch(() => {
        /* the list stays without the refunded marks */
      });
    return () => {
      live = false;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [escaped, uncreditedKey, view.block]);

  const saved = account && deployment ? keptProofFor(deployment.vault, account, view.latest.accountsRoot) : null;
  const accept = useCallback(
    (proof: { leaf: AccountLeaf; siblings: Hex[] }) => {
      if (!account) return;
      checkEscapeClaim(view, proof.leaf, proof.siblings, account);
      setClaim(proof);
      setMessage(null);
    },
    [view, account],
  );
  const fetchProof = async () => {
    if (!account) return;
    setBusy(true);
    setMessage(null);
    try {
      accept(parseAccountProof(await api.get<unknown>(`/v1/accounts/${account}/proof?height=${view.latest.height}`)));
    } catch (e) {
      setClaim(null);
      setMessage(e instanceof ClaimRefused ? e.message : e instanceof WireError ? `The proof from the exchange cannot be used: ${e.message}.` : 'The exchange API did not return a proof. If it is down, paste a proof from any validator or archive below.');
    } finally {
      setBusy(false);
    }
  };
  const usePasted = () => {
    try {
      accept(parsePastedProof(pasted));
    } catch (e) {
      setClaim(null);
      setMessage(e instanceof ClaimRefused ? e.message : e instanceof WireError ? `That proof cannot be used: ${e.message}.` : 'That proof cannot be used.');
    }
  };
  const proveSystem = async () => {
    setBusy(true);
    setMessage(null);
    const send = (pair: { insurance: { leaf: AccountLeaf; siblings: readonly Hex[] }; backstop: { leaf: AccountLeaf; siblings: readonly Hex[] } }) => {
      checkEscapeClaim(view, pair.insurance.leaf, pair.insurance.siblings, SYSTEM.insurance);
      checkEscapeClaim(view, pair.backstop.leaf, pair.backstop.siblings, SYSTEM.backstop);
      requestTx({ kind: 'proveSystemShares', insurance: pair.insurance.leaf, insuranceSiblings: [...pair.insurance.siblings], backstop: pair.backstop.leaf, backstopSiblings: [...pair.backstop.siblings] });
    };
    try {
      // The proofs kept on this device with the user's own, then a pasted proof file, then the exchange API.
      const local = saved?.system ?? (pasted.trim() ? systemProofsIn(pasted) : null);
      if (local) return send(local);
      const [ins, back] = await Promise.all([api.get<unknown>(`/v1/accounts/${SYSTEM.insurance}/proof?height=${view.latest.height}`), api.get<unknown>(`/v1/accounts/${SYSTEM.backstop}/proof?height=${view.latest.height}`)]);
      send({ insurance: parseAccountProof(ins), backstop: parseAccountProof(back) });
    } catch (e) {
      setMessage(e instanceof ClaimRefused ? e.message : e instanceof WireError ? `The proofs from the exchange cannot be used: ${e.message}.` : 'The exchange API did not return the insurance and backstop proofs, and none are kept on this device. Paste a proof file that holds them (below), or wait for anyone who has them: this is needed once for everyone.');
    } finally {
      setBusy(false);
    }
  };

  if (phase === 'normal' || phase === 'paused')
    return (
      <>
        <p className="callout callout-ok">Escape is not due. The exchange is within its deadlines.</p>
        <dl className="estimate">
          <div>
            <dt>Last checkpoint accepted by the vault</dt>
            <dd className="num">{view.lastCheckpointAt === 0n ? 'None yet' : l2Time(view.lastCheckpointAt)}</dd>
          </div>
          <div>
            <dt>Escape by silence</dt>
            <dd>{left === null ? 'Starts counting at the first checkpoint. Before that, a forced request is the exit.' : `Possible in ${seconds(left)} if no checkpoint arrives.`}</dd>
          </div>
          <div>
            <dt>Escape by an ignored request</dt>
            <dd>Possible the moment any open request passes its deadline unprocessed.</dd>
          </div>
        </dl>
      </>
    );
  if (phase === 'escapeDue')
    return (
      <>
        <div className="callout callout-danger" role="alert">
          <strong>The exchange is overdue</strong>
          <p>{view.escapeReason === 2 ? 'A request on the L2 passed its deadline without being processed.' : `No checkpoint reached the vault for ${seconds(view.escapeDelay)}.`} The vault now lets anyone start escape mode.</p>
        </div>
        <p className="fine">
          <strong>One way.</strong> Starting escape mode ends the exchange for every user: no more trading, deposits or requests. Each account then claims its last proven balance; certified withdrawals stay payable in full; uncredited deposits are refunded.
        </p>
        <button type="button" className="button button-block button-danger" disabled={!account} onClick={() => requestTx({ kind: 'triggerEscape' })}>
          Review: start escape mode
        </button>
      </>
    );
  const payout = view.account?.escapePayout;
  const mine = payout ? payoutState(payout, view.now) : 'none';
  const estimate = claim ? escapeAmount(view, claim.leaf.escapeValue) : null;
  return (
    <>
      <p className="callout callout-danger" role="status">
        Escape mode since {l2Time(view.escapeAt)}. The exchange has stopped for good.
      </p>
      {phase === 'escapeUnproven' && (
        <div className="flow-block">
          <strong>First, once for everyone: prove the insurance and backstop balances</strong>
          <p className="fine">This fixes that users are paid before the insurance fund and the backstop. Anyone can send it; it pays nothing to the sender.</p>
          <button type="button" className="button" onClick={() => void proveSystem()} disabled={busy || !account}>
            {busy ? 'Reading proofs…' : 'Review the system proof'}
          </button>
          <span className="fine">{saved?.system ? 'The two proofs are kept on this device with your own.' : 'Taken from a proof file pasted below when it holds them, else from the exchange API.'}</span>
          {!saved?.system && (
            <details className="paste">
              <summary>Paste a proof file that holds the two system proofs</summary>
              <label className="field">
                <span>Proof file (JSON) saved from this page on any device</span>
                <textarea className="num" rows={4} spellCheck={false} value={pasted} onChange={(e) => setPasted(e.target.value)} />
              </label>
            </details>
          )}
        </div>
      )}
      {phase === 'escape' && payout && mine !== 'none' && (
        <div className="callout callout-ok" role="status">
          <strong>{mine === 'paid' ? 'Your escape balance was paid' : 'Your escape balance is registered'}</strong>
          <p className="num">
            {usdc(payout.paid)} of {usdc(payout.total)} paid to {shortAddress(payout.recipient)}.
            {mine === 'waiting' ? ` A large amount is held until ${l2Time(payout.readyAt)}.` : ''}
          </p>
          {(mine === 'payable' || mine === 'partly') && (
            <button type="button" className="button button-small" onClick={() => requestTx({ kind: 'release', key: escapeKey(account as Hex), want: payout.total - payout.paid })}>
              Collect the rest
            </button>
          )}
        </div>
      )}
      {phase === 'escape' && mine === 'none' && (
        <div className="flow-block">
          <strong>Claim your last proven balance</strong>
          <p className="fine">The claim needs a proof of your account in the last checkpoint the vault accepted. The page checks it against the vault before asking your wallet.</p>
          {!claim && (
            <>
              {saved && (
                <button
                  type="button"
                  className="button button-primary"
                  onClick={() => {
                    try {
                      accept(saved);
                    } catch (e) {
                      setMessage(e instanceof ClaimRefused ? e.message : 'The saved proof cannot be used.');
                    }
                  }}
                >
                  Use the proof saved on this device
                </button>
              )}
              <button type="button" className="button" onClick={() => void fetchProof()} disabled={busy || !account}>
                {busy ? 'Reading the proof…' : 'Get my proof from the exchange API'}
              </button>
              <details className="paste">
                <summary>The API is down: paste a proof</summary>
                <label className="field">
                  <span>Account proof (JSON) from any validator or archive</span>
                  <textarea className="num" rows={4} spellCheck={false} value={pasted} onChange={(e) => setPasted(e.target.value)} />
                </label>
                <button type="button" className="button button-small" onClick={usePasted} disabled={!pasted.trim() || !account}>
                  Check this proof
                </button>
              </details>
            </>
          )}
          {claim && (
            <>
              <dl className="estimate">
                <div>
                  <dt>Proven value</dt>
                  <dd className="num">{usdc(claim.leaf.escapeValue)}</dd>
                </div>
                <div>
                  <dt>The vault will pay</dt>
                  <dd className="num">{estimate === null ? DASH : usdc(estimate)}</dd>
                </div>
              </dl>
              {estimate !== null && estimate < claim.leaf.escapeValue && <p className="msg-warn">The vault holds less than all users are owed, so every claim is reduced in the same proportion (rounded down).</p>}
              {claim.leaf.escapeValue === 0n ? (
                <p className="msg-warn">This account's proven value is zero: there is nothing to claim.</p>
              ) : (
                <button type="button" className="button button-block button-primary" disabled={estimate === null || estimate === 0n} onClick={() => requestTx({ kind: 'claimEscape', leaf: claim.leaf, siblings: claim.siblings, want: estimate ?? 0n }, () => setClaim(null))}>
                  Review claim
                </button>
              )}
            </>
          )}
        </div>
      )}
      {message && (
        <p className="msg-warn" role="status">
          {message}
        </p>
      )}
      {uncredited.length > 0 && (
        <div className="flow-block">
          <strong>Deposits the exchange never credited</strong>
          <p className="fine">Refunded in full from the vault's own record. No proof and no validator is needed.</p>
          <ul className="records">
            {uncredited.map((d) => (
              <li key={d.index.toString()}>
                <span className="num rec-amount">{usdc(d.amount)}</span>
                <span className={`badge ${refunded[d.index.toString()] ? 'status-credited' : 'status-seen'}`}>{refunded[d.index.toString()] ? 'Refunded' : 'Refundable'}</span>
                {!refunded[d.index.toString()] && (
                  <button type="button" className="button button-small" onClick={() => requestTx({ kind: 'refundDeposit', index: d.index, account: d.account, amount: d.amount })}>
                    Review refund
                  </button>
                )}
                <span className="num fine">Vault deposit #{d.index.toString()} · L2 block {d.block.toString()}</span>
              </li>
            ))}
          </ul>
        </div>
      )}
    </>
  );
}
