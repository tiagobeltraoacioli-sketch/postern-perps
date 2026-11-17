/**
 * The exchange's state and the trust model in words: what the sequencer, the committee, the vault and this
 * interface can and cannot do, and every check this interface makes before it asks for a signature.
 */
import { feed, signingDomain, eip712Problem, geo as geoStore, clockSkewMs } from '../state/session';
import { useStore } from '../lib/store';
import { feedStats } from '../lib/feed';
import { signingProblem, PINNED_MARKETS } from '../lib/pins';
import { formatDateTime, shortAddress, DASH } from '../lib/format';
import { config } from '../lib/config';

export function System() {
  const status = useStore(feed.status);
  const connection = useStore(feed.connection);
  const markets = useStore(feed.markets);
  const problem = useStore(eip712Problem);
  const geo = useStore(geoStore);
  const skew = useStore(clockSkewMs);
  const DOMAIN = useStore(signingDomain);
  const behind = status ? status.latestBlock.height - status.latestFinalized.height : null;
  const flushes = feedStats.flushMs;
  const p95 = flushes.length ? [...flushes].sort((a, b) => a - b)[Math.floor(flushes.length * 0.95)] : null;
  return (
    <div className="page">
      <header className="page-head">
        <p className="eyebrow">System</p>
        <h1 className="display">How this exchange can fail, and what this page checks</h1>
        <p className="lead">PosternDEX Perpetuals is an unaudited prototype. Nothing below is a promise; it is what the exchange reports and what this interface verifies itself.</p>
      </header>
      <div className="cards">
        <Fact label="Mode" value={status ? `${status.mode}${status.unaudited ? ' · unaudited' : ''}` : DASH} warn={status?.mode !== 'live'} />
        <Fact label="Sequencer" value={status ? `${status.sequencer.state} · ${status.sequencer.blockIntervalMs} ms blocks` : DASH} warn={status?.sequencer.state !== 'running'} />
        <Fact label="Latest block" value={status ? `${status.latestBlock.height} · ${formatDateTime(status.latestBlock.time)}` : DASH} />
        <Fact label="Final (committee-signed)" value={status ? `${status.latestFinalized.height}${behind !== null ? ` · ${behind} behind` : ''}` : DASH} warn={behind !== null && behind > 50} />
        <Fact label="Committee" value={status ? `${status.committee.threshold} of ${status.committee.n} · ${status.committee.online} online` : DASH} warn={!!status && status.committee.online < status.committee.threshold} />
        <Fact label="Vault" value={status?.vaultMode ?? DASH} warn={status?.vaultMode !== 'normal'} />
        <Fact label="L2 finality" value={status ? `block ${status.l2.finalizedBlock} · endpoints ${status.l2.endpointsAgree ? 'agree' : 'DISAGREE'}` : DASH} warn={!!status && !status.l2.endpointsAgree} />
        <Fact label="Connection" value={`${connection.state}${connection.latencyMs !== null ? ` · ${connection.latencyMs} ms` : ''}`} warn={connection.state !== 'open'} />
      </div>

      <section className="panel prose">
        <h2>Checks before every signature</h2>
        <ul className="checks">
          <Check ok={problem === null} text={problem === undefined ? 'Signing definitions: not checked yet.' : problem === null ? DOMAIN ? `Signing definitions published by the exchange equal the ones built into this page (domain ${DOMAIN.name} ${DOMAIN.version}, chain ${DOMAIN.chainId}, vault ${shortAddress(DOMAIN.verifyingContract)}).` : 'This build names no vault: there is no signing domain and nothing can be signed.' : `Signing definitions DIFFER: ${problem}. Signing is refused.`} />
          {PINNED_MARKETS.map((pin) => {
            const m = markets.find((x) => x.symbol === pin.symbol);
            const p = m ? signingProblem(m) : 'not published';
            return <Check key={pin.symbol} ok={p === null} text={p === null ? `${pin.symbol}: market #${pin.marketId}, decimals, tick and step equal the built-in table.` : `${pin.symbol}: ${p}. Signing refused on this market.`} />;
          })}
          <Check ok={!!geo && !geo.restricted && !geo.sanctioned} text={geo ? (geo.sanctioned ? 'Address on the sanctions list: everything refused.' : geo.restricted ? `Region ${geo.country ?? 'unknown'}: exit only (cancel, reduce, withdraw).` : `Region ${geo.country}: open (list ${geo.listVersion}).`) : 'Region not confirmed: exit only.'} />
          <Check ok={skew === null || Math.abs(skew) < 5000} text={skew === null ? 'Clock: not compared yet.' : `Clock: this device is ${Math.abs(Math.round(skew / 1000))} s ${skew > 0 ? 'behind' : 'ahead of'} the exchange. Nonces are the millisecond clock.`} />
          <Check ok text="Every signature is checked here before it is sent: 65 bytes, low-s, and it must recover to the key you chose." />
          <Check ok text="Market orders always carry a worst price; this page never signs “any price”." />
        </ul>
      </section>

      <section className="panel prose">
        <h2>Trust model</h2>
        <dl className="trust">
          <dt>The sequencer</dt>
          <dd>Orders every action into blocks. It cannot sign for you (every action carries your signature and a nonce) and cannot move funds out (withdrawals need the committee). It can delay or reorder your actions; the forced-action path on the Bloch L2 bounds the delay, after which the vault enters escape mode.</dd>
          <dt>The validator committee</dt>
          <dd>Replays every block and signs its state root; a block is final at {status ? `${status.committee.threshold} of ${status.committee.n}` : 'the threshold'} signatures. A threshold of colluding validators could certify a false root: that is the trust you place in them.</dd>
          <dt>The vault</dt>
          <dd>Holds the bUSDC on the Bloch L2 ({config.vault ? shortAddress(config.vault) : 'not deployed'}). It pays withdrawals only against the committee's signatures over a final root, delays large amounts, limits each window and can be paused by guardians, who cannot move funds. No admin key can move funds or change limits.</dd>
          <dt>This interface</dt>
          <dd>Operated by Bloch Inc (Panama). It signs nothing by itself: your wallet or the agent key you approved signs, after the review. Its code is served as static files with a strict content security policy (scripts only from its own origin).</dd>
        </dl>
      </section>

      <section className="panel prose">
        <h2>This page's own performance</h2>
        <p className="num">
          Feed: {feedStats.messages} messages in {feedStats.flushes} frames, largest batch {feedStats.maxBatch}, 95th percentile frame work {p95 === null ? DASH : `${p95.toFixed(2)} ms`}.
        </p>
      </section>
    </div>
  );
}

function Fact({ label, value, warn }: { label: string; value: string; warn?: boolean }) {
  return (
    <div className={`card ${warn ? 'card-warn' : ''}`}>
      <span className="card-label">{label}</span>
      <span className="card-value num">{value}</span>
    </div>
  );
}
function Check({ ok, text }: { ok: boolean; text: string }) {
  return (
    <li className={ok ? 'check-ok' : 'check-bad'}>
      <span aria-hidden="true">{ok ? '✓' : '!'}</span> {text}
    </li>
  );
}
