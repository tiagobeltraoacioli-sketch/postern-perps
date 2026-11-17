// Market parameters for the mock from 392's compliance/markets.json and compliance/risk.json (SPEC v0.1 section 10
// genesis names), converted to the mock's internal shape and to the API names of docs/API.md (tickSize = tick,
// stepSize = step, minOrderSize = minSize, maxOrderSize = maxSize, openInterestCap = oiCap). Simulation-only values
// (start price, volatility, maker level size) come from node/mock/fixtures/sim.json. UNAUDITED prototype.
//
// The converter refuses what SPEC section 10 says a genesis converter refuses: display decimals that differ from
// the precision of tick / step, tiers not strictly ascending, IMF not above MMF, an MMF not above markBandBps.
import { formatFixed } from './fixed.mjs';

const decimalsOf = (s) => (s.includes('.') ? s.split('.')[1].length : 0);
const ppmToFraction = (ppm) => trim(formatFixed(BigInt(ppm), 6));
const ppmToBps = (ppm) => trim(formatFixed(BigInt(ppm), 2));
const rateToFraction = (r) => trim(formatFixed(BigInt(r), 9));
function trim(t) {
  return t.includes('.') ? t.replace(/0+$/, '').replace(/\.$/, '') : t;
}

export function fromCompliance(marketsJson, riskJson, sim) {
  const out = [];
  for (const m of marketsJson.markets) {
    const where = `${m.symbol}`;
    if (decimalsOf(m.tick) !== m.priceDecimals) throw new Error(`${where}: priceDecimals ${m.priceDecimals} differs from the precision of tick ${m.tick}`);
    if (decimalsOf(m.step) !== m.sizeDecimals) throw new Error(`${where}: sizeDecimals ${m.sizeDecimals} differs from the precision of step ${m.step}`);
    let prev = null;
    for (const t of m.tiers) {
      if (prev !== null && !(Number(t.floor) > Number(prev))) throw new Error(`${where}: tiers not strictly ascending`);
      if (!(t.imfPpm > t.mmfPpm)) throw new Error(`${where}: IMF must exceed MMF`);
      if (!(t.mmfPpm > m.markBandBps * 100)) throw new Error(`${where}: MMF ${t.mmfPpm} ppm not above markBandBps ${m.markBandBps} (SPEC S-5.3)`);
      prev = t.floor;
    }
    const s = sim.markets[m.symbol];
    if (!s) throw new Error(`${where}: no simulation parameters in fixtures/sim.json`);
    out.push({
      symbol: m.symbol, marketId: m.marketId, base: m.base, quote: 'USD',
      priceDecimals: m.priceDecimals, sizeDecimals: m.sizeDecimals,
      tickSize: m.tick, stepSize: m.step, minOrderSize: m.minSize, maxOrderSize: m.maxSize,
      maxLeverage: String(m.maxLeverage), openInterestCap: m.oiCap,
      marginTiers: m.tiers.map((t) => ({ notionalFloor: t.floor, initialMarginFraction: ppmToFraction(t.imfPpm), maintenanceMarginFraction: ppmToFraction(t.mmfPpm), maxLeverage: String(Math.floor(1e6 / t.imfPpm)) })),
      makerFeeBps: ppmToBps(m.makerFeePpm), takerFeeBps: ppmToBps(m.takerFeePpm), liquidationFeeBps: ppmToBps(m.liquidationFeePpm),
      fundingIntervalSec: m.fundingIntervalMs / 1000, maxFundingRate: rateToFraction(m.maxFundingRate), interest: m.interest,
      markBandBps: m.markBandBps, marketSlippageBps: m.marketSlippageBps,
      oracle: { sources: sim.oracleSources, minSources: m.oracle.minSources, maxStalenessMs: m.oracle.maxStalenessMs, maxDeviationBps: String(m.oracle.agreeBps) },
      mockStartPrice: s.startPrice, mockVolBps: s.volBps, mockLevelSize: s.levelSize,
    });
  }
  return {
    source: `compliance/markets.json ${marketsJson.version} + compliance/risk.json ${riskJson.version}`,
    markets: out,
    defaults: { quote: 'USD', withdrawalFee: riskJson.global.withdrawalFee, feeRecipient: riskJson.global.feeRecipient.toLowerCase() },
  };
}
