/**
 * The vault on the L2 and the transactions the page asks the wallet for. UNAUDITED prototype.
 *
 * One read-only L2 client for the build's endpoint, one view of the vault (re-read while a page that shows it is
 * open and after every transaction), and one transaction at a time through a review sheet: built from typed values
 * (vault/vault.ts `buildTx`), described from its calldata, simulated, and only then handed to the wallet. A build
 * whose configuration names no vault or token has no deployment: nothing is read, built or sent (fail closed).
 */
import { createContext, useCallback, useContext, useEffect, useMemo, useRef, useState, type ReactNode } from 'react';
import type { Hex } from 'viem';
import { config } from '../lib/config';
import { Store, useStore } from '../lib/store';
import { L2, L2Error } from '../vault/l2';
import { buildTx, deploymentOf, describeTx, readVault, sendTx, TxRefused, VaultMismatch, waitReceipt, type Deployment, type Tx, type TxDescription, type TxInput, type VaultView } from '../vault/vault';
import type { Receipt } from '../vault/l2';
import { ChainError, ensureChain, sameChain } from '../wallet/chain';
import { walletError } from '../wallet/wallet';
import { useActions } from './actions';
import { toast } from '../ui/toasts';

export const deployment: Deployment | null = deploymentOf(config);
export const l2: L2 | null = deployment ? new L2(config.l2RpcUrl, config.l2ChainId) : null;
export const vaultView = new Store<VaultView | null>(null);
export const vaultError = new Store<string | null>(null);

let reading: Promise<void> | null = null;
/** Re-reads the vault for `account`. Concurrent calls share one read. An unreadable vault clears the view. */
export function refreshVault(account: string | null): Promise<void> {
  if (!deployment || !l2) return Promise.resolve();
  reading ??= readVault(l2, deployment, account)
    .then((v) => {
      vaultView.set(v);
      vaultError.set(null);
    })
    .catch((e) => {
      vaultView.set(null);
      vaultError.set(e instanceof VaultMismatch || e instanceof L2Error ? e.message : 'The vault could not be read from the Bloch L2.');
    })
    .finally(() => {
      reading = null;
    });
  return reading;
}

/** Keeps the vault view fresh while the calling component is mounted. */
export function useVault(account: string | null, everyMs = 12_000): { view: VaultView | null; error: string | null } {
  const view = useStore(vaultView);
  const error = useStore(vaultError);
  useEffect(() => {
    void refreshVault(account);
    const id = setInterval(() => void refreshVault(account), everyMs);
    return () => clearInterval(id);
  }, [account, everyMs]);
  // A view read for another account is not this account's view.
  const mine = view && (view.account?.address ?? null) === (account?.toLowerCase() ?? null) ? view : null;
  return { view: mine, error };
}

export type TxOutcome = { ok: boolean; title: string; text: string; receipt?: Receipt };
type PendingTx = {
  input: TxInput;
  tx: Tx;
  described: TxDescription;
  phase: 'review' | 'wallet' | 'mining' | 'done';
  hash: Hex | null;
  outcome: TxOutcome | null;
  onDone?: (outcome: TxOutcome) => void;
};
type Ctx = {
  /** True when the connected wallet reports the Bloch L2. */
  onL2: boolean;
  switching: boolean;
  networkError: string | null;
  switchNetwork: () => Promise<boolean>;
  requestTx: (input: TxInput, onDone?: (outcome: TxOutcome) => void) => void;
  pending: PendingTx | null;
  problem: string | null;
  confirm: () => Promise<void>;
  close: () => void;
};
const FundsContext = createContext<Ctx | null>(null);

export function FundsProvider({ children }: { children: ReactNode }) {
  const { wallet, account } = useActions();
  const [pending, setPending] = useState<PendingTx | null>(null);
  const [problem, setProblem] = useState<string | null>(null);
  const [switching, setSwitching] = useState(false);
  const [networkError, setNetworkError] = useState<string | null>(null);
  const onL2 = !!wallet.account && sameChain(wallet.account.chainId, config.l2ChainId);
  const providerRef = useRef(wallet.provider);
  providerRef.current = wallet.provider;

  const switchNetwork = useCallback(async () => {
    const provider = providerRef.current;
    if (!provider) return false;
    setSwitching(true);
    setNetworkError(null);
    try {
      await ensureChain(provider);
      return true;
    } catch (e) {
      setNetworkError(e instanceof ChainError ? e.message : 'The wallet could not switch network.');
      return false;
    } finally {
      setSwitching(false);
    }
  }, []);

  const requestTx = useCallback<Ctx['requestTx']>((input, onDone) => {
    if (!deployment) return;
    setProblem(null);
    try {
      const tx = buildTx(deployment, input);
      setPending({ input, tx, described: describeTx(deployment, tx), phase: 'review', hash: null, outcome: null, onDone });
    } catch {
      setPending(null);
      toast('danger', 'Not built', 'The transaction could not be built from these values. Nothing was sent.');
    }
  }, []);

  const close = useCallback(() => setPending((p) => (p && (p.phase === 'wallet' || p.phase === 'mining') ? p : null)), []);

  const confirm = useCallback(async () => {
    const p = pending;
    const provider = providerRef.current;
    if (!p || p.phase !== 'review' || !deployment || !l2) return;
    if (!provider || !account) {
      setProblem('Connect a wallet first.');
      return;
    }
    setProblem(null);
    setPending({ ...p, phase: 'wallet' });
    let outcome: TxOutcome;
    try {
      const hash = await sendTx((a) => provider.request(a), l2, deployment, account, p.tx);
      setPending({ ...p, phase: 'mining', hash });
      const receipt = await waitReceipt(l2, hash);
      if (!receipt) outcome = { ok: false, title: 'Not in a block yet', text: `The transaction ${hash} was sent but is not in an L2 block after 3 minutes. It may still be included: check your wallet before sending it again.` };
      else if (receipt.status === 'reverted') outcome = { ok: false, title: 'Reverted on the L2', text: 'The transaction was included but the contract rejected it. Nothing moved except the gas fee.', receipt };
      else outcome = { ok: true, title: `In L2 block ${receipt.blockNumber}`, text: 'The transaction was included and succeeded.', receipt };
      setPending({ ...p, phase: 'done', hash, outcome });
    } catch (e) {
      if (e instanceof TxRefused) {
        // Refused before the wallet was asked: back to the review, with the reason.
        setPending({ ...p, phase: 'review' });
        setProblem(e.message);
        return;
      }
      outcome = e instanceof L2Error ? { ok: false, title: 'L2 not readable', text: `${e.message} If the wallet sent the transaction, it may still be included: check your wallet.` } : { ok: false, title: 'Not sent', text: `${walletError(e).replace('Connection declined', 'Declined in the wallet').replace('Could not connect to this wallet. Unlock it and try again.', 'The wallet did not send the transaction.')} Nothing was sent.` };
      setPending({ ...p, phase: 'done', outcome });
    }
    toast(outcome.ok ? 'ok' : 'danger', outcome.title, outcome.text);
    void refreshVault(account);
    p.onDone?.(outcome);
  }, [pending, account]);

  const value = useMemo<Ctx>(() => ({ onL2, switching, networkError, switchNetwork, requestTx, pending, problem, confirm, close }), [onL2, switching, networkError, switchNetwork, requestTx, pending, problem, confirm, close]);
  return <FundsContext.Provider value={value}>{children}</FundsContext.Provider>;
}

export function useFunds(): Ctx {
  const ctx = useContext(FundsContext);
  if (!ctx) throw new Error('useFunds outside FundsProvider');
  return ctx;
}
