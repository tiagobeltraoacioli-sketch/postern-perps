/** Build-time configuration (config/production.json, or a loopback file for a local render build; vite.config.ts). */
declare const __POSTERN_CONFIG__: {
  apiUrl: string;
  wsUrl: string;
  l2RpcUrl: string;
  l2ChainId: number;
  vault: string | null;
  collateral: string | null;
  explorerTx: string | null;
};
declare const __POSTERN_LOCAL_RENDER__: boolean;

export const config = __POSTERN_CONFIG__;
export const LOCAL_RENDER = __POSTERN_LOCAL_RENDER__;
export const L2_CHAIN_HEX = `0x${config.l2ChainId.toString(16)}`;
