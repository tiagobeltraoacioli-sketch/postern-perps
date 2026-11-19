/** Portfolio: the account as the exchange reports it, beside this interface's own estimate from the same inputs. */
import { feed, go } from '../state/session';
import { useStore } from '../lib/store';
import { useActions } from '../state/actions';
import { accountSummary, health } from '../lib/calc';
import { formatCompactUsd, formatPpm, formatUsd, formatBps, DASH } from '../lib/format';
import { positionInputs } from '../trade/hooks';
import { AccountTables } from '../trade/AccountTables';
import { HealthMeter } from '../trade/OrderPanel';
import { GateGlyph } from '../ui/BrandMark';

export function Portfolio() {
  const state = useStore(feed.account);
  const markets = useStore(feed.markets);
  const tickers = useStore(feed.tickers);
  const { account } = useActions();
  if (!account)
    return (
      <div className="page-message">
        <GateGlyph size={40} />
        <h1>Portfolio</h1>
        <p>Connect a wallet to see your equity, margin, positions and history.</p>
      </div>
    );
  const a = state?.account;
  const ours = state ? accountSummary(state.account.collateral, positionInputs(state, markets, tickers)) : null;
  const ratio = a ? a.marginRatioPpm : null;
  const h = health(ratio);
  const used = a && a.equity > 0n ? Number((a.initialMargin * 10_000n) / a.equity) / 100 : 0;
  const usedPct = Math.max(0, Math.min(100, used));
  return (
    <div className="page page-wide">
      <header className="page-head page-head-row">
        <div>
          <p className="eyebrow">Portfolio</p>
          <h1 className="display num">{a ? `${formatUsd(a.equity)} USDC` : DASH}</h1>
          <p className="lead">Equity: collateral plus unrealised PnL, as reported by the exchange at block {a ? a.height : DASH}.</p>
        </div>
        <div className="head-actions">
          <button type="button" className="button button-primary" onClick={() => go({ page: 'transfers', tab: 'deposit' })}>
            Deposit
          </button>
          <button type="button" className="button" onClick={() => go({ page: 'transfers', tab: 'withdraw' })}>
            Withdraw
          </button>
          <button type="button" className="button button-quiet" onClick={() => go({ page: 'history', tab: 'fills' })}>
            History
          </button>
        </div>
      </header>
      <div className="portfolio-top">
        <section className="panel balance-panel" aria-label="Balance">
          <header className="panel-head">
            <h2>Where the equity is</h2>
            <span className="fine num">{a ? `${formatPpm(BigInt(Math.round(usedPct * 10_000)), 1)} in use` : ''}</span>
          </header>
          {/* One measure (equity) split in two parts: margin held by positions and orders, and the rest. */}
          <div className="split-bar" role="img" aria-label={a ? `Margin in use ${formatUsd(a.initialMargin)} USDC of ${formatUsd(a.equity)} USDC equity` : 'No account data'}>
            <span className="split-used" style={{ width: `${usedPct}%` }} />
          </div>
          <dl className="balance-rows">
            <Row label="Collateral" hint="bUSDC credited to your trading account" value={a && `${formatUsd(a.collateral)} USDC`} />
            <Row label="Unrealised PnL" hint="Open positions at the mark price" value={a && `${formatUsd(a.unrealised, { sign: 'always' })} USDC`} tone={a ? (a.unrealised > 0n ? 'up' : a.unrealised < 0n ? 'down' : undefined) : undefined} />
            <Row label="Margin in use" hint="Held by positions and resting orders" value={a && `${formatUsd(a.initialMargin)} USDC`} swatch="used" />
            <Row label="Free collateral" hint="Available for new orders" value={a && `${formatUsd(a.free, { rounding: 'floor' })} USDC`} swatch="free" />
            <Row label="Withdrawable" hint="What a withdrawal may take now" value={a && `${formatUsd(a.withdrawable, { rounding: 'floor' })} USDC`} />
          </dl>
        </section>
        <section className={`panel risk-panel health-${h}`} aria-label="Risk">
          <header className="panel-head">
            <h2>Distance from liquidation</h2>
          </header>
          <div className="risk-body">
            <span className="risk-figure num">{ratio === null ? DASH : formatPpm(ratio, 2)}</span>
            <span className="fine">Margin ratio: maintenance margin over equity. Cross positions are liquidated at 100%.</span>
            <HealthMeter ppm={ratio} />
            <dl className="balance-rows">
              <Row label="Maintenance margin" hint="Below this equity, liquidation starts" value={a && `${formatUsd(a.maintenanceMargin)} USDC`} />
              <Row label="Fee tier" hint={a ? `14-day volume $${formatCompactUsd(a.feeTier.volume30d)}` : ''} value={a && `Tier ${a.feeTier.tier}: maker ${formatBps(a.feeTier.makerPpm)}, taker ${formatBps(a.feeTier.takerPpm)}`} />
            </dl>
          </div>
        </section>
      </div>
      {ours && a && ours.equity !== a.equity && (
        <p className="fine">
          This interface's own estimate of equity from the same positions at the current mark: {formatUsd(ours.equity)} USDC. The exchange's figure above is the one that counts; the two differ while marks move between blocks.
        </p>
      )}
      <AccountTables />
    </div>
  );
}

function Row({ label, hint, value, tone, swatch }: { label: string; hint?: string; value: string | undefined | null | false; tone?: 'up' | 'down'; swatch?: 'used' | 'free' }) {
  return (
    <div>
      <dt>
        {swatch && <span className={`swatch swatch-${swatch}`} aria-hidden="true" />}
        {label}
        {hint && <span className="fine"> {hint}</span>}
      </dt>
      <dd className={`num ${tone ?? ''}`}>{value || DASH}</dd>
    </div>
  );
}
