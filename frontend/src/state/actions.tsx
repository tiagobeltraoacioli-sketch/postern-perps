/**
 * Wallet, signer choice and the review sheet every action passes through. UNAUDITED prototype.
 *
 * `request(type, build)` opens the review: `build(nonce)` makes the message with a fresh nonce for the chosen signer,
 * the sheet shows it in words from the signed values (lib/eip712.ts `describe`) with every refusal, and only the
 * sheet's own button asks the wallet. Signer choice: an action an agent key may sign uses the agent key of this
 * device when one is approved and unexpired (no wallet prompt), unless the user switches to the wallet in the sheet;
 * everything else always uses the main wallet.
 */
import { createContext, useCallback, useContext, useMemo, useState, type ReactNode } from 'react';
import { api, ApiError, engineErrorWords, errorWords } from '../lib/api';
import { agentFor, agentSigner } from '../lib/agent';
import { AGENT_ALLOWED, type ActionType, type Message } from '../lib/eip712';
import { nextNonce, NonceError } from '../lib/nonce';
import { prepare, Refused, signAndSubmit, walletSigner, type Prepared, type Signer } from '../lib/sign';
import type { ActionAck } from '../lib/types';
import { useWallet } from '../wallet/useWallet';
import { walletError } from '../wallet/wallet';
import { currentGuard } from './session';
import { toast } from '../ui/toasts';

export type Outcome = { ok: boolean; title: string; text: string; ack?: ActionAck };
type Pending = {
  type: ActionType;
  build: (nonce: bigint, account: string) => Message;
  useAgent: boolean;
  prepared: Prepared | null;
  signer: Signer | null;
  phase: 'review' | 'signing' | 'done';
  outcome: Outcome | null;
  buildError: string | null;
  onDone?: (outcome: Outcome) => void;
};

type Ctx = {
  wallet: ReturnType<typeof useWallet>;
  account: string | null;
  /** The agent key of this device for the connected account, if approved and unexpired. */
  agentAddress: string | null;
  request: (type: ActionType, build: (nonce: bigint, account: string) => Message, onDone?: (o: Outcome) => void) => void;
  pending: Pending | null;
  switchSigner: (useAgent: boolean) => void;
  confirm: () => Promise<void>;
  close: () => void;
  refreshAgent: () => void;
};
const ActionContext = createContext<Ctx | null>(null);

function signerFor(useAgent: boolean, type: ActionType, wallet: ReturnType<typeof useWallet>): Signer | null {
  const address = wallet.account?.address;
  if (!address || !wallet.provider) return null;
  if (useAgent && AGENT_ALLOWED.includes(type)) {
    const agent = agentFor(address);
    if (agent) return agentSigner(agent);
  }
  const provider = wallet.provider;
  return walletSigner(address, (args) => provider.request(args));
}

function build(p: Pending, wallet: ReturnType<typeof useWallet>): Pending {
  const signer = signerFor(p.useAgent, p.type, wallet);
  const account = wallet.account?.address.toLowerCase() ?? null;
  if (!signer || !account) return { ...p, signer, prepared: null, buildError: 'Connect a wallet first.' };
  try {
    const message = p.build(nextNonce(signer.address, Date.now()), account);
    return { ...p, signer, prepared: prepare(p.type, message, signer, currentGuard(account)), buildError: null };
  } catch (e) {
    return { ...p, signer, prepared: null, buildError: e instanceof NonceError ? e.message : 'The request could not be built. Nothing was signed.' };
  }
}

export function outcomeOf(type: ActionType, ack: ActionAck): Outcome {
  if (ack.status === 'rejected') return { ok: false, title: 'Not accepted', text: 'The exchange did not include the request. Nothing changed.', ack };
  if (ack.status === 'accepted') return { ok: true, title: 'Accepted, not yet in a block', text: 'The sequencer queued the request. It is final only when it appears in a block; watch the account panel.', ack };
  if (ack.result && !ack.result.ok) return { ok: false, title: 'Rejected by the engine', text: engineErrorWords(ack.result.error), ack };
  const words: Partial<Record<ActionType, string>> = {
    PlaceOrder: ack.result?.orderStatus === 'filled' ? 'Order filled.' : ack.result?.orderStatus ? `Order ${ack.result.orderStatus}.` : 'Order placed.',
    CancelOrder: 'Order cancelled.',
    CancelAll: 'Orders cancelled.',
    UpdateLeverage: 'Leverage updated.',
    UpdateMargin: 'Isolated margin updated.',
    Withdraw: `Withdrawal ${ack.result?.withdrawalId ? `#${ack.result.withdrawalId} ` : ''}requested. Follow it on Transfers.`,
    ApproveAgent: 'Agent key approved.',
    RevokeAgent: 'Agent key revoked.',
  };
  return { ok: true, title: `Included in block ${ack.height ?? ''}`.trim(), text: words[type] ?? 'Done.', ack };
}

export function ActionProvider({ children }: { children: ReactNode }) {
  const wallet = useWallet();
  const [pending, setPending] = useState<Pending | null>(null);
  const [agentTick, setAgentTick] = useState(0);
  const account = wallet.account?.address.toLowerCase() ?? null;
  const agentAddress = useMemo(() => (account ? (agentFor(account)?.agent ?? null) : null), [account, agentTick]); // eslint-disable-line react-hooks/exhaustive-deps

  const request = useCallback<Ctx['request']>(
    (type, b, onDone) => {
      setPending(build({ type, build: b, useAgent: true, prepared: null, signer: null, phase: 'review', outcome: null, buildError: null, onDone }, wallet));
    },
    [wallet],
  );
  const switchSigner = useCallback((useAgent: boolean) => setPending((p) => (p && p.phase === 'review' ? build({ ...p, useAgent }, wallet) : p)), [wallet]);
  const close = useCallback(() => setPending((p) => (p?.phase === 'signing' ? p : null)), []);
  const confirm = useCallback(async () => {
    const p = pending;
    if (!p?.prepared || !p.signer || p.phase !== 'review' || p.prepared.refusals.length) return;
    setPending({ ...p, phase: 'signing' });
    let outcome: Outcome;
    try {
      const ack = await signAndSubmit(p.prepared, p.signer, () => currentGuard(account), (body) => api.action(body));
      outcome = outcomeOf(p.type, ack);
    } catch (e) {
      if (e instanceof Refused) outcome = { ok: false, title: 'Refused before signing or sending', text: e.reasons.join(' ') };
      else if (e instanceof ApiError) outcome = { ok: false, title: 'Refused by the exchange', text: errorWords(e) };
      else outcome = { ok: false, title: 'Not signed', text: `${walletError(e)} Nothing was submitted.` };
    }
    setPending({ ...p, phase: 'done', outcome });
    toast(outcome.ok ? 'ok' : 'danger', outcome.title, outcome.text);
    p.onDone?.(outcome);
  }, [pending, account]);

  const value = useMemo<Ctx>(
    () => ({ wallet, account, agentAddress, request, pending, switchSigner, confirm, close, refreshAgent: () => setAgentTick((t) => t + 1) }),
    [wallet, account, agentAddress, request, pending, switchSigner, confirm, close],
  );
  return <ActionContext.Provider value={value}>{children}</ActionContext.Provider>;
}

export function useActions(): Ctx {
  const ctx = useContext(ActionContext);
  if (!ctx) throw new Error('useActions outside ActionProvider');
  return ctx;
}
