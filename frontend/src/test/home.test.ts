/** The home page states fees and custody numbers FROM the files that own them: a change there changes the page, and
 *  the page cannot say a number that no file holds. */
import { describe, expect, it } from 'vitest';
import risk from '../../../compliance/risk.json';
import launch from '../../../compliance/markets.json';
import vault from '../../../contracts/deploy/vault.config.json';
import { FEES, VAULT_PROPOSAL } from '../pages/Home';

describe('home page figures', () => {
  it('base fees are tier 0 of compliance/risk.json and equal every launch market\'s own fee', () => {
    expect(FEES.base).toEqual(risk.feeTiers.tiers[0]);
    expect(FEES.base.tier).toBe(0);
    for (const m of launch.markets) expect([m.takerFeePpm, m.makerFeePpm], m.symbol).toEqual([FEES.base.takerFeePpm, FEES.base.makerFeePpm]);
    // The brief's base tier (taker 4.5 bps, maker 1.5 bps), as the data has it today.
    expect([FEES.base.takerFeePpm, FEES.base.makerFeePpm]).toEqual([450, 150]);
  });
  it('tiers only get cheaper with volume and no maker fee is negative (no rebate is promised)', () => {
    for (let i = 1; i < FEES.tiers.length; i++) {
      expect(BigInt(FEES.tiers[i].minVolume14d) > BigInt(FEES.tiers[i - 1].minVolume14d)).toBe(true);
      expect(FEES.tiers[i].takerFeePpm).toBeLessThanOrEqual(FEES.tiers[i - 1].takerFeePpm);
      expect(FEES.tiers[i].makerFeePpm).toBeLessThanOrEqual(FEES.tiers[i - 1].makerFeePpm);
      expect(FEES.tiers[i].makerFeePpm).toBeGreaterThanOrEqual(0);
    }
  });
  it('custody numbers are the vault configuration\'s', () => {
    expect(VAULT_PROPOSAL).toMatchObject({ validators: vault.validators.length, threshold: vault.threshold, forcedDelayMs: vault.forcedDelay * 1000, escapeDelayMs: vault.escapeDelay * 1000, largeDelayMs: vault.largeDelay * 1000 });
    expect(VAULT_PROPOSAL.threshold * 2).toBeGreaterThan(VAULT_PROPOSAL.validators);
    expect(VAULT_PROPOSAL.forcedDelayMs).toBeLessThanOrEqual(VAULT_PROPOSAL.escapeDelayMs);
  });
});
