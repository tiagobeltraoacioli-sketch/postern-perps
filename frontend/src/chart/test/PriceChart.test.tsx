import { describe, expect, it, vi } from 'vitest';
import { render, screen, fireEvent } from '@testing-library/react';
import { PriceChart, CHART_INTERFACE_VERSION } from '../index';
import type { ChartLevel, PriceChartProps } from '../types';
import { walk } from './fixtures';

const M15 = 900_000;
const T0 = Date.UTC(2026, 10, 17);
const candles = walk(50, T0, M15);

function props(extra: Partial<PriceChartProps> = {}): PriceChartProps {
  return { symbol: 'BTC-USD', base: 'BTC', interval: '15m', candles, ready: true, priceDecimals: 1, sizeDecimals: 4, tickSize: 0.5, ...extra };
}

describe('PriceChart', () => {
  it('interface version is exported', () => expect(CHART_INTERFACE_VERSION).toMatch(/^\d+\.\d+\.\d+$/));
  it('reads out the newest candle in the header', () => {
    render(<PriceChart {...props()} />);
    const last = candles[candles.length - 1];
    const head = document.querySelector('.pc-readout')!;
    expect(head.textContent).toContain(last.c.toLocaleString('en-US', { minimumFractionDigits: 1, maximumFractionDigits: 1 }));
    expect(head.textContent).toContain('UTC');
    expect(head.textContent).toContain('BTC');
  });
  it('has an accessible name and a summary with every level as the app wrote it', () => {
    const levels: ChartLevel[] = [
      { id: 'liq', kind: 'liquidation', side: 'buy', price: 1, priceText: '61,234.5', label: 'Liquidation' },
      { id: 'pos', kind: 'position', side: 'buy', price: 2, priceText: '66,900.0', label: 'Long 0.5000 BTC', detail: '+225.60 USDC' },
    ];
    render(<PriceChart {...props({ levels, mark: { price: 67000, text: '67,000.0' } })} />);
    const plot = screen.getByRole('group', { name: 'BTC-USD price chart' });
    const ids = plot.getAttribute('aria-describedby')!.split(' ');
    const text = ids.map((id) => document.getElementById(id)!.textContent).join(' ');
    expect(text).toContain('Liquidation at 61,234.5');
    expect(text).toContain('Long 0.5000 BTC at 66,900.0, +225.60 USDC');
    expect(text).toContain('Mark price 67,000.0');
    expect(plot.tabIndex).toBe(0);
  });
  it('interval switch only with a handler; the current one is pressed', () => {
    const onIntervalChange = vi.fn();
    const { rerender } = render(<PriceChart {...props()} />);
    expect(screen.queryByRole('group', { name: 'Candle interval' })).toBeNull();
    rerender(<PriceChart {...props({ onIntervalChange, intervals: ['15m', '1h'] })} />);
    expect(screen.getByRole('button', { name: '15m' }).getAttribute('aria-pressed')).toBe('true');
    fireEvent.click(screen.getByRole('button', { name: '1h' }));
    expect(onIntervalChange).toHaveBeenCalledWith('1h');
    fireEvent.click(screen.getByRole('button', { name: '15m' }));
    expect(onIntervalChange).toHaveBeenCalledTimes(1);
  });
  it('loading and empty states', () => {
    const { rerender } = render(<PriceChart {...props({ ready: false, candles: [] })} />);
    expect(screen.getByRole('status').textContent).toContain('Reading candles');
    rerender(<PriceChart {...props({ ready: true, candles: [] })} />);
    expect(screen.getByRole('status').textContent).toContain('No candles yet');
  });
  it('a notice is shown over the plot, danger as an alert, and the stale mark is written', () => {
    render(<PriceChart {...props({ stale: true, notice: { tone: 'danger', title: 'Market halted', text: 'Oracle unsafe.' } })} />);
    expect(screen.getByRole('alert').textContent).toBe('Market halted' + 'Oracle unsafe.');
    expect(document.querySelector('.pc-stale')?.textContent).toBe('stale');
  });
  it('says how many candles it refused to draw', () => {
    const bad = [...candles.slice(0, 5), { ...candles[5], h: -1 }, ...candles.slice(6)];
    render(<PriceChart {...props({ candles: bad })} />);
    expect(screen.getByText(/1 candle not drawn: invalid data/)).toBeTruthy();
  });
  it('keyboard keys used by the chart do not scroll the page', () => {
    render(<PriceChart {...props()} />);
    const plot = screen.getByRole('group', { name: 'BTC-USD price chart' });
    const ev = new KeyboardEvent('keydown', { key: 'ArrowLeft', bubbles: true, cancelable: true });
    plot.dispatchEvent(ev);
    expect(ev.defaultPrevented).toBe(true);
    const other = new KeyboardEvent('keydown', { key: 'Tab', bubbles: true, cancelable: true });
    plot.dispatchEvent(other);
    expect(other.defaultPrevented).toBe(false);
  });
});
