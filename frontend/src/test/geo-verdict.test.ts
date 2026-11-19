import { describe, expect, it } from 'vitest';
import { geoVerdict } from '../compliance/Restricted';
import { OPEN_GEO, RESTRICTED_GEO } from './fixtures';

describe('region verdict shown to the visitor', () => {
  it('open only when the exchange says open; restricted with the country; sanctioned first', () => {
    expect(geoVerdict(OPEN_GEO, 'ok')).toBe('open');
    expect(geoVerdict(RESTRICTED_GEO, 'ok')).toBe('restricted');
    expect(geoVerdict({ ...RESTRICTED_GEO, sanctioned: true }, 'ok')).toBe('sanctioned');
    expect(geoVerdict({ ...OPEN_GEO, sanctioned: true }, 'ok')).toBe('sanctioned');
  });
  it('an answer without a country, or with a policy other than open, is never open', () => {
    expect(geoVerdict({ ...RESTRICTED_GEO, country: null }, 'ok')).toBe('unknown');
    expect(geoVerdict({ ...OPEN_GEO, policy: 'exit_only' }, 'ok')).toBe('restricted');
    expect(geoVerdict({ ...OPEN_GEO, restricted: true }, 'ok')).toBe('restricted');
  });
  it('no answer from the exchange is not a verdict about the visitor, and is never open', () => {
    expect(geoVerdict(null, 'failed')).toBe('unreachable');
    expect(geoVerdict(null, 'ok')).toBe('unreachable');
    expect(geoVerdict(null, 'pending')).toBe('pending');
    expect(geoVerdict(OPEN_GEO, 'pending')).toBe('pending');
  });
});
