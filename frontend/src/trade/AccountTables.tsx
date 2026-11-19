/** Positions, open orders, order history, fills and funding of the connected account. */
import { useState } from 'react';
import { feed } from '../state/session';
import { useActions } from '../state/actions';
import { useStore, persisted } from '../lib/store';
import { formatDateTime, formatPpm, formatPx, formatRate, formatSize, formatUsd, DASH } from '../lib/format';
import { roePpm } from '../lib/calc';
import { orderTypeWords } from '../lib/eip712';
import { engineErrorWords } from '../lib/api';
import { buildOrder, DEFAULT_SLIPPAGE_PPM, newClientOrderId, sizeText } from './order-builder';
import { MarginDialog } from './LeverageDialog';
import type { Market, Order, Position } from '../lib/types';
import { go } from '../state/session';

type Tab = 'positions' | 'orders' | 'history' | 'fills' | 'funding';
const TABS: [Tab, string][] = [
  ['positions', 'Positions'],
  ['orders', 'Open orders'],
  ['history', 'Order history'],
  ['fills', 'Fills'],
  ['funding', 'Funding'],
];
const tabStore = persisted<Tab>('postern.perps.tab.v1', 'positions', (v): v is Tab => TABS.some(([t]) => t === v));

export function AccountTables({ only }: { only?: string }) {
  const state = useStore(feed.account);
  const markets = useStore(feed.markets);
  const tickers = useStore(feed.tickers);
  const tab = useStore(tabStore);
  const { account, request } = useActions();
  const [marginFor, setMarginFor] = useState<Position | null>(null);
  const [onlyThis, setOnlyThis] = useState(false);
  const market = (s: string) => markets.find((m) => m.symbol === s);
  const filter = <X extends { symbol: string }>(list: X[]) => (onlyThis && only ? list.filter((x) => x.symbol === only) : list);
  const counts: Partial<Record<Tab, number>> = { positions: state?.account.positions.length, orders: state?.orders.length };

  const close = (p: Position) => {
    const m = market(p.symbol);
    const t = tickers.get(p.symbol);
    if (!m) return;
    const clientOrderId = newClientOrderId();
    request('PlaceOrder', (nonce, acct) => {
      const b = buildOrder({ form: { side: p.size > 0n ? 'sell' : 'buy', kind: 'market', size: sizeText(p.size, m), price: '', trigger: '', tif: 'IOC', reduceOnly: true, slippagePpm: DEFAULT_SLIPPAGE_PPM }, market: m, mark: t?.mark ?? null, levels: [], account: acct, nonce, clientOrderId });
      if (!b.message) throw new Error(b.errors[0]);
      return b.message;
    });
  };
  const cancel = (o: Order) => {
    const m = market(o.symbol);
    if (m) request('CancelOrder', (nonce, acct) => ({ account: acct, nonce: nonce.toString(), marketId: m.marketId, orderId: o.id }));
  };

  return (
    <section className="panel account-tables" aria-label="Account">
      <header className="panel-head tabs-head">
        <div role="tablist" className="tabs">
          {TABS.map(([t, label]) => (
            <button key={t} type="button" role="tab" aria-selected={tab === t} onClick={() => tabStore.set(t)}>
              {label}
              {counts[t] ? <span className="count num">{counts[t]}</span> : null}
            </button>
          ))}
        </div>
        <div className="tabs-tools">
          {only && (
            <label className="check">
              <input type="checkbox" checked={onlyThis} onChange={(e) => setOnlyThis(e.target.checked)} />
              <span>{only} only</span>
            </label>
          )}
          {tab === 'orders' && state && state.orders.length > 0 && (
            <button type="button" className="button button-small button-quiet" onClick={() => request('CancelAll', (nonce, acct) => ({ account: acct, nonce: nonce.toString(), marketId: 0 }))}>
              Cancel all
            </button>
          )}
        </div>
      </header>
      {!account ? (
        <p className="empty">Connect a wallet to see positions and orders. Accounts are public on this exchange, like the book.</p>
      ) : !state ? (
        <p className="empty">Reading the account…</p>
      ) : (
        <div className="table-wrap" role="tabpanel">
          {tab === 'positions' && (
            <table>
              <thead>
                <tr>
                  <th>Market</th>
                  <th className="r">Size</th>
                  <th className="r">Value</th>
                  <th className="r">Entry</th>
                  <th className="r">Mark</th>
                  <th className="r">Liquidation</th>
                  <th className="r">Margin</th>
                  <th className="r">Unrealised PnL (ROE)</th>
                  <th className="r">Funding</th>
                  <th>
                    <span className="sr-only">Action</span>
                  </th>
                </tr>
              </thead>
              <tbody>
                {filter(state.account.positions).map((p) => {
                  const m = market(p.symbol);
                  const dp = m?.priceDecimals ?? 2;
                  const mark = tickers.get(p.symbol)?.mark ?? p.markPx;
                  const margin = p.isolated ? p.isolatedMargin : p.initialMargin;
                  const roe = roePpm(p.unrealised, margin);
                  return (
                    <tr key={p.symbol}>
                      <td>
                        <button type="button" className="link-button" onClick={() => go({ page: 'trade', symbol: p.symbol })}>
                          {p.symbol}
                        </button>
                        <span className={`badge ${p.size > 0n ? 'badge-long' : 'badge-short'}`}>{p.size > 0n ? 'Long' : 'Short'}</span>
                        <span className="badge">{p.leverage}x {p.isolated ? 'isolated' : 'cross'}</span>
                      </td>
                      <td className="r num">{formatSize(p.size < 0n ? -p.size : p.size, m?.sizeDecimals ?? 4)} {m?.base}</td>
                      <td className="r num">{formatUsd(p.notional)}</td>
                      <td className="r num">{formatPx(p.entryPx, dp)}</td>
                      <td className="r num">{formatPx(mark, dp)}</td>
                      <td className="r num liq">{p.liquidationPx === null ? 'None' : formatPx(p.liquidationPx, dp)}</td>
                      <td className="r num">
                        {formatUsd(margin)}
                        {p.isolated && m && (
                          <button type="button" className="link-button" onClick={() => setMarginFor(p)} aria-label={`Change isolated margin of ${p.symbol}`}>
                            Edit
                          </button>
                        )}
                      </td>
                      <td className={`r num ${p.unrealised >= 0n ? 'up' : 'down'}`}>
                        {formatUsd(p.unrealised, { sign: 'always' })} {roe !== null && <span className="sub">({formatPpm(roe, 2, 'always')})</span>}
                      </td>
                      <td className={`r num ${p.funding >= 0n ? 'up' : 'down'}`}>{formatUsd(p.funding, { sign: 'always' })}</td>
                      <td className="r">
                        <button type="button" className="button button-small" onClick={() => close(p)}>
                          Close at market
                        </button>
                      </td>
                    </tr>
                  );
                })}
                {state.account.positions.length === 0 && <EmptyRow cols={10} text="No open positions." />}
              </tbody>
            </table>
          )}
          {tab === 'orders' && (
            <OrderTable orders={filter(state.orders)} market={market} onCancel={cancel} />
          )}
          {tab === 'history' && <OrderTable orders={filter(state.history)} market={market} />}
          {tab === 'fills' && (
            <table>
              <thead>
                <tr>
                  <th>Time (UTC)</th>
                  <th>Market</th>
                  <th>Side</th>
                  <th className="r">Price</th>
                  <th className="r">Size</th>
                  <th className="r">Fee</th>
                  <th>Liquidity</th>
                  <th className="r">Realised PnL</th>
                </tr>
              </thead>
              <tbody>
                {filter(state.fills).slice(0, 200).map((f) => {
                  const m = market(f.symbol);
                  return (
                    <tr key={f.id}>
                      <td className="num">{formatDateTime(f.time)}</td>
                      <td>{f.symbol}</td>
                      <td className={f.buy ? 'up' : 'down'}>
                        {f.buy ? 'Buy' : 'Sell'}
                        {f.liquidation && <span className="badge badge-danger">Liquidation</span>}
                      </td>
                      <td className="r num">{formatPx(f.px, m?.priceDecimals ?? 2)}</td>
                      <td className="r num">{formatSize(f.size, m?.sizeDecimals ?? 4)}</td>
                      <td className="r num">{formatUsd(f.fee, { dp: 4 })}</td>
                      <td>{f.maker ? 'Maker' : 'Taker'}</td>
                      <td className={`r num ${f.realised > 0n ? 'up' : f.realised < 0n ? 'down' : ''}`}>{f.realised === 0n ? DASH : formatUsd(f.realised, { sign: 'always' })}</td>
                    </tr>
                  );
                })}
                {state.fills.length === 0 && <EmptyRow cols={8} text="No fills yet." />}
              </tbody>
            </table>
          )}
          {tab === 'funding' && (
            <table>
              <thead>
                <tr>
                  <th>Time (UTC)</th>
                  <th>Market</th>
                  <th className="r">Position</th>
                  <th className="r">Rate</th>
                  <th className="r">Payment (USDC)</th>
                </tr>
              </thead>
              <tbody>
                {filter(state.funding).slice(0, 200).map((f, i) => {
                  const m = market(f.symbol);
                  return (
                    <tr key={`${f.symbol}-${f.time}-${i}`}>
                      <td className="num">{formatDateTime(f.time)}</td>
                      <td>{f.symbol}</td>
                      <td className="r num">{formatSize(f.size, m?.sizeDecimals ?? 4, 'always')}</td>
                      <td className="r num">{formatRate(f.rate)}</td>
                      <td className={`r num ${f.payment >= 0n ? 'up' : 'down'}`}>{formatUsd(f.payment, { sign: 'always', dp: 4 })}</td>
                    </tr>
                  );
                })}
                {state.funding.length === 0 && <EmptyRow cols={5} text="No funding payments yet." />}
              </tbody>
            </table>
          )}
        </div>
      )}
      {marginFor && market(marginFor.symbol) && <MarginDialog market={market(marginFor.symbol)!} position={marginFor} onClose={() => setMarginFor(null)} />}
    </section>
  );
}

function OrderTable({ orders, market, onCancel }: { orders: Order[]; market: (s: string) => Market | undefined; onCancel?: (o: Order) => void }) {
  return (
    <table>
      <thead>
        <tr>
          <th>Time (UTC)</th>
          <th>Market</th>
          <th>Type</th>
          <th>Side</th>
          <th className="r">Price</th>
          <th className="r">Trigger</th>
          <th className="r">Filled / size</th>
          <th>Status</th>
          {onCancel && (
            <th>
              <span className="sr-only">Action</span>
            </th>
          )}
        </tr>
      </thead>
      <tbody>
        {orders.slice(0, 300).map((o) => {
          const m = market(o.symbol);
          const dp = m?.priceDecimals ?? 2;
          const sd = m?.sizeDecimals ?? 4;
          const isMarket = o.type === 'market' || o.type === 'stopMarket' || o.type === 'takeProfitMarket';
          return (
            <tr key={o.id}>
              <td className="num">{formatDateTime(o.createdAt)}</td>
              <td>{o.symbol}</td>
              <td>
                {orderTypeWords(o.type)}
                {o.tif !== 'GTC' && <span className="sub"> · {o.tif === 'POST_ONLY' ? 'post only' : 'IOC'}</span>}
                {o.reduceOnly && <span className="badge">reduce only</span>}
              </td>
              <td className={o.buy ? 'up' : 'down'}>{o.buy ? 'Buy' : 'Sell'}</td>
              <td className="r num">{o.px === null ? (isMarket ? 'Market' : DASH) : `${isMarket ? 'worst ' : ''}${formatPx(o.px, dp)}`}</td>
              <td className="r num">{o.triggerPx === null ? DASH : `${o.triggerCondition === 'markAbove' ? '≥' : '≤'} ${formatPx(o.triggerPx, dp)}`}</td>
              <td className="r num">
                {formatSize(o.filled, sd)} / {formatSize(o.size, sd)}
              </td>
              <td>
                <span className={`badge status-${o.status}`}>{o.status === 'untriggered' ? 'waiting for trigger' : o.status}</span>
                {o.rejectReason && <span className="sub"> {engineErrorWords(o.rejectReason)}</span>}
              </td>
              {onCancel && (
                <td className="r">
                  <button type="button" className="button button-small button-quiet" onClick={() => onCancel(o)}>
                    Cancel
                  </button>
                </td>
              )}
            </tr>
          );
        })}
        {orders.length === 0 && <EmptyRow cols={onCancel ? 9 : 8} text={onCancel ? 'No open orders.' : 'No closed orders yet.'} />}
      </tbody>
    </table>
  );
}

const EmptyRow = ({ cols, text }: { cols: number; text: string }) => (
  <tr>
    <td colSpan={cols} className="empty">
      {text}
    </td>
  </tr>
);

