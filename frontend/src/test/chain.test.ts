import { describe, expect, it } from 'vitest';
import { blochL2, ChainError, ensureChain, sameChain } from '../wallet/chain';
import type { Provider } from '../wallet/wallet';

const L2 = blochL2({ l2ChainId: 700770, l2RpcUrl: 'https://l2rpc.invalid', explorerTx: null });

/** A wallet on `chain` that knows the chains in `known`. `stuck`: answers the switch but stays where it is. */
function fakeWallet(chain: string, known: string[], options: { stuck?: boolean; decline?: 'switch' | 'add'; unknownCode?: number } = {}) {
  const asked: { method: string; params?: unknown[] }[] = [];
  const provider: Provider = {
    async request(a) {
      asked.push(a);
      if (a.method === 'eth_chainId') return chain;
      const want = (a.params?.[0] as { chainId: string }).chainId;
      if (a.method === 'wallet_switchEthereumChain') {
        if (options.decline === 'switch') throw Object.assign(new Error('no'), { code: 4001 });
        if (!known.includes(want)) throw Object.assign(new Error('unknown chain'), { code: options.unknownCode ?? 4902 });
        if (!options.stuck) chain = want;
        return null;
      }
      if (a.method === 'wallet_addEthereumChain') {
        if (options.decline === 'add') throw Object.assign(new Error('no'), { code: 4001 });
        known.push(want);
        if (!options.stuck) chain = want;
        return null;
      }
      throw new Error('unsupported');
    },
  };
  return { provider, asked, methods: () => asked.map((a) => a.method) };
}

describe('wallet network (chain 700770)', () => {
  it('the network entry comes from the build configuration: Bloch L2, BLCH 18, the configured RPC', () => {
    expect(L2).toEqual({ chainId: 700770, chainName: 'Bloch L2', nativeCurrency: { name: 'Bloch', symbol: 'BLCH', decimals: 18 }, rpcUrls: ['https://l2rpc.invalid'] });
    expect(blochL2({ l2ChainId: 700770, l2RpcUrl: 'https://r.invalid', explorerTx: 'https://scan.invalid/tx/' }).blockExplorerUrls).toEqual(['https://scan.invalid']);
    expect(blochL2({ l2ChainId: 700770, l2RpcUrl: 'https://r.invalid', explorerTx: 'not a url' }).blockExplorerUrls).toBeUndefined();
  });
  it('sameChain reads only well-formed hex', () => {
    expect(sameChain('0xab162', 700770)).toBe(true);
    expect(sameChain('0xAB162', 700770)).toBe(true);
    expect(sameChain('0x1', 700770)).toBe(false);
    expect(sameChain(700770, 700770)).toBe(false);
    expect(sameChain('700770', 700770)).toBe(false);
    expect(sameChain(undefined, 700770)).toBe(false);
  });
  it('already on the L2: the wallet is asked nothing more', async () => {
    const w = fakeWallet('0xab162', ['0xab162']);
    await ensureChain(w.provider, L2);
    expect(w.methods()).toEqual(['eth_chainId']);
  });
  it('a known network is switched to, then the chain is read again', async () => {
    const w = fakeWallet('0x1', ['0x1', '0xab162']);
    await ensureChain(w.provider, L2);
    expect(w.methods()).toEqual(['eth_chainId', 'wallet_switchEthereumChain', 'eth_chainId']);
  });
  it('an unknown network (4902, or -32603) is added with exactly the configured entry', async () => {
    for (const unknownCode of [4902, -32603]) {
      const w = fakeWallet('0x1', ['0x1'], { unknownCode });
      await ensureChain(w.provider, L2);
      expect(w.methods()).toEqual(['eth_chainId', 'wallet_switchEthereumChain', 'wallet_addEthereumChain', 'eth_chainId']);
      expect(w.asked[2].params).toEqual([{ chainId: '0xab162', chainName: 'Bloch L2', nativeCurrency: { name: 'Bloch', symbol: 'BLCH', decimals: 18 }, rpcUrls: ['https://l2rpc.invalid'] }]);
    }
  });
  it('a wallet that answers the switch but stays on another network is not believed', async () => {
    const w = fakeWallet('0x1', ['0x1', '0xab162'], { stuck: true });
    await expect(ensureChain(w.provider, L2)).rejects.toThrow(/still not on Bloch L2/);
  });
  it('a declined switch or add is said so; another failure asks the user to switch in the wallet', async () => {
    await expect(ensureChain(fakeWallet('0x1', ['0x1', '0xab162'], { decline: 'switch' }).provider, L2)).rejects.toThrow(/declined the network switch/);
    await expect(ensureChain(fakeWallet('0x1', ['0x1'], { decline: 'add' }).provider, L2)).rejects.toThrow(/declined adding the network/);
    await expect(ensureChain(fakeWallet('0x1', ['0x1'], { unknownCode: -32000 }).provider, L2)).rejects.toThrow(ChainError);
  });
});
