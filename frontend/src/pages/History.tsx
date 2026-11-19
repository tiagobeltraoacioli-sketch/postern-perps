/**
 * History: closed orders, fills, funding payments and transfers of the connected account, as the exchange reports
 * them, with a market filter, totals computed in integers, and a CSV export of exactly the rows shown.
 */
import { useMemo, useState } from 'react';
import { Download } from 'lucide-react';
import { feed, go, type HistoryTab } from '../state/session';
import { useStore } from '../lib/store';
import { useActions } from '../state/actions';
import { formatDateTime, formatPx, formatRate, formatSize, formatUsd, shortAddress, DASH } from '../lib/format';
import { toDecimalString, PX_DECIMALS, SZ_DECIMALS, USDC_DECIMALS, RATE_DECIMALS } from '../lib/fixed';
import { orderTypeWords } from '../lib/eip712';
import { engineErrorWords } from '../lib/api';
import { GateGlyph } from '../ui/BrandMark';
import type { Market } from '../lib/types';

const TABS: [HistoryTab, string][] = [
  ['orders', 'Orders'],
  ['fills', 'Fills'],
  ['funding', 'Funding payments'],
  ['transfers', 'Transfers'],
];

/** RFC 4180 CSV. Values are exact decimal strings from the integers; a cell that a spreadsheet would run as a formula is quoted with a leading apostrophe. */
export function toCsv(header: string[], rows: (string | number)[][]): string {
  const cell = (v: string | number) => {
    let s = String(v);
    if (/^[=+\-@\t\r]/.test(s) && !/^-?\d+(\.\d+)?$/.test(s)) s = `'${s}`;
    return /[",\r\n]/.test(s) ? `"${s.replace(/"/g, '""')}"` : s;
  };
  return [header, ...rows].map((r) => r.map(cell).join(',')).join('\r\n') + '\r\n';
}

function download(name: string, text: string) {
  const url = URL.createObjectURL(new Blob([text], { type: 'text/csv;charset=utf-8' }));
  const a = document.createElement('a');
  a.href = url;
  a.download = name;
  document.body.appendChild(a);
  a.click();
  a.remove();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}
const iso = (ms: number) => new Date(ms).toISOString();

export function History({ tab }: { tab: HistoryTab }) {
  const { account } = useActions();
  const state = useStore(feed.account);
  const markets = useStore(feed.markets);
  const [symbol, setSymbol] = useState('');
  const market = (s: string): Market | undefined => markets.find((m) => m.symbol === s);
  const pick = <X extends { symbol: string }>(list: X[] | undefined) => (list ?? []).filter((x) => !symbol || x.symbol === symbol);
  const orders = useMemo(() => pick(state?.history), [state?.history, symbol]); // eslint-disable-line react-hooks/exhaustive-deps
  const fills = useMemo(() => pick(state?.fills), [state?.fills, symbol]); // eslint-disable-line react-hooks/exhaustive-deps
  const funding = useMemo(() => pick(state?.funding), [state?.funding, symbol]); // eslint-disable-line react-hooks/exhaustive-deps
  const fillTotals = useMemo(() => fills.reduce((t, f) => ({ fees: t.fees + f.fee, realised: t.realised + f.realised }), { fees: 0n, realised: 0n }), [fills]);
  const fundingTotal = useMemo(() => funding.reduce((t, f) => t + f.payment, 0n), [funding]);
  if (!account)
    return (
      <div className="page-message">
        <GateGlyph size={40} />
        <h1>History</h1>
        <p>Connect a wallet to see your orders, fills, funding payments and transfers.</p>
      </div>
    );
  const exportCsv = () => {
    const stamp = new Date().toISOString().slice(0, 10);
    if (tab === 'orders')
      download(`postern-perps-orders-${stamp}.csv`, toCsv(['time_utc', 'order_id', 'market', 'type', 'time_in_force', 'side', 'price', 'trigger_price', 'filled', 'size', 'reduce_only', 'status'], orders.map((o) => [iso(o.createdAt), o.id, o.symbol, o.type, o.tif, o.buy ? 'buy' : 'sell', o.px === null ? '' : toDecimalString(o.px, PX_DECIMALS), o.triggerPx === null ? '' : toDecimalString(o.triggerPx, PX_DECIMALS), toDecimalString(o.filled, SZ_DECIMALS), toDecimalString(o.size, SZ_DECIMALS), String(o.reduceOnly), o.status])));
    if (tab === 'fills')
      download(`postern-perps-fills-${stamp}.csv`, toCsv(['time_utc', 'fill_id', 'order_id', 'market', 'side', 'price', 'size', 'fee_usdc', 'liquidity', 'realised_pnl_usdc', 'liquidation'], fills.map((f) => [iso(f.time), f.id, f.orderId, f.symbol, f.buy ? 'buy' : 'sell', toDecimalString(f.px, PX_DECIMALS), toDecimalString(f.size, SZ_DECIMALS), toDecimalString(f.fee, USDC_DECIMALS), f.maker ? 'maker' : 'taker', toDecimalString(f.realised, USDC_DECIMALS), String(f.liquidation)])));
    if (tab === 'funding')
      download(`postern-perps-funding-${stamp}.csv`, toCsv(['time_utc', 'market', 'rate', 'position_size', 'mark_price', 'payment_usdc'], funding.map((f) => [iso(f.time), f.symbol, toDecimalString(f.rate, RATE_DECIMALS), toDecimalString(f.size, SZ_DECIMALS), toDecimalString(f.markPx, PX_DECIMALS), toDecimalString(f.payment, USDC_DECIMALS)])));
    if (tab === 'transfers')
      download(
        `postern-perps-transfers-${stamp}.csv`,
        toCsv(['time_utc', 'kind', 'id', 'amount_usdc', 'fee_usdc', 'status', 'recipient', 'l2_tx'], [
          ...(state?.deposits ?? []).map((d) => [iso(d.time), 'deposit', d.id, toDecimalString(d.amount, USDC_DECIMALS), '0', d.status, '', d.txHash]),
          ...(state?.withdrawals ?? []).map((w) => [iso(w.time), 'withdrawal', w.id, toDecimalString(w.amount, USDC_DECIMALS), toDecimalString(w.fee, USDC_DECIMALS), w.status, w.recipient, w.txHash ?? '']),
        ]),
      );
  };
  const transfers = [
    ...(state?.deposits ?? []).map((d) => ({ kind: 'Deposit' as const, id: d.id, time: d.time, amount: d.amount, fee: 0n, status: d.status === 'l2Finalized' ? 'L2 final' : d.status, to: null as string | null })),
    ...(state?.withdrawals ?? []).map((w) => ({ kind: 'Withdrawal' as const, id: w.id, time: w.time, amount: w.amount, fee: w.fee, status: w.status as string, to: w.recipient as string | null })),
  ].sort((a, b) => b.time - a.time);
  return (
    <div className="page page-wide">
      <header className="page-head">
        <p className="eyebrow">History</p>
        <h1 className="display">Account history</h1>
        <p className="lead">Everything the exchange recorded for this account. The exchange is transparent: any account's history is public by address.</p>
      </header>
      <section className="panel" aria-label="History">
        <header className="panel-head tabs-head">
          <div role="tablist" className="tabs">
            {TABS.map(([t, label]) => (
              <button key={t} type="button" role="tab" aria-selected={tab === t} onClick={() => go({ page: 'history', tab: t })}>
                {label}
              </button>
            ))}
          </div>
          <div className="tabs-tools">
            {tab !== 'transfers' && (
              <label className="field field-inline">
                <span className="sr-only">Market</span>
                <select value={symbol} onChange={(e) => setSymbol(e.target.value)}>
                  <option value="">All markets</option>
                  {markets.map((m) => (
                    <option key={m.symbol} value={m.symbol}>
                      {m.symbol}
                    </option>
                  ))}
                </select>
              </label>
            )}
            <button type="button" className="button button-small button-quiet" onClick={exportCsv} disabled={!state}>
              <Download size={14} aria-hidden="true" /> CSV
            </button>
          </div>
        </header>
        {tab === 'fills' && (
          <dl className="table-totals">
            <div>
              <dt>Fills shown</dt>
              <dd className="num">{fills.length}</dd>
            </div>
            <div>
              <dt>Fees paid</dt>
              <dd className="num">{formatUsd(fillTotals.fees, { dp: 4 })} USDC</dd>
            </div>
            <div>
              <dt>Realised PnL</dt>
              <dd className={`num ${fillTotals.realised > 0n ? 'up' : fillTotals.realised < 0n ? 'down' : ''}`}>{formatUsd(fillTotals.realised, { sign: 'always' })} USDC</dd>
            </div>
          </dl>
        )}
        {tab === 'funding' && (
          <dl className="table-totals">
            <div>
              <dt>Payments shown</dt>
              <dd className="num">{funding.length}</dd>
            </div>
            <div>
              <dt>Net funding</dt>
              <dd className={`num ${fundingTotal > 0n ? 'up' : fundingTotal < 0n ? 'down' : ''}`}>{formatUsd(fundingTotal, { sign: 'always', dp: 4 })} USDC</dd>
            </div>
          </dl>
        )}
        <div className="table-wrap table-tall">
          {tab === 'orders' && (
            <table>
              <thead>
                <tr>
                  <th>Time (UTC)</th>
                  <th>Market</th>
                  <th>Type</th>
                  <th>Side</th>
                  <th className="r">Price</th>
                  <th className="r">Avg. fill</th>
                  <th className="r">Filled / size</th>
                  <th>Status</th>
                </tr>
              </thead>
              <tbody>
                {orders.slice(0, 500).map((o) => {
                  const m = market(o.symbol);
                  const dp = m?.priceDecimals ?? 2;
                  const sd = m?.sizeDecimals ?? 4;
                  return (
                    <tr key={o.id}>
                      <td className="num">{formatDateTime(o.createdAt)}</td>
                      <td>{o.symbol}</td>
                      <td>
                        {orderTypeWords(o.type)}
                        {o.reduceOnly && <span className="badge">reduce only</span>}
                      </td>
                      <td className={o.buy ? 'up' : 'down'}>{o.buy ? 'Buy' : 'Sell'}</td>
                      <td className="r num">{o.px === null ? DASH : formatPx(o.px, dp)}</td>
                      <td className="r num">{o.avgPx === null ? DASH : formatPx(o.avgPx, dp)}</td>
                      <td className="r num">
                        {formatSize(o.filled, sd)} / {formatSize(o.size, sd)}
                      </td>
                      <td>
                        <span className={`badge status-${o.status}`}>{o.status}</span>
                        {o.rejectReason && <span className="sub"> {engineErrorWords(o.rejectReason)}</span>}
                      </td>
                    </tr>
                  );
                })}
                {orders.length === 0 && <Empty cols={8} text={state ? 'No closed orders.' : 'Reading…'} />}
              </tbody>
            </table>
          )}
          {tab === 'fills' && (
            <table>
              <thead>
                <tr>
                  <th>Time (UTC)</th>
                  <th>Market</th>
                  <th>Side</th>
                  <th className="r">Price</th>
                  <th className="r">Size</th>
                  <th className="r">Fee (USDC)</th>
                  <th>Liquidity</th>
                  <th className="r">Realised PnL</th>
                </tr>
              </thead>
              <tbody>
                {fills.slice(0, 500).map((f) => {
                  const m = market(f.symbol);
                  return (
                    <tr key={f.id}>
                      <td className="num">{formatDateTime(f.time)}</td>
                      <td>{f.symbol}</td>
                      <td className={f.buy ? 'up' : 'down'}>
                        {f.buy ? 'Buy' : 'Sell'}
                        {f.liquidation && <span className="badge badge-danger">liquidation</span>}
                      </td>
                      <td className="r num">{formatPx(f.px, m?.priceDecimals ?? 2)}</td>
                      <td className="r num">{formatSize(f.size, m?.sizeDecimals ?? 4)}</td>
                      <td className="r num">{formatUsd(f.fee, { dp: 4 })}</td>
                      <td>{f.maker ? 'Maker' : 'Taker'}</td>
                      <td className={`r num ${f.realised > 0n ? 'up' : f.realised < 0n ? 'down' : ''}`}>{f.realised === 0n ? DASH : formatUsd(f.realised, { sign: 'always' })}</td>
                    </tr>
                  );
                })}
                {fills.length === 0 && <Empty cols={8} text={state ? 'No fills.' : 'Reading…'} />}
              </tbody>
            </table>
          )}
          {tab === 'funding' && (
            <table>
              <thead>
                <tr>
                  <th>Time (UTC)</th>
                  <th>Market</th>
                  <th className="r">Rate</th>
                  <th className="r">Position</th>
                  <th className="r">Mark price</th>
                  <th className="r">Payment (USDC)</th>
                </tr>
              </thead>
              <tbody>
                {funding.slice(0, 500).map((f) => {
                  const m = market(f.symbol);
                  return (
                    <tr key={`${f.symbol}-${f.time}`}>
                      <td className="num">{formatDateTime(f.time)}</td>
                      <td>{f.symbol}</td>
                      <td className="r num">{formatRate(f.rate)}</td>
                      <td className="r num">{formatSize(f.size, m?.sizeDecimals ?? 4, 'always')}</td>
                      <td className="r num">{formatPx(f.markPx, m?.priceDecimals ?? 2)}</td>
                      <td className={`r num ${f.payment >= 0n ? 'up' : 'down'}`}>{formatUsd(f.payment, { sign: 'always', dp: 4 })}</td>
                    </tr>
                  );
                })}
                {funding.length === 0 && <Empty cols={6} text={state ? 'No funding payments.' : 'Reading…'} />}
              </tbody>
            </table>
          )}
          {tab === 'transfers' && (
            <table>
              <thead>
                <tr>
                  <th>Time (UTC)</th>
                  <th>Kind</th>
                  <th className="r">Amount (USDC)</th>
                  <th className="r">Fee</th>
                  <th>Status</th>
                  <th>Recipient</th>
                </tr>
              </thead>
              <tbody>
                {transfers.map((t) => (
                  <tr key={`${t.kind}-${t.id}`}>
                    <td className="num">{formatDateTime(t.time)}</td>
                    <td>
                      {t.kind} <span className="sub num">#{t.id}</span>
                    </td>
                    <td className={`r num ${t.kind === 'Deposit' ? 'up' : ''}`}>{formatUsd(t.amount, { sign: t.kind === 'Deposit' ? 'always' : 'auto' })}</td>
                    <td className="r num">{t.fee === 0n ? DASH : formatUsd(t.fee)}</td>
                    <td>
                      <span className={`badge status-${t.status === 'L2 final' ? 'l2Finalized' : t.status}`}>{t.status}</span>
                    </td>
                    <td className="num">{t.to ? shortAddress(t.to) : DASH}</td>
                  </tr>
                ))}
                {transfers.length === 0 && <Empty cols={6} text={state ? 'No transfers.' : 'Reading…'} />}
              </tbody>
            </table>
          )}
        </div>
        <p className="fine table-foot">Up to the newest 500 records of each kind are loaded. Times are UTC. The CSV holds exact values, not the rounded ones shown.</p>
      </section>
    </div>
  );
}

const Empty = ({ cols, text }: { cols: number; text: string }) => (
  <tr>
    <td colSpan={cols} className="empty">
      {text}
    </td>
  </tr>
);
