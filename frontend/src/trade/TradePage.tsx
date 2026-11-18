/** The trading screen: market bar, chart, book and trades, the order gate, the account. */
import { useEffect, useState } from 'react';
import { feed } from '../state/session';
import { useStore } from '../lib/store';
import { MarketBar } from './MarketBar';
import { ChartPanel, chartInterval } from './ChartPanel';
import { OrderBook, TradeTape } from './BookAndTrades';
import { OrderPanel } from './OrderPanel';
import { AccountTables } from './AccountTables';
import { useMarket, useTicker } from './hooks';
import { signingProblem } from '../lib/pins';

type PhoneTab = 'chart' | 'book' | 'trades';

export function TradePage({ symbol }: { symbol: string }) {
  const market = useMarket(symbol);
  const ticker = useTicker(symbol);
  const markets = useStore(feed.markets);
  const marketsError = useStore(feed.marketsError);
  const interval = useStore(chartInterval);
  const [phoneTab, setPhoneTab] = useState<PhoneTab>('chart');
  const [sideTab, setSideTab] = useState<'book' | 'trades'>('book');
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
    <div className="trade" data-phone-tab={phoneTab}>
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
        {(['chart', 'book', 'trades'] as PhoneTab[]).map((t) => (
          <button key={t} type="button" aria-pressed={phoneTab === t} onClick={() => setPhoneTab(t)}>
            {t === 'chart' ? 'Chart' : t === 'book' ? 'Book' : 'Trades'}
          </button>
        ))}
      </div>
      <div className="trade-chart">
        <ChartPanel market={market} ticker={ticker} />
      </div>
      {/* On a fitted desktop screen of moderate height the book and the tape share one column behind a switch (the
          book keeps its full depth); on a tall one, and on every other layout, both are shown. */}
      <div className="trade-side" data-side-tab={sideTab}>
        <div className="side-tabs segmented" role="group" aria-label="Book or trades">
          {(['book', 'trades'] as const).map((t) => (
            <button key={t} type="button" aria-pressed={sideTab === t} onClick={() => setSideTab(t)}>
              {t === 'book' ? 'Order book' : 'Trades'}
            </button>
          ))}
        </div>
        <div className="trade-book">
          <OrderBook market={market} />
        </div>
        <div className="trade-tape">
          <TradeTape market={market} />
        </div>
      </div>
      <div className="trade-form">
        <OrderPanel market={market} ticker={ticker} />
      </div>
      <div className="trade-account">
        <AccountTables only={market.symbol} />
      </div>
    </div>
  );
}
