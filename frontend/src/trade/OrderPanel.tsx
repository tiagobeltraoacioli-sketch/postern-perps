/**
 * The order form: the gate of the trading screen. The lamp over it is the form's state (idle, kindling while the
 * input is incomplete, lit with a valid order, pending while a signature is open, settled after an included order,
 * amber on a warning, red on an error or a refusal). Every figure is computed by calc.ts from integers and labelled
 * an estimate; the review sheet shows the signed values.
 */
import { useEffect, useMemo, useRef, useState, type ReactNode } from 'react';
import { feed, geo as geoStore } from '../state/session';
import { useActions } from '../state/actions';
import { useStore, persisted } from '../lib/store';
import { BASE_FEES, orderPreview, sizeForBudget, accountSummary, health } from '../lib/calc';
import { formatPpm, formatPx, formatSize, formatUsd, DASH } from '../lib/format';
import { SZ_DECIMALS, toDecimalString } from '../lib/fixed';
import { buildOrder, DEFAULT_SLIPPAGE_PPM, isMarketKind, isTriggerKind, newClientOrderId, parseAt, priceText, type OrderForm } from './order-builder';
import { orderSide, positionInputs, pricePick, useAccountState, useScrollMore } from './hooks';
import type { Market, OrderType, Ticker, TimeInForce } from '../lib/types';
import { storedAttestation } from '../compliance/eligibility';
import { Attestation } from '../compliance/Attestation';
import { X } from 'lucide-react';
import { LeverageDialog } from './LeverageDialog';
import { orderTypeWords } from '../lib/eip712';

type Mode = 'limit' | 'market' | 'trigger';
const slippage = persisted<string>('postern.perps.slippage.v1', '1', (v): v is string => typeof v === 'string' && /^\d{1,2}(\.\d{1,2})?$/.test(v));

export function OrderPanel({ market, ticker, onClose }: { market: Market; ticker: Ticker | undefined; onClose?: () => void }) {
  const { account, request, pending } = useActions();
  const state = useAccountState();
  const book = useStore(feed.book);
  const markets = useStore(feed.markets);
  const tickers = useStore(feed.tickers);
  const geo = useStore(geoStore);
  const picked = useStore(pricePick);
  const slip = useStore(slippage);
  const side = useStore(orderSide);
  const setSide = (s: 'buy' | 'sell') => orderSide.set(s);
  const [mode, setMode] = useState<Mode>('limit');
  const [triggerKind, setTriggerKind] = useState<OrderType>('stopMarket');
  const [size, setSize] = useState('');
  const [price, setPrice] = useState('');
  const [trigger, setTrigger] = useState('');
  const [tif, setTif] = useState<TimeInForce>('GTC');
  const [reduceOnly, setReduceOnly] = useState(false);
  const [leverageOpen, setLeverageOpen] = useState(false);
  const [settledAt, setSettledAt] = useState(0);
  const [ackTick, setAckTick] = useState(0);

  // A new market clears the typed values (a BTC price typed into ETH would be absurd).
  useEffect(() => {
    setSize('');
    setPrice('');
    setTrigger('');
  }, [market.symbol]);
  useEffect(() => {
    if (picked && Date.now() - picked.at < 2000) {
      if (mode === 'market') setMode('limit');
      setPrice(picked.text);
    }
  }, [picked]); // eslint-disable-line react-hooks/exhaustive-deps

  const restricted = !geo || geo.restricted || geo.policy !== 'open' || geo.sanctioned;
  const forcedReduce = restricted || market.status === 'closeOnly';
  const reduce = forcedReduce || reduceOnly;
  const kind: OrderType = mode === 'limit' ? 'limit' : mode === 'market' ? 'market' : triggerKind;
  const slipPpm = (() => {
    const v = parseAt(slip, 4, 4).value; // percent with up to 4 places, as 1e4 units of a percent
    return v === null ? DEFAULT_SLIPPAGE_PPM : v; // percent x 1e4 = ppm
  })();
  // Exit only (region or close-only market): the side that reduces the open position is the useful default.
  const positionSign = state?.account.positions.find((p) => p.symbol === market.symbol)?.size ?? 0n;
  useEffect(() => {
    if (forcedReduce && positionSign !== 0n) setSide(positionSign > 0n ? 'sell' : 'buy');
  }, [forcedReduce, positionSign > 0n, positionSign === 0n, market.symbol]); // eslint-disable-line react-hooks/exhaustive-deps
  const form: OrderForm = { side, kind, size, price, trigger, tif, reduceOnly: reduce, slippagePpm: slipPpm };
  const position = state?.account.positions.find((p) => p.symbol === market.symbol);
  const leverage = position?.leverage ?? Math.min(10, market.maxLeverage);
  const isolated = position?.isolated ?? false;
  const levels = side === 'buy' ? book.asks : book.bids;
  const built = buildOrder({ form, market, mark: ticker?.mark ?? null, levels, account, nonce: 0n, clientOrderId: `0x${'00'.repeat(32)}` });

  const inputs = positionInputs(state, markets, tickers);
  const index = inputs.findIndex((p) => p.market === market.symbol);
  const fees = state ? { takerPpm: state.account.feeTier.takerPpm, makerPpm: state.account.feeTier.makerPpm } : { takerPpm: market.takerPpm || BASE_FEES.takerPpm, makerPpm: market.makerPpm || BASE_FEES.makerPpm };
  const fillPx = built.isMarket ? (built.estimate?.avgPx ?? built.price) : built.price;
  const preview =
    built.size && fillPx && ticker && state
      ? orderPreview({ side: side === 'buy' ? 'long' : 'short', size: built.size, px: fillPx, markPx: ticker.mark, leverage, isolated, reduceOnly: reduce, taker: built.isMarket || tif === 'IOC', fees, tiers: market.tiers, tick: market.tick, collateral: state.account.collateral, positions: inputs, index, orderMargin: 0n })
      : null;
  const summary = state ? accountSummary(state.account.collateral, inputs) : null;

  const acknowledged = account ? !!storedAttestation(account) : false;
  void ackTick;
  const busy = pending?.phase === 'signing';
  const problems = preview?.problems ?? [];
  const lamp = busy ? 'pending' : built.errors.length && size ? 'danger' : problems.length ? 'danger' : built.warnings.length ? 'warn' : built.message ? 'lit' : Date.now() - settledAt < 4000 ? 'settled' : size ? 'kindling' : 'idle';

  const setFraction = (pct: bigint) => {
    if (!summary || !ticker) return;
    if (reduce && position) {
      const abs = position.size < 0n ? -position.size : position.size;
      const s = (abs * pct) / 100n;
      setSize(toDecimalString((s - (s % market.step)) / 10n ** BigInt(SZ_DECIMALS - market.sizeDecimals), market.sizeDecimals));
      return;
    }
    const px = built.price ?? ticker.mark;
    const s = sizeForBudget({ budget: (summary.available * pct) / 100n, px, leverage, feePpm: fees.takerPpm, step: market.step, tiers: market.tiers });
    setSize(toDecimalString(s / 10n ** BigInt(SZ_DECIMALS - market.sizeDecimals), market.sizeDecimals));
  };

  const submit = () => {
    if (!built.message) return;
    const clientOrderId = newClientOrderId();
    request(
      'PlaceOrder',
      (nonce, acct) => {
        const b = buildOrder({ form, market, mark: ticker?.mark ?? null, levels, account: acct, nonce, clientOrderId });
        if (!b.message) throw new Error(b.errors[0] ?? 'invalid');
        return b.message;
      },
      (o) => {
        if (o.ok) {
          setSettledAt(Date.now());
          setSize('');
        }
      },
      preview
        ? [
            { label: 'Estimated order value', value: `${formatUsd(preview.notional)} USDC` },
            { label: 'Estimated margin', value: `${formatUsd(preview.margin)} USDC` },
            { label: `Estimated fee (${built.isMarket || tif === 'IOC' ? 'taker' : 'maker if it rests'})`, value: `${formatUsd(preview.fee)} USDC` },
            { label: 'Estimated liquidation price', value: preview.liquidationPx === null ? 'None' : `${formatPx(preview.liquidationPx, market.priceDecimals)} USD` },
          ]
        : undefined,
    );
  };

  const verb = side === 'buy' ? 'Long' : 'Short';
  const bodyRef = useRef<HTMLDivElement>(null);
  const more = useScrollMore(bodyRef);
  return (
    <section className={`panel order-panel lamp-${lamp}`} aria-label="Place order">
      <div className="gate-head" aria-hidden="true">
        <span className="gate-lamp" />
      </div>
      {onClose && (
        <button type="button" className="icon-button sheet-close" aria-label="Close the order form" onClick={onClose}>
          <X size={20} aria-hidden="true" />
        </button>
      )}
      <div className="gate-top">
      <div className="side-switch" role="radiogroup" aria-label="Side">
        <button type="button" role="radio" aria-checked={side === 'buy'} className="side-long" onClick={() => setSide('buy')}>
          Long
        </button>
        <button type="button" role="radio" aria-checked={side === 'sell'} className="side-short" onClick={() => setSide('sell')}>
          Short
        </button>
      </div>
      <div className="mode-tabs">
        {/* The tab list holds only tabs; the leverage button stands beside it. */}
        <div className="mode-tablist" role="tablist" aria-label="Order type">
          {(['limit', 'market', 'trigger'] as Mode[]).map((m) => (
            <button key={m} type="button" role="tab" aria-selected={mode === m} onClick={() => setMode(m)}>
              {m === 'trigger' ? 'Stop / TP' : m === 'limit' ? 'Limit' : 'Market'}
            </button>
          ))}
        </div>
        <button type="button" className="lev-chip" onClick={() => setLeverageOpen(true)} disabled={!account} title="Leverage and margin mode for this market">
          <span className="num">{leverage}x</span> {isolated ? 'Isolated' : 'Cross'}
        </button>
      </div>
      </div>
      {/* The body holds the form and fits the fitted screen from 720 px of height; where it still has to scroll (a
          long statement, a list of refusals) its last lines fade into the action bar instead of being cut. The
          account's own figures and the action stand in the foot: nothing of them is ever under anything. */}
      <div className="gate-body" ref={bodyRef} data-more={more ? 'true' : undefined}>
      {account && !acknowledged ? (
        /* Before the first order the gate shows the statement and nothing else: no order can be built yet. */
        <Attestation account={account} onDone={() => setAckTick((t) => t + 1)} />
      ) : (
      <>
      {mode === 'trigger' && (
        <label className="field field-inline">
          <span>Trigger order</span>
          <select value={triggerKind} onChange={(e) => setTriggerKind(e.target.value as OrderType)}>
            {(['stopMarket', 'stopLimit', 'takeProfitMarket', 'takeProfitLimit'] as OrderType[]).map((k) => (
              <option key={k} value={k}>
                {orderTypeWords(k)}
              </option>
            ))}
          </select>
        </label>
      )}
      <div className={isTriggerKind(kind) && !isMarketKind(kind) ? 'amount-pair' : 'amount-single'}>
      {isTriggerKind(kind) && (
        <Amount label="Trigger price (mark)" unit="USD" value={trigger} onChange={setTrigger} placeholder={ticker ? formatPx(ticker.mark, market.priceDecimals).replace(/,/g, '') : ''} hint={`Tick ${formatPx(market.tick, market.priceDecimals)}`} />
      )}
      {!isMarketKind(kind) && (
        <Amount
          label="Limit price"
          unit="USD"
          value={price}
          onChange={setPrice}
          placeholder=""
          action={
            ticker && (
              <button type="button" className="link-button" onClick={() => setPrice(priceText(ticker.mark, market, side === 'buy' ? 'floor' : 'ceil'))}>
                Mark
              </button>
            )
          }
        />
      )}
      </div>
      <Amount label="Size" unit={market.base} value={size} onChange={setSize} placeholder="0" hint={`Step ${toDecimalString(market.step / 10n ** BigInt(SZ_DECIMALS - market.sizeDecimals), market.sizeDecimals)}`} />
      <div className="fractions" role="group" aria-label={reduce ? 'Part of the position' : 'Part of available margin'}>
        {[25n, 50n, 75n, 100n].map((p) => (
          <button key={String(p)} type="button" onClick={() => setFraction(p)} disabled={!summary}>
            {String(p)}%
          </button>
        ))}
      </div>

      <div className="options">
        {mode === 'limit' && (
          <label className="field field-inline tif">
            <span>Time in force</span>
            <select value={tif} onChange={(e) => setTif(e.target.value as TimeInForce)}>
              <option value="GTC">Good till cancelled</option>
              <option value="IOC">Immediate or cancel</option>
              <option value="POST_ONLY">Post only (maker)</option>
            </select>
          </label>
        )}
        {isMarketKind(kind) && (
          <label className="field field-inline">
            <span>Worst price: mark ± </span>
            <span className="slip">
              <input inputMode="decimal" value={slip} onChange={(e) => slippage.set(e.target.value.replace(/[^\d.]/g, '').slice(0, 5) || '1')} aria-label="Price bound in percent" />%
            </span>
          </label>
        )}
        <label className="check">
          <input type="checkbox" checked={reduce} disabled={forcedReduce} onChange={(e) => setReduceOnly(e.target.checked)} />
          <span>Reduce only{forcedReduce ? (restricted ? ' (your region: exit only)' : ' (market is close-only)') : ''}</span>
        </label>
      </div>

      <dl className="estimate">
        {built.isMarket && built.price !== null && (
          <div className="est-strong">
            <dt>{side === 'buy' ? 'Pays at most' : 'Sells at least'}</dt>
            <dd className="num">{formatPx(built.price, market.priceDecimals)} USD</dd>
          </div>
        )}
        {built.estimate?.avgPx && (
          <div>
            <dt>Estimated average fill</dt>
            <dd className="num">
              {formatPx(built.estimate.avgPx, market.priceDecimals)} · slippage {formatPpm(built.estimate.slippagePpm ?? 0n, 3)}
            </dd>
          </div>
        )}
        <div>
          <dt>Order value</dt>
          <dd className="num">{preview ? `${formatUsd(preview.notional)} USDC` : DASH}</dd>
        </div>
        <div>
          <dt>Margin required</dt>
          <dd className="num">{preview ? `${formatUsd(preview.margin, { rounding: 'half' })} USDC` : DASH}</dd>
        </div>
        <div>
          <dt>Fee ({built.isMarket || tif === 'IOC' ? 'taker' : 'maker if it rests'})</dt>
          <dd className="num">{preview ? `${formatUsd(preview.fee)} USDC` : DASH}</dd>
        </div>
        <div className="est-strong">
          <dt>Est. liquidation price</dt>
          <dd className="num">{preview ? (preview.liquidationPx === null ? 'None' : `${formatPx(preview.liquidationPx, market.priceDecimals)} USD`) : DASH}</dd>
        </div>
        <div>
          <dt>Position after</dt>
          <dd className="num">{preview ? `${formatSize(preview.after.size, market.sizeDecimals, 'always')} ${market.base}` : DASH}</dd>
        </div>
      </dl>

      <p className="fine estimate-note">
        <span className="note-long">Estimates from integer arithmetic on the exchange's published values; the engine's result is final. Leveraged positions can be liquidated.</span>
        <span className="note-short">Estimates; the engine's result is final.</span>
      </p>
      </>
      )}
      </div>
      <div className="gate-foot">
      {summary && (
        <dl className="account-mini">
          <div>
            <dt>
              Equity<span className="unit-dt"> (USDC)</span>
            </dt>
            <dd className="num">
              {formatUsd(summary.equity)} <span className="unit">USDC</span>
            </dd>
          </div>
          <div>
            <dt>Available</dt>
            <dd className="num">
              {formatUsd(summary.available, { rounding: 'floor' })} <span className="unit">USDC</span>
            </dd>
          </div>
          <div className="mini-ratio">
            <dt>Margin ratio</dt>
            <dd className={`num health-${health(summary.marginRatioPpm)}`}>
              <HealthMeter ppm={summary.marginRatioPpm} />
              {summary.marginRatioPpm === null ? DASH : formatPpm(summary.marginRatioPpm, 2)}
            </dd>
          </div>
        </dl>
      )}
      {!(account && !acknowledged) && ((built.errors.length > 0 && size) || problems.length > 0 || built.warnings.length > 0) ? (
        <ul className="form-messages" aria-live="polite">
          {size && built.errors.map((e) => <li key={e} className="msg-danger">{e}</li>)}
          {problems.map((p) => (
            <li key={p} className="msg-danger">
              {PROBLEM_WORDS[p]}
            </li>
          ))}
          {built.warnings.map((w) => (
            <li key={w} className="msg-warn">
              {w}
            </li>
          ))}
        </ul>
      ) : null}
        {account && !acknowledged ? (
          <p className="gate-foot-note">Read and sign the statement above to place your first order.</p>
        ) : (
          <button type="button" className={`button button-block button-trade ${side === 'buy' ? 'tone-long' : 'tone-short'}`} disabled={!built.message || busy || problems.includes('reduce-only-increases')} onClick={submit}>
            {!account ? 'Connect a wallet to trade' : built.message ? `Review ${verb.toLowerCase()} ${size} ${market.base}` : `${verb} ${market.base}`}
          </button>
        )}
      </div>
      {leverageOpen && <LeverageDialog market={market} current={leverage} isolated={isolated} onClose={() => setLeverageOpen(false)} />}
    </section>
  );
}

const PROBLEM_WORDS = {
  'insufficient-margin': 'Not enough available margin for this order (margin plus fee).',
  'leverage-above-tier': 'At this size the margin tier allows less leverage than you set.',
  'reduce-only-increases': 'Reduce only: this order would open or grow a position.',
  'would-liquidate': 'This order would put the position past its liquidation price at the current mark.',
} as const;

export function HealthMeter({ ppm }: { ppm: bigint | null }) {
  const pct = ppm === null ? 0 : Math.min(100, Number(ppm / 10_000n));
  return (
    <span className="health-meter" role="img" aria-label={`Margin ratio ${pct}% of the liquidation level`}>
      <span style={{ width: `${pct}%` }} />
    </span>
  );
}

function Amount({ label, unit, value, onChange, placeholder, hint, action }: { label: string; unit: string; value: string; onChange: (v: string) => void; placeholder?: string; hint?: string; action?: ReactNode }) {
  const id = useMemo(() => `f-${label.replace(/\W+/g, '-').toLowerCase()}`, [label]);
  return (
    <div className="amount">
      <div className="amount-label">
        <label htmlFor={id}>{label}</label>
        {action}
        {hint && <span className="amount-hint">{hint}</span>}
      </div>
      <div className="amount-box">
        <input id={id} className="num" inputMode="decimal" autoComplete="off" spellCheck={false} value={value} placeholder={placeholder} onChange={(e) => onChange(e.target.value.replace(/[^\d.,]/g, ''))} />
        <span className="amount-unit">{unit}</span>
      </div>
    </div>
  );
}

