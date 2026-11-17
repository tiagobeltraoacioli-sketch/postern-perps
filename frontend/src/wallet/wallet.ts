export type Provider = {
  isMetaMask?: boolean;
  disconnect?(): void;
  request(args: { method: string; params?: unknown[] }): Promise<unknown>;
  on?(event: string, listener: (...args: unknown[]) => void): void;
  removeListener?(event: string, listener: (...args: unknown[]) => void): void;
};
export type WalletOption = { id: string; name: string; provider: Provider; rdns?: string };
export type WalletAccount = { address: string; chainId: string; name: string };

const DISPLAY_CONTROLS = /[\u0000-\u001f\u007f-\u009f\u061c\u200e\u200f\u202a-\u202e\u2066-\u2069]/gu;
const MAX_RAW_WALLET_NAME_UNITS = 512;

/** Bound untrusted EIP-6963 and WalletConnect names before displaying them. */
export function walletDisplayName(value: unknown, fallback = 'Browser wallet'): string {
  if (typeof value !== 'string') return fallback;
  const cleaned = value
    .slice(0, MAX_RAW_WALLET_NAME_UNITS)
    .normalize('NFKC')
    .replace(DISPLAY_CONTROLS, ' ')
    .replace(/\s+/gu, ' ')
    .trim();
  return Array.from(cleaned).slice(0, 60).join('') || fallback;
}

// Fixed first-party destination: never forward query strings, wallet metadata,
// arbitrary return URLs or a preview host to a mobile wallet.
export const METAMASK_MOBILE_URL = 'https://metamask.app.link/dapp/blochdex.com/?connect=metamask';

/** Presentation hint only. Injected wallet metadata is not authenticated. */
export function isMetaMaskWallet(wallet: WalletOption): boolean {
  try {
    return (
      wallet.rdns === 'io.metamask' ||
      (wallet.id === 'legacy-injected' && wallet.provider.isMetaMask === true)
    );
  } catch {
    return false;
  }
}

declare global {
  interface Window {
    ethereum?: Provider;
  }
}

export function validAddress(value: unknown): value is string {
  return (
    typeof value === 'string' &&
    /^0x[0-9a-fA-F]{40}$/.test(value) &&
    !/^0x0{40}$/i.test(value)
  );
}
export function validAccounts(value: unknown): value is string[] {
  if (!Array.isArray(value) || value.length < 1 || value.length > 20) return false;
  // Array.every skips holes, which can otherwise validate an undefined identity.
  for (let index = 0; index < value.length; index++) {
    if (!validAddress(value[index])) return false;
  }
  return true;
}
export function validChain(value: unknown): value is string {
  return typeof value === 'string' && /^0x[1-9a-fA-F][0-9a-fA-F]{0,63}$/.test(value);
}

/** Discover providers without requesting accounts. Names are untrusted text; icons are not loaded. */
export function discoverWallets(onWallet: (wallet: WalletOption) => void): () => void {
  const seen = new Set<Provider>();
  const seenIds = new Set<string>();
  const announce = (event: Event) => {
    let wallet: WalletOption | undefined;
    try {
      const detail: unknown = (event as CustomEvent<unknown>).detail;
      if (!detail || typeof detail !== 'object') return;
      const { info, provider } = detail as {
        info?: { uuid?: unknown; name?: unknown; rdns?: unknown };
        provider?: Provider;
      };
      if (
        !provider ||
        typeof provider.request !== 'function' ||
        !info ||
        typeof info.uuid !== 'string' ||
        typeof info.name !== 'string'
      )
        return;
      const normalizedId = info.uuid.toLowerCase();
      if (
        !/^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(normalizedId) ||
        seen.has(provider) ||
        seenIds.has(normalizedId) ||
        seen.size >= 20
      )
        return;
      wallet = {
        id: info.uuid,
        name: walletDisplayName(info.name),
        provider,
        rdns: typeof info.rdns === 'string' ? info.rdns.slice(0, 253) : undefined,
      };
      seen.add(provider);
      seenIds.add(normalizedId);
    } catch {
      return;
    }
    onWallet(wallet);
  };
  let graceComplete = false;
  const scanLegacy = () => {
    if (!graceComplete) return;
    let provider: Provider | undefined;
    let isMetaMask = false;
    try {
      provider = window.ethereum;
      if (!provider || typeof provider.request !== 'function') return;
      isMetaMask = provider.isMetaMask === true;
    } catch {
      return;
    }
    if (!seen.has(provider) && seen.size < 20) {
      seen.add(provider);
      onWallet({
        id: 'legacy-injected',
        name: isMetaMask ? 'MetaMask' : 'Browser wallet',
        provider,
      });
    }
  };
  const rediscover = () => {
    window.dispatchEvent(new Event('eip6963:requestProvider'));
    scanLegacy();
  };
  const discoveryEvents = ['pageshow', 'focus', 'postern:discover-wallets'];
  window.addEventListener('eip6963:announceProvider', announce);
  window.addEventListener('ethereum#initialized', scanLegacy);
  discoveryEvents.forEach((event) => window.addEventListener(event, rediscover));
  window.dispatchEvent(new Event('eip6963:requestProvider'));
  // Preserve announcement precedence, then cover asynchronous mobile injection.
  // Retries are bounded; returning to the page or explicitly retrying can rescan later.
  const timers = [250, 500, 1000, 2000, 3000, 5000].map((delay) =>
    window.setTimeout(() => {
      graceComplete = true;
      scanLegacy();
    }, delay),
  );
  return () => {
    timers.forEach((timer) => window.clearTimeout(timer));
    window.removeEventListener('eip6963:announceProvider', announce);
    window.removeEventListener('ethereum#initialized', scanLegacy);
    discoveryEvents.forEach((event) => window.removeEventListener(event, rediscover));
  };
}

export async function connectAccount(wallet: WalletOption): Promise<WalletAccount> {
  const accounts = await wallet.provider.request({ method: 'eth_requestAccounts' });
  if (!validAccounts(accounts))
    throw new Error('The wallet did not return a valid account.');
  // Providers own their response arrays and may mutate them during the next request.
  const address = accounts[0];
  const chainId = await wallet.provider.request({ method: 'eth_chainId' });
  if (!validChain(chainId)) throw new Error('The wallet did not return a valid network.');
  return { address, chainId, name: walletDisplayName(wallet.name) };
}

export function walletError(error: unknown): string {
  let code: unknown;
  try {
    code = error && typeof error === 'object' && 'code' in error ? error.code : undefined;
  } catch {
    // Provider rejection values are untrusted, including accessor-backed objects.
  }
  if (code === 4001) return 'Connection declined. You can try again when you are ready.';
  if (code === -32002) return 'A connection request is already open. Check your wallet.';
  if (code === 4900 || code === 4901) return 'Your wallet is offline. Reconnect it and try again.';
  return 'Could not connect to this wallet. Unlock it and try again.';
}
