/**
 * The review every action passes through before a signature. Everything shown is rendered from the values that are
 * signed (lib/eip712.ts `describe`), never from the form. Refusals are shown in full and disable the button.
 */
import { useActions } from '../state/actions';
import { Modal } from './Modal';
import { AGENT_ALLOWED } from '../lib/eip712';
import { shortAddress } from '../lib/format';

export function ReviewSheet() {
  const { pending, close, confirm, switchSigner, agentAddress } = useActions();
  if (!pending) return null;
  const { prepared, signer, phase, outcome, buildError, type } = pending;
  const d = prepared?.described;
  const refused = buildError ? [buildError] : (prepared?.refusals ?? []);
  const agentPossible = !!agentAddress && AGENT_ALLOWED.includes(type);
  return (
    <Modal title={d?.title ?? 'Review'} onClose={close} locked={phase === 'signing'} eyebrow={phase === 'done' ? 'Result' : 'Review before signing'}>
      <div className={`review review-${d?.tone ?? 'neutral'}`} data-phase={phase}>
        {d && (
          <dl className="review-rows">
            {d.rows.map((r) => (
              <div key={r.label} className={r.emphasis ? 'review-row emphasis' : 'review-row'}>
                <dt>{r.label}</dt>
                <dd className="num">{r.value}</dd>
              </div>
            ))}
          </dl>
        )}
        {pending.estimate && pending.estimate.length > 0 && phase !== 'done' && (
          <dl className="review-estimate" aria-label="Estimates, not signed">
            {pending.estimate.map((r) => (
              <div key={r.label}>
                <dt>{r.label}</dt>
                <dd className="num">{r.value}</dd>
              </div>
            ))}
          </dl>
        )}
        {d && d.notes.length > 0 && (
          <ul className="review-notes">
            {d.notes.map((n) => (
              <li key={n}>{n}</li>
            ))}
          </ul>
        )}
        {phase !== 'done' && refused.length > 0 && (
          <div className="callout callout-danger" role="alert">
            <strong>Signing refused</strong>
            <ul>
              {refused.map((r) => (
                <li key={r}>{r}</li>
              ))}
            </ul>
          </div>
        )}
        {phase !== 'done' && signer && (
          <div className="review-signer">
            <span className="eyebrow">Signed by</span>
            <span className="num">{signer.kind === 'agent' ? `Agent key ${shortAddress(signer.address)} on this device (no wallet prompt)` : `Your wallet ${shortAddress(signer.address)}`}</span>
            {agentPossible && phase === 'review' && (
              <button type="button" className="link-button" onClick={() => switchSigner(signer.kind !== 'agent')}>
                {signer.kind === 'agent' ? 'Sign with my wallet instead' : 'Use the agent key'}
              </button>
            )}
          </div>
        )}
        {prepared && (
          <details className="review-raw">
            <summary>Exactly what is signed (EIP-712)</summary>
            <pre className="num">{JSON.stringify({ domain: prepared.data.domain, primaryType: prepared.data.primaryType, message: prepared.data.message }, null, 2)}</pre>
          </details>
        )}
        {outcome && (
          <div className={`callout ${outcome.ok ? 'callout-ok' : 'callout-danger'}`} role={outcome.ok ? 'status' : 'alert'}>
            <strong>{outcome.title}</strong>
            <p>{outcome.text}</p>
          </div>
        )}
        <div className="review-actions">
          {phase === 'done' ? (
            <button type="button" className="button button-primary" onClick={close} autoFocus>
              Close
            </button>
          ) : (
            <>
              <button type="button" className="button button-quiet" onClick={close} disabled={phase === 'signing'}>
                Cancel
              </button>
              <button
                type="button"
                className={`button button-primary tone-${d?.tone ?? 'neutral'}`}
                onClick={() => void confirm()}
                disabled={phase === 'signing' || refused.length > 0 || !prepared}
                autoFocus
              >
                {phase === 'signing' ? (signer?.kind === 'agent' ? 'Signing and sending…' : 'Confirm in your wallet…') : signer?.kind === 'agent' ? 'Sign with agent key and send' : 'Sign in wallet and send'}
              </button>
            </>
          )}
        </div>
      </div>
    </Modal>
  );
}
