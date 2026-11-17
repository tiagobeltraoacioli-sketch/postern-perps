/** Portfolio: the account as the exchange reports it, beside this interface's own estimate from the same inputs. */
import { feed } from '../state/session';
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
  return (
    <div className="page">
      <header className="page-head">
        <p className="eyebrow">Portfolio</p>
        <h1 className="display num">{a ? `${formatUsd(a.equity)} USDC` : DASH}</h1>
        <p className="lead">Equity: collateral plus unrealised PnL, as reported by the exchange at block {a ? a.height : DASH}.</p>
      </header>
      <div className="cards">
        <Card label="Collateral" value={a && `${formatUsd(a.collateral)} USDC`} />
        <Card label="Unrealised PnL" value={a && `${formatUsd(a.unrealised, { sign: 'always' })} USDC`} tone={a ? (a.unrealised >= 0n ? 'up' : 'down') : undefined} />
        <Card label="Margin in use" value={a && `${formatUsd(a.initialMargin)} USDC`} />
        <Card label="Maintenance margin" value={a && `${formatUsd(a.maintenanceMargin)} USDC`} />
        <Card label="Free collateral" value={a && `${formatUsd(a.free, { rounding: 'floor' })} USDC`} />
        <Card label="Withdrawable" value={a && `${formatUsd(a.withdrawable, { rounding: 'floor' })} USDC`} />
        <div className={`card health-card health-${h}`}>
          <span className="card-label">Margin ratio</span>
          <span className="card-value num">{ratio === null ? DASH : formatPpm(ratio, 2)}</span>
          <HealthMeter ppm={ratio} />
          <span className="fine">Maintenance margin over equity. Cross positions are liquidated at 100%.</span>
        </div>
        <Card label="Fee tier" value={a && `Tier ${a.feeTier.tier}: maker ${formatBps(a.feeTier.makerPpm)}, taker ${formatBps(a.feeTier.takerPpm)}`} sub={a && `30-day volume $${formatCompactUsd(a.feeTier.volume30d)}`} />
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

function Card({ label, value, sub, tone }: { label: string; value: string | undefined | null | false; sub?: string | false | null; tone?: 'up' | 'down' }) {
  return (
    <div className="card">
      <span className="card-label">{label}</span>
      <span className={`card-value num ${tone ?? ''}`}>{value || DASH}</span>
      {sub && <span className="fine">{sub}</span>}
    </div>
  );
}
