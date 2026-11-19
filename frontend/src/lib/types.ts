/**
 * Wire types of docs/API.md v0.1 (agent 351) and the parsed forms the interface works with.
 * Wire values are decimal strings; `convert.ts` turns them into fixed-point integers once, at the boundary.
 */
import type { Tier } from './calc';

export type WireTier = { notionalFloor: string; initialMarginFraction: string; maintenanceMarginFraction: string; maxLeverage: string };
export type MarketStatus = 'active' | 'halted' | 'closeOnly' | 'settled';
export type WireMarket = {
  symbol: string;
  marketId: number;
  base: string;
  quote: string;
  status: MarketStatus;
  priceDecimals: number;
  sizeDecimals: number;
  tickSize: string;
  stepSize: string;
  minOrderSize: string;
  maxOrderSize: string;
  maxLeverage: string;
  marginTiers: WireTier[];
  makerFeeBps: string;
  takerFeeBps: string;
  liquidationFeeBps: string;
  fundingIntervalSec: number;
  maxFundingRate: string;
  /** Published by the node since API 0.3 of 2026-11-19 (F434-5, F434-6); the mock and older nodes have none. */
  markBandBps?: number;
  marketSlippageBps?: number;
  oracle: { sources: string[]; minSources: number; maxStalenessMs: number; maxDeviationBps: string; exitHaircutBps?: number; exitHaircutPerMinBps?: number; exitMaxHaircutBps?: number; maxHaltMs?: number };
  openInterestCap: string;
  haltReason?: string | null;
};
export type WireTicker = {
  symbol: string;
  markPrice: string;
  indexPrice: string;
  lastPrice: string;
  fundingRate: string;
  nextFundingTime: number;
  openInterest: string;
  volume24h: string;
  high24h: string;
  low24h: string;
  price24hAgo: string;
  change24h: string;
  height: number;
  time: number;
};
export type WireBook = { symbol: string; seq: number; prevSeq?: number; height: number; time: number; bids: [string, string][]; asks: [string, string][] };
export type WireTrade = { tradeId: string; symbol: string; price: string; size: string; side: 'buy' | 'sell'; time: number; height: number; liquidation: boolean };
export type WireCandle = { t: number; o: string; h: string; l: string; c: string; v: string; q: string; n: number };
export type WireFundingRate = { symbol: string; time: number; rate: string; premium: string; markPrice: string; indexPrice: string };
export type WirePosition = {
  symbol: string;
  size: string;
  entryPrice: string;
  /** Signed open notional, integer string in notional units (1e-16 USD): the node's exact figure (since b810675). */
  openNotional?: string;
  markPrice: string;
  notional: string;
  unrealizedPnl: string;
  realizedPnl: string;
  liquidationPrice: string | null;
  marginMode: 'cross' | 'isolated';
  leverage: string;
  isolatedMargin: string | null;
  initialMargin: string;
  maintenanceMargin: string;
  fundingSinceOpen: string;
};
export type WireAccount = {
  address: string;
  height: number;
  time: number;
  collateral: string;
  unrealizedPnl: string;
  equity: string;
  initialMargin: string;
  maintenanceMargin: string;
  freeCollateral: string;
  withdrawable: string;
  marginRatio: string;
  feeTier: { tier: number; makerFeeBps: string; takerFeeBps: string; volume30d: string };
  positions: WirePosition[];
  openOrders: number;
};
export type OrderType = 'limit' | 'market' | 'stopMarket' | 'stopLimit' | 'takeProfitMarket' | 'takeProfitLimit';
export type TimeInForce = 'GTC' | 'IOC' | 'POST_ONLY';
export type OrderStatus = 'untriggered' | 'open' | 'filled' | 'canceled' | 'rejected' | 'expired';
export type WireOrder = {
  orderId: string;
  clientOrderId: string | null;
  account: string;
  symbol: string;
  side: 'buy' | 'sell';
  type: OrderType;
  timeInForce: TimeInForce;
  price: string | null;
  size: string;
  filledSize: string;
  avgFillPrice: string | null;
  reduceOnly: boolean;
  triggerPrice: string | null;
  triggerCondition: 'markAbove' | 'markBelow' | null;
  status: OrderStatus;
  createdAt: number;
  updatedAt: number;
  height: number;
  rejectReason: string | null;
};
export type WireFill = {
  fillId: string;
  tradeId: string;
  orderId: string;
  account: string;
  symbol: string;
  side: 'buy' | 'sell';
  price: string;
  size: string;
  fee: string;
  liquidity: 'maker' | 'taker';
  realizedPnl: string;
  liquidation: boolean;
  time: number;
  height: number;
};
export type WireFundingPayment = { symbol: string; time: number; rate: string; positionSize: string; markPrice: string; payment: string };
export type WireAgent = { agent: string; name: string; approvedAt: number; expiresAt: number; height: number };
export type DepositStatus = 'seen' | 'l2Finalized' | 'credited';
export type WireDeposit = { depositId: string; account: string; amount: string; l2TxHash: string; logIndex: number; l2Block: number; status: DepositStatus; creditedHeight: number | null; time: number };
export type WithdrawalStatus = 'requested' | 'included' | 'finalized' | 'certified' | 'submitted' | 'delayed' | 'paid' | 'rejected';
export type WireWithdrawal = {
  withdrawalId: string;
  account: string;
  recipient: string;
  amount: string;
  fee: string;
  status: WithdrawalStatus;
  requestedHeight: number;
  checkpointHeight: number | null;
  certificate: unknown | null;
  l2TxHash: string | null;
  availableAt: number | null;
  time: number;
};
export type WireBlockRef = { height: number; time: number; hash: string; stateRoot: string; logHash: string };
export type VaultMode = 'normal' | 'paused' | 'escape' | 'unknown';
export type WireStatus = {
  name: string;
  version: string;
  apiVersion: string;
  mode: 'mock' | 'dev' | 'live';
  unaudited: boolean;
  network: {
    eip712: { name: string; version: string; chainId: number };
    collateralChainId: number;
    vault: string;
    collateralToken: string;
    collateralSymbol: string;
    collateralDecimals: number;
  };
  time: number;
  latestBlock: WireBlockRef;
  latestFinalized: WireBlockRef;
  sequencer: { state: 'running' | 'recovering' | 'halted'; blockIntervalMs: number; pendingActions: number };
  committee: { n: number; threshold: number; online: number };
  vaultMode: VaultMode;
  l2: { finalizedBlock: number; lastDepositBlock: number; endpointsAgree: boolean };
};
export type WireGeo = {
  country: string | null;
  restricted: boolean;
  policy: 'actions_refused' | 'exit_only' | 'open';
  listVersion: string;
  reasonCode: string | null;
  message: string | null;
  sanctioned: boolean;
};
export type WireError = { error: { code: string; message: string; details?: Record<string, unknown> } };
export type ActionAck = {
  actionHash: string;
  status: 'accepted' | 'included' | 'rejected';
  receivedAt: number;
  height?: number;
  index?: number;
  result?: { ok: boolean; orderId?: string; orderStatus?: OrderStatus; filledSize?: string; withdrawalId?: string; error: string | null };
};
export type WireAccountUpdate = {
  height: number;
  time: number;
  account?: WireAccount;
  orders?: WireOrder[];
  fills?: WireFill[];
  funding?: WireFundingPayment[];
  deposits?: WireDeposit[];
  withdrawals?: WireWithdrawal[];
};

/* ---------- Parsed forms ---------- */

export type Market = {
  symbol: string;
  marketId: number;
  base: string;
  status: MarketStatus;
  haltReason: string | null;
  priceDecimals: number;
  sizeDecimals: number;
  tick: bigint;
  step: bigint;
  minSize: bigint;
  maxSize: bigint;
  maxLeverage: number;
  tiers: Tier[];
  makerPpm: bigint;
  takerPpm: bigint;
  liquidationFeePpm: bigint;
  fundingIntervalSec: number;
  maxFundingRate: bigint;
  oracle: WireMarket['oracle'];
  /** The engine's band for market orders (and PRICE_BAND), ppm, as the exchange publishes it; null: not published. */
  marketSlippagePpm?: bigint | null;
  openInterestCap: bigint;
};
/** `GET /v1/oracle/{symbol}`: only what the page reads (the engine's current exit band of a halted market). */
export type WireOracleStatus = {
  symbol: string;
  status: string;
  haltedSince?: number | null;
  exitBand?: { low: string; high: string; settlePrice: string; settleDue: boolean; asOf: number } | null;
};
export type Ticker = {
  symbol: string;
  mark: bigint;
  index: bigint;
  last: bigint;
  fundingRate: bigint;
  nextFundingTime: number;
  openInterest: bigint;
  volume24h: bigint; // USDC units
  high24h: bigint;
  low24h: bigint;
  /** 24 h change in parts per million. */
  changePpm: bigint;
  time: number;
};
export type Trade = { id: string; px: bigint; size: bigint; buy: boolean; time: number; liquidation: boolean };
export type Candle = { t: number; o: number; h: number; l: number; c: number; v: number };
export type Position = {
  symbol: string;
  size: bigint;
  entryPx: bigint;
  /** The exchange's exact signed open notional (size x price units), or null when it gave none that can be used. */
  openNotional: bigint | null;
  markPx: bigint;
  notional: bigint;
  unrealised: bigint;
  realised: bigint;
  liquidationPx: bigint | null;
  isolated: boolean;
  leverage: number;
  isolatedMargin: bigint;
  initialMargin: bigint;
  maintenanceMargin: bigint;
  funding: bigint;
};
export type Account = {
  address: string;
  height: number;
  time: number;
  collateral: bigint;
  unrealised: bigint;
  equity: bigint;
  initialMargin: bigint;
  maintenanceMargin: bigint;
  free: bigint;
  withdrawable: bigint;
  marginRatioPpm: bigint | null;
  feeTier: { tier: number; makerPpm: bigint; takerPpm: bigint; volume30d: bigint };
  positions: Position[];
};
export type Order = {
  id: string;
  symbol: string;
  buy: boolean;
  type: OrderType;
  tif: TimeInForce;
  px: bigint | null;
  size: bigint;
  filled: bigint;
  avgPx: bigint | null;
  reduceOnly: boolean;
  triggerPx: bigint | null;
  triggerCondition: 'markAbove' | 'markBelow' | null;
  status: OrderStatus;
  createdAt: number;
  updatedAt: number;
  rejectReason: string | null;
};
export type Fill = {
  id: string;
  orderId: string;
  symbol: string;
  buy: boolean;
  px: bigint;
  size: bigint;
  fee: bigint;
  maker: boolean;
  realised: bigint;
  liquidation: boolean;
  time: number;
};
export type FundingPayment = { symbol: string; time: number; rate: bigint; size: bigint; markPx: bigint; payment: bigint };
export type Deposit = { id: string; amount: bigint; txHash: string; l2Block: number; status: DepositStatus; time: number };
export type Withdrawal = {
  id: string;
  recipient: string;
  amount: bigint;
  fee: bigint;
  status: WithdrawalStatus;
  txHash: string | null;
  availableAt: number | null;
  time: number;
  /** The exchange's certificate as it arrived (unparsed, untrusted): vault/wire.ts reads it, vault/vault.ts checks it. */
  certificate: unknown | null;
};
