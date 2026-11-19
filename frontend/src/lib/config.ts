/** Build-time configuration (config/production.json, or a loopback file for a local render build; vite.config.ts). */
declare const __POSTERN_CONFIG__: {
  apiUrl: string;
  wsUrl: string;
  l2RpcUrl: string;
  l2ChainId: number;
  vault: string | null;
  collateral: string | null;
  explorerTx: string | null;
  /** L2 block the vault was deployed at (where the page starts reading the vault's own logs); absent: block 0. */
  vaultDeployBlock?: number;
};
declare const __POSTERN_LOCAL_RENDER__: boolean;

export const config = __POSTERN_CONFIG__;
export const LOCAL_RENDER = __POSTERN_LOCAL_RENDER__;
export const L2_CHAIN_HEX = `0x${config.l2ChainId.toString(16)}`;
