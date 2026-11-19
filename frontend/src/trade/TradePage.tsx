/** The trading screen: market bar, chart, book and trades, the order gate, the account. */
import { useEffect, useRef, useState } from 'react';
import { feed } from '../state/session';
import { useStore } from '../lib/store';
import { MarketBar } from './MarketBar';
import { ChartPanel, chartInterval } from './ChartPanel';
import { OrderBook, TradeTape } from './BookAndTrades';
import { DepthChart } from './DepthChart';
import { OrderPanel } from './OrderPanel';
import { AccountTables } from './AccountTables';
import { orderSide, pricePick, useMarket, useMedia, useTicker } from './hooks';
import { signingProblem } from '../lib/pins';

type PhoneTab = 'chart' | 'book' | 'depth' | 'trades';
type SideTab = 'book' | 'depth' | 'trades';
const SIDE_WORDS: Record<SideTab, string> = { book: 'Book', depth: 'Depth', trades: 'Trades' };

export function TradePage({ symbol }: { symbol: string }) {
  const market = useMarket(symbol);
  const ticker = useTicker(symbol);
  const markets = useStore(feed.markets);
  const marketsError = useStore(feed.marketsError);
  const interval = useStore(chartInterval);
  const [phoneTab, setPhoneTab] = useState<PhoneTab>('chart');
  const [sideTab, setSideTab] = useState<SideTab>('book');
  const phone = useMedia('(max-width: 899px)');
  // Where the tape is in view beside or under the book (a tall or a very wide fitted screen) the switch offers only
  // Book and Depth; a remembered "Trades" then reads as Book.
  const tapeInView = useMedia('(min-width: 1024px) and (min-height: 1000px), (min-width: 2100px) and (min-height: 680px)');
  const sideShown: SideTab = tapeInView && sideTab === 'trades' ? 'book' : sideTab;
  const [sheet, setSheet] = useState(false);
  const formRef = useRef<HTMLDivElement>(null);
  const openSheet = (side: 'buy' | 'sell') => {
    orderSide.set(side);
    setSheet(true);
  };
  useEffect(() => {
    if (!phone) setSheet(false);
  }, [phone]);
  useEffect(() => {
    if (!(phone && sheet)) return;
    const key = (e: KeyboardEvent) => e.key === 'Escape' && setSheet(false);
    document.addEventListener('keydown', key);
    const previous = document.body.style.overflow;
    document.body.style.overflow = 'hidden';
    formRef.current?.querySelector<HTMLElement>('input, button')?.focus({ preventScroll: true });
    return () => {
      document.removeEventListener('keydown', key);
      document.body.style.overflow = previous;
    };
  }, [phone, sheet]);
  // A price picked in the book or the chart on a phone opens the form with it.
  const picked = useStore(pricePick);
  useEffect(() => {
    if (phone && picked && Date.now() - picked.at < 500) setSheet(true);
  }, [phone, picked]);
  useEffect(() => {
    if (market) feed.select(market.symbol, interval);
  }, [market, interval]);
  useEffect(() => {
    document.title = ticker && market ? `${market.symbol} perpetual · PosternDEX` : 'PosternDEX Perpetuals';
  }, [ticker, market]);
  if (!market)
    return (
      <div className="page-message">
        {marketsError ? <p className="callout callout-danger">{marketsError} Nothing can be signed until the markets are read.</p> : markets.length ? <p>{symbol} is not a market of this exchange.</p> : <p>Reading markets…</p>}
      </div>
    );
  const pinProblem = signingProblem(market);
  return (
    <div className="trade" data-phone-tab={phoneTab} data-sheet={phone && sheet ? 'open' : 'closed'}>
      <h1 className="sr-only">{market.symbol} perpetual: trade</h1>
      <MarketBar market={market} ticker={ticker} />
      {pinProblem && (
        <p className="callout callout-danger trade-alert" role="alert">
          Signing is refused on {market.symbol}: {pinProblem}. This interface signs only the market table it was built with.
        </p>
      )}
      {market.status === 'halted' && (
        <p className="callout callout-danger trade-alert" role="alert">
          <strong>{market.symbol} is halted.</strong> {market.haltReason ?? ''} Open orders can be cancelled; new orders are not accepted until it reopens.
        </p>
      )}
      <div className="phone-tabs segmented" role="group" aria-label="View">
        {(['chart', 'book', 'depth', 'trades'] as PhoneTab[]).map((t) => (
          <button key={t} type="button" aria-pressed={phoneTab === t} onClick={() => setPhoneTab(t)}>
            {t === 'chart' ? 'Chart' : SIDE_WORDS[t]}
          </button>
        ))}
      </div>
      <div className="trade-chart">
        <ChartPanel market={market} ticker={ticker} />
      </div>
      {/* On a fitted desktop screen of moderate height the book and the tape share one column behind a switch (the
          book keeps its full depth); on a tall one, and on every other layout, both are shown. */}
      <div className="trade-side" data-side-tab={sideShown} data-tape-in-view={tapeInView ? 'true' : undefined}>
        <div className="side-tabs segmented" role="group" aria-label="Book, depth or trades">
          {(['book', 'depth', 'trades'] as const).map((t) => (
            <button key={t} type="button" data-tab={t} aria-pressed={sideShown === t} onClick={() => setSideTab(t)}>
              {SIDE_WORDS[t]}
            </button>
          ))}
        </div>
        <div className="trade-book">
          <OrderBook market={market} />
        </div>
        {/* Drawn only while it is the chosen view. */}
        <div className="trade-depth">{(phone ? phoneTab : sideShown) === 'depth' && <DepthChart market={market} />}</div>
        <div className="trade-tape">
          <TradeTape market={market} />
        </div>
      </div>
      {/* Phone: the order form is a sheet from the bottom, opened by the Long / Short bar that stays in thumb reach.
          While closed it is inert there (not reachable by keyboard or assistive technology); elsewhere it is a column. */}
      {phone && sheet && <button type="button" className="sheet-scrim" aria-label="Close the order form" onClick={() => setSheet(false)} />}
      <div className="trade-form" ref={formRef} data-open={sheet ? 'true' : 'false'} inert={phone && !sheet} role={phone ? 'dialog' : undefined} aria-modal={phone && sheet ? true : undefined} aria-label={phone ? 'Order form' : undefined}>
        <OrderPanel market={market} ticker={ticker} onClose={phone ? () => setSheet(false) : undefined} />
      </div>
      <div className="trade-cta" role="group" aria-label="Trade">
        <button type="button" className="button button-trade tone-long" onClick={() => openSheet('buy')}>
          Long
        </button>
        <button type="button" className="button button-trade tone-short" onClick={() => openSheet('sell')}>
          Short
        </button>
      </div>
      <div className="trade-account">
        <AccountTables only={market.symbol} />
      </div>
    </div>
  );
}
