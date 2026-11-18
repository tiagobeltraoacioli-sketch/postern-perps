import { useEffect, useRef, useState } from 'react';
import { connectAccount, discoverWallets, validAccounts, validChain, walletError } from './wallet';
import type { Provider, WalletAccount, WalletOption } from './wallet';

export function useWallet() {
  const [wallets, setWallets] = useState<WalletOption[]>([]);
  const [account, setAccount] = useState<WalletAccount | null>(null);
  // The connected wallet's provider, for explicit transaction flows only.
  const [provider, setProvider] = useState<Provider | null>(null);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState('');
  const generation = useRef(0);
  const busy = useRef(false);
  const cleanup = useRef<() => void>(() => {});
  useEffect(() => {
    const stop = discoverWallets((wallet) =>
      setWallets((previous) =>
        previous.some((w) => w.id === wallet.id || w.provider === wallet.provider)
          ? previous
          : [...previous, wallet],
      ),
    );
    return () => {
      stop();
      cleanup.current();
      generation.current++;
      busy.current = false;
    };
  }, []);

  function disconnect() {
    generation.current++;
    busy.current = false;
    cleanup.current();
    cleanup.current = () => {};
    setAccount(null);
    setProvider(null);
    setPending(false);
    setError('');
  }

  async function connect(wallet: WalletOption) {
    if (busy.current) return;
    busy.current = true;
    const attempt = ++generation.current;
    cleanup.current();
    setAccount(null);
    setPending(true);
    setError('');
    // Observe before requesting accounts so changes during an open prompt invalidate it.
    let initializing = true;
    let disposed = false;
    let latestAccounts: string[] | null | undefined;
    let latestChain: unknown;
    let receivedChain = false;
    const onAccounts = (...args: unknown[]) => {
      if (disposed || attempt !== generation.current) return;
      latestAccounts = validAccounts(args[0]) ? [args[0][0]] : null;
      if (initializing) return;
      if (!latestAccounts) {
        disconnect();
        return;
      }
      const address = latestAccounts[0];
      setAccount((current) => (current ? { ...current, address } : null));
    };
    const onChain = (...args: unknown[]) => {
      if (disposed || attempt !== generation.current) return;
      latestChain = args[0];
      receivedChain = true;
      if (initializing) return;
      if (!validChain(latestChain)) {
        disconnect();
        return;
      }
      const chainId = latestChain;
      setAccount((current) => (current ? { ...current, chainId } : null));
    };
    const onDisconnect = () => {
      if (!disposed && attempt === generation.current) disconnect();
    };
    const provider = wallet.provider;
    const listeners = [
      ['accountsChanged', onAccounts],
      ['chainChanged', onChain],
      ['disconnect', onDisconnect],
    ] as const;
    cleanup.current = () => {
      if (disposed) return;
      disposed = true;
      for (const [event, listener] of listeners) {
        try {
          provider.removeListener?.(event, listener);
        } catch {
          // Local identity must clear even if an injected provider cannot clean up.
        }
      }
      try {
        if (wallet.id === 'postern-walletconnect') provider.disconnect?.();
      } catch {
        // A remote cleanup failure cannot keep the local account connected.
      }
    };
    try {
      for (const [event, listener] of listeners) provider.on?.(event, listener);
      const result = await connectAccount(wallet);
      if (attempt !== generation.current) return;
      if (latestAccounts !== undefined) {
        if (!latestAccounts) throw new Error('Account unavailable.');
        result.address = latestAccounts[0];
      }
      if (receivedChain) {
        if (!validChain(latestChain)) throw new Error('Network unavailable.');
        result.chainId = latestChain;
      }
      initializing = false;
      setAccount(result);
      setProvider(() => provider);
    } catch (failure) {
      if (attempt !== generation.current) return;
      cleanup.current();
      setError(walletError(failure));
    } finally {
      if (attempt === generation.current) {
        busy.current = false;
        setPending(false);
      }
    }
  }
  return {
    wallets,
    account,
    provider: account ? provider : null,
    pending,
    error,
    connect,
    disconnect,
  };
}
