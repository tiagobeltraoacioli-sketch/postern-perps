/**
 * The user's acknowledgement before the first order: the risk notice and the eligibility statements. Recorded
 * LOCALLY with the notice version (compliance/notices.ts). Whether it must also be signed and recorded by the
 * operator is for agents 392 / 397 and counsel; this interface does not decide it.
 */
import { useState } from 'react';
import { ATTESTATION_POINTS, NOTICE_VERSION, RISK_POINTS, writeAck } from './notices';

export function Attestation({ account, onDone }: { account: string; onDone: () => void }) {
  const [checked, setChecked] = useState<boolean[]>(ATTESTATION_POINTS.map(() => false));
  const all = checked.every(Boolean);
  return (
    <div className="attestation">
      <h3>Before your first order</h3>
      <details open>
        <summary>Risk notice</summary>
        <ul>
          {RISK_POINTS.map((p) => (
            <li key={p}>{p}</li>
          ))}
        </ul>
      </details>
      {ATTESTATION_POINTS.map((p, i) => (
        <label key={p} className="check">
          <input type="checkbox" checked={checked[i]} onChange={(e) => setChecked((c) => c.map((v, j) => (j === i ? e.target.checked : v)))} />
          <span>{p}</span>
        </label>
      ))}
      <button
        type="button"
        className="button button-block button-primary"
        disabled={!all}
        onClick={() => {
          writeAck(account);
          onDone();
        }}
      >
        I confirm
      </button>
      <p className="fine">Notice version {NOTICE_VERSION}. Draft wording for counsel. Kept on this device only.</p>
    </div>
  );
}
