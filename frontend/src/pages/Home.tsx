/**
 * The public home page of posterndex.com. UNAUDITED prototype; the product is NOT live and the page says so.
 *
 * Rule of this page: every sentence is true of the system as built, and every number comes from the file that owns
 * it (fees and tiers from compliance/risk.json and compliance/markets.json; the vault's delays and committee from
 * contracts/deploy/vault.config.json, marked as proposed; markets and prices from the exchange API, live). It claims
 * no audit, licence, volume or user that does not exist. Notices and risk wording are 392's drafts for counsel
 * (compliance/notices.ts). The gate and the lamp carry the page: no stock imagery, no third-party asset.
 */
import risk from '../../../compliance/risk.json';
import launch from '../../../compliance/markets.json';
import vault from '../../../contracts/deploy/vault.config.json';
import { ArrowRight, ArrowUpRight } from 'lucide-react';
import { feed, go } from '../state/session';
import { useStore } from '../lib/store';
import { formatBps, formatCompactUsd, formatDuration, formatFixed, formatPpm, formatPx, formatRate, DASH } from '../lib/format';
import { notional } from '../lib/calc';
import { AssetMark } from '../trade/AssetMark';
import { OPERATOR_LINE, RESTRICTED_SUMMARY, RISK_POINTS } from '../compliance/notices';
import { ESCAPE_VALUATION_WORDS } from '../lib/exit-band';

/** What the page states about fees: read from compliance data, never typed into the prose. Exported for the test. */
export const FEES = {
  base: risk.feeTiers.tiers[0],
  tiers: risk.feeTiers.tiers,
  window: risk.feeTiers.window,
  withdrawal: risk.global.withdrawalFee,
  deposit: risk.global.depositFee,
  liquidationPpm: [...new Set(launch.markets.map((m) => m.liquidationFeePpm))],
  maxFundingRate: [...new Set(launch.markets.map((m) => m.maxFundingRate))],
};
export const VAULT_PROPOSAL = {
  validators: vault.validators.length,
  threshold: vault.threshold,
  guardians: vault.guardians.length,
  forcedDelayMs: vault.forcedDelay * 1000,
  escapeDelayMs: vault.escapeDelay * 1000,
  largeDelayMs: vault.largeDelay * 1000,
  largeThreshold: BigInt(vault.largeThreshold),
  pauseMs: vault.pauseDuration * 1000,
};
const bps = (ppm: number) => formatBps(BigInt(ppm));
const usd0 = (s: string) => `$${formatCompactUsd(BigInt(s) * 1_000_000n)}`;

export function Home() {
  const markets = useStore(feed.markets);
  const tickers = useStore(feed.tickers);
  const error = useStore(feed.marketsError);
  const status = useStore(feed.status);
  const first = markets[0]?.symbol ?? 'BTC-USD';
  const launchApp = () => go({ page: 'trade', symbol: first });
  const v = VAULT_PROPOSAL;
  return (
    <div className="home">
      <section className="hero" aria-labelledby="hero-title">
        <div className="hero-copy">
          <p className="eyebrow hero-eyebrow">
            <span className="lamp-dot" aria-hidden="true" /> Perpetual futures · by Bloch Inc · prototype, not live
          </p>
          <h1 id="hero-title" className="hero-title">
            Trade in the open.
            <br />
            Keep the way out.
          </h1>
          <p className="hero-lead">
            PosternDEX is a perpetual futures exchange with one transparent order book. Your collateral sits in a vault contract on the Bloch L2 with no admin key, and the vault itself guarantees your exit: a postern is the gate you can always leave by.
          </p>
          <div className="hero-actions">
            <button type="button" className="button button-primary button-large" onClick={launchApp}>
              Launch app <ArrowRight size={18} aria-hidden="true" />
            </button>
            <a className="button button-large" href="#custody" onClick={(e) => (e.preventDefault(), document.getElementById('custody')?.scrollIntoView({ behavior: 'smooth', block: 'start' }))}>
              How custody works
            </a>
          </div>
          <p className="fine hero-note">Unaudited prototype. No real funds. Nothing here is an offer or advice.</p>
        </div>
        <Gate />
      </section>

      <section className="strip" aria-label="Markets, live from the exchange">
        {markets.length === 0 ? (
          <p className="strip-empty">{error ? 'The exchange is not reachable from here right now: no market data to show. Nothing is live yet.' : 'Reading markets…'}</p>
        ) : (
          <ul>
            {markets.map((m) => {
              const t = tickers.get(m.symbol);
              return (
                <li key={m.symbol}>
                  <button type="button" onClick={() => go({ page: 'trade', symbol: m.symbol })} aria-label={`Open ${m.symbol}`}>
                    <AssetMark asset={m.base} size={30} />
                    <span className="strip-name">
                      <strong>{m.symbol}</strong>
                      <span className="sub">Perpetual · up to {m.maxLeverage}x</span>
                    </span>
                    <span className="strip-px num">{t ? formatPx(t.mark, m.priceDecimals) : DASH}</span>
                    <span className={`num strip-change ${t ? (t.changePpm > 0n ? 'up' : t.changePpm < 0n ? 'down' : '') : ''}`}>{t ? formatPpm(t.changePpm, 2, 'always') : DASH}</span>
                    <span className="strip-meta">
                      <span className="sub">24 h volume</span>
                      <span className="num">{t ? `$${formatCompactUsd(t.volume24h)}` : DASH}</span>
                    </span>
                    <span className="strip-meta">
                      <span className="sub">Open interest</span>
                      <span className="num">{t ? `$${formatCompactUsd(notional(t.openInterest, t.mark))}` : DASH}</span>
                    </span>
                    <span className="strip-meta">
                      <span className="sub">Funding</span>
                      <span className={`num ${t ? (t.fundingRate >= 0n ? 'up' : 'down') : ''}`}>{t ? formatRate(t.fundingRate) : DASH}</span>
                    </span>
                    <ArrowUpRight size={16} aria-hidden="true" className="strip-go" />
                  </button>
                </li>
              );
            })}
          </ul>
        )}
        <p className="fine strip-foot">
          {status ? `Read live from the exchange${status.mode !== 'live' ? ` (a ${status.mode} instance, not a live market)` : ''}.` : 'Figures appear when the exchange answers.'} Marks follow an oracle index, never the last trade alone.
        </p>
      </section>

      <section className="pillars" aria-label="What it is">
        <article>
          <span className="pillar-n num">I</span>
          <h2>One book, in the open</h2>
          <p>A central limit order book with price-time priority. Every order, fill, funding payment and liquidation is in a public log that anyone can replay to the same state root.</p>
        </article>
        <article>
          <span className="pillar-n num">II</span>
          <h2>Custody by contract</h2>
          <p>Collateral is bUSDC in a vault on the Bloch L2. No owner, no upgrade, no key that can move funds: they leave only against a state root that a threshold of independent validators replayed and signed.</p>
        </article>
        <article>
          <span className="pillar-n num">III</span>
          <h2>The exit is yours</h2>
          <p>If the exchange ignores you, ask the vault. If the exchange stops, the vault pays every account its last proven balance. No one's permission is needed, and no region rule can close that gate.</p>
        </article>
      </section>

      <section className="custody" id="custody" aria-labelledby="custody-title">
        <header className="section-head">
          <p className="eyebrow">Custody</p>
          <h2 id="custody-title" className="display">Where your collateral is, and how it leaves</h2>
          <p className="lead">The exchange runs off-chain for speed. The money does not: it stays in the vault, and the vault believes only what the validators signed, or what it recorded itself.</p>
        </header>
        <CustodyDiagram />
        <ol className="custody-steps">
          <li>
            <strong>Deposit.</strong> You send bUSDC to the vault. The exchange credits it only after the L2 block is final.
          </li>
          <li>
            <strong>Trade.</strong> One sequencer orders signed actions into blocks. It cannot sign for you and cannot move funds.
          </li>
          <li>
            <strong>Checkpoint.</strong> Validators replay every block. {v.threshold} of {v.validators} must sign a state root before the vault accepts it (proposed launch values).
          </li>
          <li>
            <strong>Withdraw.</strong> The vault pays a withdrawal proven against a signed root. {formatFixed(v.largeThreshold, 6, { dp: 0 })} bUSDC or more waits {formatDuration(v.largeDelayMs)} first; guardians can pause, for a bounded time, and can never move funds.
          </li>
          <li>
            <strong>Forced request.</strong> Censored? Post your withdrawal, close or cancel on the L2. The exchange must process it within {formatDuration(v.forcedDelayMs)}.
          </li>
          <li>
            <strong>Escape.</strong> A request ignored past its deadline, or no checkpoint for {formatDuration(v.escapeDelayMs)}: anyone can switch the vault to escape mode. Every account then claims its last proven balance ({ESCAPE_VALUATION_WORDS}); users are paid before the insurance fund.
          </li>
        </ol>
        <p className="fine">
          Delays and committee size are the proposed launch values of the vault's configuration; the deployed contract fixes them for good. What you trust: the validator threshold for the state root, the Bloch L2 and its bridge for bUSDC, and software that has not been audited.
        </p>
      </section>

      <section className="fees" aria-labelledby="fees-title">
        <header className="section-head">
          <p className="eyebrow">Fees</p>
          <h2 id="fees-title" className="display">
            Taker {bps(FEES.base.takerFeePpm)}, maker {bps(FEES.base.makerFeePpm)}
          </h2>
          <p className="lead">Of the traded value, at the base tier. No deposit fee. Funding is paid between traders; the exchange takes no part of it.</p>
        </header>
        <div className="fees-grid">
          <div className="panel">
            <div className="table-wrap">
              <table>
                <caption className="sr-only">Fee tiers by volume</caption>
                <thead>
                  <tr>
                    <th>Tier</th>
                    <th className="r">14-day volume from</th>
                    <th className="r">Taker</th>
                    <th className="r">Maker</th>
                  </tr>
                </thead>
                <tbody>
                  {FEES.tiers.map((t) => (
                    <tr key={t.tier}>
                      <td className="num">{t.tier}</td>
                      <td className="r num">{t.minVolume14d === '0' ? 'Any' : usd0(t.minVolume14d)}</td>
                      <td className="r num">{bps(t.takerFeePpm)}</td>
                      <td className="r num">{t.makerFeePpm === 0 ? 'None' : bps(t.makerFeePpm)}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
            <p className="fine table-foot">Proposed tiers; the prototype engine applies the base tier to every account until volume tiers are switched on.</p>
          </div>
          <dl className="fees-facts">
            <div>
              <dt>Deposit</dt>
              <dd className="num">{FEES.deposit === '0' ? 'No fee' : `${FEES.deposit} USDC`}</dd>
              <dd className="fine">You pay the L2 gas.</dd>
            </div>
            <div>
              <dt>Withdrawal</dt>
              <dd className="num">{FEES.withdrawal} USDC flat</dd>
              <dd className="fine">Covers the L2 gas of the payout.</dd>
            </div>
            <div>
              <dt>Liquidation</dt>
              <dd className="num">{FEES.liquidationPpm.map((p) => formatPpm(BigInt(p), 2)).join(' / ')}</dd>
              <dd className="fine">Of the liquidated value, to the insurance fund, never below zero equity.</dd>
            </div>
            <div>
              <dt>Funding cap</dt>
              <dd className="num">{FEES.maxFundingRate.map((r) => `±${formatRate(BigInt(r), 'never')}`).join(' / ')} per hour</dd>
              <dd className="fine">Between longs and shorts.</dd>
            </div>
          </dl>
        </div>
      </section>

      <section className="honest" aria-labelledby="honest-title">
        <header className="section-head">
          <p className="eyebrow">Before you trade</p>
          <h2 id="honest-title" className="display">What can go wrong</h2>
        </header>
        <ul className="honest-list">
          {RISK_POINTS.slice(0, 4).map((p) => (
            <li key={p}>{p}</li>
          ))}
        </ul>
        <div className="honest-foot">
          <p>{RESTRICTED_SUMMARY}</p>
          <div className="hero-actions">
            <button type="button" className="button" onClick={() => go({ page: 'legal' })}>
              Notices, risk and where it is not available
            </button>
            <button type="button" className="button button-primary" onClick={launchApp}>
              Launch app <ArrowRight size={16} aria-hidden="true" />
            </button>
          </div>
          <p className="fine">{OPERATOR_LINE} Notice and risk wording are drafts for counsel.</p>
        </div>
      </section>
    </div>
  );
}

/** The gate: dressed stone, one arch, the lamp over it. Drawn here; no image file. */
function Gate() {
  const courses = [0, 1, 2, 3, 4, 5, 6, 7, 8];
  return (
    <div className="gate-art" aria-hidden="true">
      <svg viewBox="0 0 420 480" focusable="false">
        <defs>
          <radialGradient id="g-glow" cx="50%" cy="16%" r="62%">
            <stop offset="0" stopColor="var(--net)" stopOpacity="0.5" />
            <stop offset="0.45" stopColor="var(--net)" stopOpacity="0.1" />
            <stop offset="1" stopColor="var(--net)" stopOpacity="0" />
          </radialGradient>
          <linearGradient id="g-way" x1="0" y1="0" x2="0" y2="1">
            <stop offset="0" stopColor="var(--net)" stopOpacity="0.16" />
            <stop offset="1" stopColor="var(--net)" stopOpacity="0" />
          </linearGradient>
          <clipPath id="g-wall">
            <rect x="20" y="40" width="380" height="420" rx="6" />
          </clipPath>
        </defs>
        <rect x="20" y="40" width="380" height="420" rx="6" fill="var(--stone-1)" stroke="var(--stone-edge)" />
        <g clipPath="url(#g-wall)" stroke="var(--mortar)" strokeWidth="1.5" opacity="0.7">
          {courses.map((c) => (
            <g key={c}>
              <path d={`M20 ${86 + c * 46}H400`} />
              {[0, 1, 2, 3, 4].map((b) => (
                <path key={b} d={`M${(c % 2 ? 68 : 20) + b * 96} ${40 + c * 46}v46`} />
              ))}
            </g>
          ))}
        </g>
        <rect x="20" y="40" width="380" height="420" rx="6" fill="url(#g-glow)" />
        {/* The voussoirs of the arch and the opening. */}
        <path d="M118 460V250a92 92 0 0 1 184 0V460" fill="none" stroke="var(--stone-edge)" strokeWidth="26" strokeLinejoin="round" />
        <path d="M118 460V250a92 92 0 0 1 184 0V460" fill="none" stroke="var(--stone-2)" strokeWidth="22" strokeLinejoin="round" />
        <g stroke="var(--mortar)" strokeWidth="1.5" opacity="0.8">
          {[-72, -48, -24, 0, 24, 48, 72].map((a) => {
            const r1 = 81;
            const r2 = 103;
            const rad = ((a - 90) * Math.PI) / 180;
            return <path key={a} d={`M${210 + r1 * Math.cos(rad)} ${250 + r1 * Math.sin(rad)}L${210 + r2 * Math.cos(rad)} ${250 + r2 * Math.sin(rad)}`} />;
          })}
          {[300, 350, 400].map((y) => (
            <g key={y}>
              <path d={`M107 ${y}h22`} />
              <path d={`M291 ${y}h22`} />
            </g>
          ))}
        </g>
        <path d="M129 460V250a81 81 0 0 1 162 0V460Z" fill="var(--bg)" />
        <path d="M129 460V250a81 81 0 0 1 162 0V460Z" fill="url(#g-way)" />
        <path d="M150 460 210 330 270 460Z" fill="var(--net)" opacity="0.07" />
        <rect x="6" y="458" width="408" height="10" rx="3" fill="var(--stone-3)" stroke="var(--stone-edge)" />
        {/* The lamp. */}
        <path d="M210 40v52" stroke="var(--lamp-iron)" strokeWidth="2" />
        <path d="M198 92h24l-4 26h-16Z" fill="var(--bg)" stroke="var(--lamp-iron)" strokeWidth="2" strokeLinejoin="round" />
        <circle className="gate-art-lamp" cx="210" cy="104" r="6" fill="var(--net)" />
      </svg>
    </div>
  );
}

/** How funds move, in the brand's own drawing: you, the vault (the gate), the exchange, the validators. */
function CustodyDiagram() {
  return (
    <figure className="diagram">
      <svg viewBox="0 0 960 370" role="img" aria-labelledby="dg-title dg-desc" focusable="false">
        <title id="dg-title">How collateral moves</title>
        <desc id="dg-desc">
          You deposit bUSDC into the vault on the Bloch L2. The exchange engine orders trades off-chain. Validators replay it and sign checkpoints, which the vault accepts. The vault pays withdrawals proven against a checkpoint. If the exchange is silent, a forced request and then the escape mode pay you directly from the vault.
        </desc>
        <defs>
          <marker id="dg-arrow" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse">
            <path d="M0 0 10 5 0 10Z" fill="var(--text-2)" />
          </marker>
          <marker id="dg-arrow-net" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse">
            <path d="M0 0 10 5 0 10Z" fill="var(--net)" />
          </marker>
        </defs>
        {/* Lanes */}
        <text x="20" y="24" className="dg-lane">ON THE BLOCH L2</text>
        <text x="640" y="24" className="dg-lane">OFF-CHAIN, REPLAYED BY VALIDATORS</text>
        <path d="M596 12V358" className="dg-divider" />
        {/* You */}
        <g>
          <rect x="20" y="148" width="150" height="84" rx="8" className="dg-box" />
          <text x="95" y="184" className="dg-name" textAnchor="middle">You</text>
          <text x="95" y="206" className="dg-sub" textAnchor="middle">your wallet, your key</text>
        </g>
        {/* Vault: the gate */}
        <g>
          <path d="M300 276V166a80 80 0 0 1 160 0V276Z" className="dg-vault" />
          <path d="M300 276V166a80 80 0 0 1 160 0V276Z" className="dg-vault-edge" />
          <circle cx="380" cy="112" r="5" className="dg-lamp" />
          <text x="380" y="176" className="dg-name" textAnchor="middle">Vault</text>
          <text x="380" y="198" className="dg-sub" textAnchor="middle">holds the bUSDC</text>
          <text x="380" y="216" className="dg-sub" textAnchor="middle">no admin key</text>
          <rect x="284" y="276" width="192" height="8" rx="3" className="dg-sill" />
        </g>
        {/* Engine */}
        <g>
          <rect x="640" y="44" width="290" height="92" rx="8" className="dg-box" />
          <text x="785" y="82" className="dg-name" textAnchor="middle">Exchange engine</text>
          <text x="785" y="104" className="dg-sub" textAnchor="middle">one sequencer orders signed actions</text>
          <text x="785" y="122" className="dg-sub" textAnchor="middle">order book, margin, funding, liquidation</text>
        </g>
        {/* Validators */}
        <g>
          <rect x="640" y="214" width="290" height="92" rx="8" className="dg-box" />
          <text x="785" y="250" className="dg-name" textAnchor="middle">Validators</text>
          <text x="785" y="272" className="dg-sub" textAnchor="middle">replay every block</text>
          <text x="785" y="290" className="dg-sub" textAnchor="middle">a threshold signs each state root</text>
        </g>
        {/* Flows */}
        <path d="M170 176H298" className="dg-flow" markerEnd="url(#dg-arrow)" />
        <text x="234" y="166" className="dg-label" textAnchor="middle">1 deposit</text>
        <path d="M298 212H172" className="dg-flow" markerEnd="url(#dg-arrow)" />
        <text x="234" y="232" className="dg-label" textAnchor="middle">4 withdrawal paid</text>
        <path d="M95 148V62H638" className="dg-flow dg-flow-quiet" markerEnd="url(#dg-arrow)" />
        <text x="366" y="52" className="dg-label" textAnchor="middle">2 signed orders (cannot move funds)</text>
        <path d="M785 136V212" className="dg-flow" markerEnd="url(#dg-arrow)" />
        <text x="797" y="180" className="dg-label">blocks</text>
        <path d="M640 252H464" className="dg-flow" markerEnd="url(#dg-arrow)" />
        <text x="552" y="242" className="dg-label" textAnchor="middle">3 signed checkpoint</text>
        {/* The exit */}
        <path d="M95 232V330H380V288" className="dg-flow dg-exit" markerEnd="url(#dg-arrow-net)" />
        <text x="238" y="352" className="dg-label dg-label-net" textAnchor="middle">5 forced request · 6 escape: the vault pays you directly</text>
      </svg>
      <figcaption className="fine">The numbers match the six steps below. Solid lines happen every day; the lit line is the exit that needs nobody's permission.</figcaption>
    </figure>
  );
}
