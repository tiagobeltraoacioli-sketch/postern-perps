/**
 * PosternDEX Perpetuals price chart (agent 396): an in-house canvas chart. UNAUDITED prototype work.
 *
 *   import { PriceChart } from '../chart';
 *
 * Only this file is the public surface; everything else in this directory is internal and may change.
 */
export const CHART_INTERFACE_VERSION = '0.1.0';

export { PriceChart } from './PriceChart';
export { chartStats, resetChartStats } from './engine';

export type {
  CandleStyle,
  ChartCandle,
  ChartFill,
  ChartFrameStats,
  ChartInterval,
  ChartLevel,
  ChartLevelKind,
  ChartNotice,
  ChartPrice,
  PriceChartProps,
} from './types';
