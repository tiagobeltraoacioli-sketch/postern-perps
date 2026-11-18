/**
 * The review every L2 transaction passes through before the wallet is asked. Everything shown is decoded from the
 * calldata that will be sent (vault/vault.ts `describeTx`), never from the form.
 */
import { useFunds, deployment } from '../state/funds';
import { useActions } from '../state/actions';
import { Modal } from './Modal';
import { shortAddress } from '../lib/format';
import { config } from '../lib/config';

export function TxSheet() {
  const { pending, problem, confirm, close, onL2, switchNetwork, switching, networkError } = useFunds();
  const { account } = useActions();
  if (!pending || !deployment) return null;
  const { described: d, phase, outcome, tx, hash } = pending;
  const busy = phase === 'wallet' || phase === 'mining';
  return (
    <Modal title={d.title} onClose={close} locked={busy} eyebrow={phase === 'done' ? 'Result' : 'Review before sending · Bloch L2 transaction'}>
      <div className="review review-neutral" data-phase={phase}>
        <dl className="review-rows">
          {d.rows.map((r) => (
            <div key={r.label} className={r.emphasis ? 'review-row emphasis' : 'review-row'}>
              <dt>{r.label}</dt>
              <dd className="num">{r.value}</dd>
            </div>
          ))}
        </dl>
        <ul className="review-notes">
          {d.notes.map((n) => (
            <li key={n}>{n}</li>
          ))}
        </ul>
        {phase === 'review' && !onL2 && (
          <div className="callout callout-warn" role="status">
            <strong>Your wallet is on another network</strong>
            <p>This transaction is for the Bloch L2 (chain {config.l2ChainId}). Switch the wallet first; the page adds the network if the wallet does not know it.</p>
            <button type="button" className="button button-small" onClick={() => void switchNetwork()} disabled={switching}>
              {switching ? 'Check your wallet…' : 'Switch to Bloch L2'}
            </button>
            {networkError && <p className="msg-danger">{networkError}</p>}
          </div>
        )}
        {phase === 'review' && problem && (
          <div className="callout callout-danger" role="alert">
            <strong>Not sent</strong>
            <p>{problem}</p>
          </div>
        )}
        {phase !== 'done' && account && (
          <div className="review-signer">
            <span className="eyebrow">Sent by</span>
            <span className="num">Your wallet {shortAddress(account)}, which pays the L2 gas fee in BLCH</span>
          </div>
        )}
        <details className="review-raw">
          <summary>Exactly what is sent</summary>
          <pre className="num">{JSON.stringify({ chainId: deployment.chainId, to: tx.to, value: '0', data: tx.data }, null, 2)}</pre>
        </details>
        {phase === 'mining' && hash && (
          <div className="callout callout-ok" role="status">
            <strong>Sent. Waiting for an L2 block…</strong>
            <p className="num tx-hash">{hash}</p>
          </div>
        )}
        {outcome && (
          <div className={`callout ${outcome.ok ? 'callout-ok' : 'callout-danger'}`} role={outcome.ok ? 'status' : 'alert'}>
            <strong>{outcome.title}</strong>
            <p>{outcome.text}</p>
            {hash && <p className="num tx-hash">{hash}</p>}
          </div>
        )}
        <div className="review-actions">
          {phase === 'done' ? (
            <button type="button" className="button button-primary" onClick={close} autoFocus>
              Close
            </button>
          ) : (
            <>
              <button type="button" className="button button-quiet" onClick={close} disabled={busy}>
                Cancel
              </button>
              <button type="button" className="button button-primary" onClick={() => void confirm()} disabled={busy || !onL2 || !account} autoFocus>
                {phase === 'wallet' ? 'Confirm in your wallet…' : phase === 'mining' ? 'Waiting for the L2…' : 'Send in wallet'}
              </button>
            </>
          )}
        </div>
      </div>
    </Modal>
  );
}
