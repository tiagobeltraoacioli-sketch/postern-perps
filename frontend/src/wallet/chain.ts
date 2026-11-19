/**
 * The wallet's network. UNAUDITED prototype.
 *
 * Both things the page asks a wallet for need the wallet on the Bloch L2 (chain 700770): vault transactions, and
 * EIP-712 signatures, whose domain names that chain (SPEC 2.1; wallets refuse typed data for another chain than the
 * active one). `ensureChain` asks the wallet to switch, adds the network when the wallet does not know it (EIP-3085,
 * error 4902), then RE-READS the wallet's chain: a wallet that says "done" but stays elsewhere is not believed.
 * The network entry offered to the wallet comes only from the build configuration, never from the API.
 */
import { config } from '../lib/config';
import type { Provider } from './wallet';

export type AddChainParameters = {
  chainId: number;
  chainName: string;
  nativeCurrency: { name: string; symbol: string; decimals: number };
  rpcUrls: string[];
  blockExplorerUrls?: string[];
};

/** The Bloch L2 as offered to a wallet. Native currency BLCH, 18 decimals (the L2's gas token). */
export function blochL2(c: { l2ChainId: number; l2RpcUrl: string; explorerTx: string | null } = config): AddChainParameters {
  let explorer: string | null = null;
  try {
    if (c.explorerTx) explorer = new URL(c.explorerTx).origin;
  } catch {
    explorer = null;
  }
  return {
    chainId: c.l2ChainId,
    chainName: 'Bloch L2',
    nativeCurrency: { name: 'Bloch', symbol: 'BLCH', decimals: 18 },
    rpcUrls: [c.l2RpcUrl],
    ...(explorer ? { blockExplorerUrls: [explorer] } : {}),
  };
}

export const chainHex = (chainId: number) => `0x${chainId.toString(16)}`;
/** True when a wallet-reported chain id (hex string) is `chainId`. Anything unreadable is "no". */
export function sameChain(reported: unknown, chainId: number): boolean {
  if (typeof reported !== 'string' || !/^0x[0-9a-fA-F]{1,64}$/.test(reported)) return false;
  return BigInt(reported) === BigInt(chainId);
}

export class ChainError extends Error {}

async function walletChain(provider: Provider): Promise<unknown> {
  return provider.request({ method: 'eth_chainId' });
}
const code = (error: unknown): unknown => {
  try {
    return error && typeof error === 'object' && 'code' in error ? (error as { code: unknown }).code : undefined;
  } catch {
    return undefined;
  }
};

/** Switch the wallet to `params.chainId`, adding the network when unknown. Throws ChainError in words otherwise. */
export async function ensureChain(provider: Provider, params: AddChainParameters = blochL2()): Promise<void> {
  if (sameChain(await walletChain(provider), params.chainId)) return;
  const chainId = chainHex(params.chainId);
  try {
    await provider.request({ method: 'wallet_switchEthereumChain', params: [{ chainId }] });
  } catch (error) {
    const c = code(error);
    if (c === 4001) throw new ChainError('You declined the network switch. Nothing was signed or sent.');
    // 4902: the wallet does not know the chain. Some wallets answer -32603 with the same meaning.
    if (c !== 4902 && c !== -32603) throw new ChainError(`The wallet could not switch to ${params.chainName}. Switch it in the wallet and try again.`);
    try {
      await provider.request({
        method: 'wallet_addEthereumChain',
        params: [{ chainId, chainName: params.chainName, nativeCurrency: params.nativeCurrency, rpcUrls: params.rpcUrls, ...(params.blockExplorerUrls?.length ? { blockExplorerUrls: params.blockExplorerUrls } : {}) }],
      });
    } catch (addError) {
      if (code(addError) === 4001) throw new ChainError('You declined adding the network. Nothing was signed or sent.');
      throw new ChainError(`The wallet could not add ${params.chainName} (chain ${params.chainId}). Add it in the wallet and try again.`);
    }
  }
  if (!sameChain(await walletChain(provider), params.chainId)) throw new ChainError(`The wallet is still not on ${params.chainName} (chain ${params.chainId}). Nothing was signed or sent.`);
}
