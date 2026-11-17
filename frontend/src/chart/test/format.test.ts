import { describe, expect, it } from 'vitest';
import { MINUS, decimalsOf, formatAxisTime, formatCandleTime, formatNumber, formatPercent, intervalWords } from '../format';

describe('formatNumber', () => {
  it('groups thousands and uses U+2212 for negatives', () => {
    expect(formatNumber(67123.4, 1)).toBe('67,123.4');
    expect(formatNumber(-1234567.891, 2)).toBe(`${MINUS}1,234,567.89`);
    expect(formatNumber(1234.5, 1, false)).toBe('1234.5');
  });
  it('never writes a negative zero', () => {
    expect(formatNumber(-0.0004, 2)).toBe('0.00');
    expect(formatNumber(-0, 0)).toBe('0');
  });
  it('rounds half away from zero on the exact double', () => {
    expect(formatNumber(0.125, 2)).toBe('0.13'); // 0.125 is exact in binary
    expect(formatNumber(-0.125, 2)).toBe(`${MINUS}0.13`);
  });
  it('exact: a decimal of up to 15 significant digits is written as itself at its own places', () => {
    // Every candle price parsed from the API's decimal strings (Number(s)) must come back as the same string.
    const samples = ['67123.4', '0.000123', '123456789012.345', '0.1', '99999.99999', '3.14159265358979', '1e-7'];
    for (const s of samples) {
      const v = Number(s);
      const places = s.includes('e-') ? Number(s.split('e-')[1]) : (s.split('.')[1] ?? '').length;
      const out = formatNumber(v, places, false);
      expect(Number(out)).toBe(v);
    }
    let x = 7;
    for (let i = 0; i < 2000; i++) {
      x = (x * 48271) % 2147483647;
      const s = `${x % 1000000}.${String(x % 100000).padStart(5, '0')}`; // 11 significant digits at most
      expect(formatNumber(Number(s), 5, false)).toBe(s);
    }
  });
  it('returns empty text for a non-finite value (nothing invented)', () => {
    expect(formatNumber(NaN, 2)).toBe('');
    expect(formatNumber(Infinity, 2)).toBe('');
  });
});

describe('formatPercent', () => {
  it('signs and two places', () => {
    expect(formatPercent(0.0002)).toBe('+0.02%');
    expect(formatPercent(-0.0125)).toBe(`${MINUS}1.25%`);
    expect(formatPercent(0)).toBe('0.00%');
    expect(formatPercent(-0.00001)).toBe('0.00%');
  });
});

describe('decimalsOf', () => {
  it('counts the places of a step', () => {
    expect(decimalsOf(0.5)).toBe(1);
    expect(decimalsOf(25)).toBe(0);
    expect(decimalsOf(0.0025)).toBe(4);
    expect(decimalsOf(0.1 + 0.2)).toBe(1); // float noise ignored
    expect(decimalsOf(0)).toBe(0);
  });
});

describe('times are UTC', () => {
  const t = Date.UTC(2026, 10, 17, 3, 45);
  it('axis labels by rank', () => {
    expect(formatAxisTime(t, 'time')).toBe('03:45');
    expect(formatAxisTime(t, 'day')).toBe('17 Nov');
    expect(formatAxisTime(t, 'month')).toBe('Nov');
    expect(formatAxisTime(t, 'year')).toBe('2026');
  });
  it('candle time', () => {
    expect(formatCandleTime(t, 900_000)).toBe('Tue 17 Nov 2026 03:45');
    expect(formatCandleTime(Date.UTC(2026, 10, 17), 86_400_000)).toBe('Tue 17 Nov 2026');
  });
  it('interval words', () => {
    expect(intervalWords(900_000)).toBe('15-minute');
    expect(intervalWords(14_400_000)).toBe('4-hour');
    expect(intervalWords(86_400_000)).toBe('1-day');
  });
});
