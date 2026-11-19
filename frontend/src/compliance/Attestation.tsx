/**
 * Before the first order: the risk notice and the eligibility statement, signed by the main wallet (397's typed
 * data, 392's text). The statement is shown in full, exactly as signed (its hash is checked in attestation.ts).
 * The record is kept on this device; how the operator receives and keeps it is 397's and counsel's (not yet an API).
 */
import { useState } from 'react';
import { useStore } from '../lib/store';
import { geo as geoStore, signingDomain } from '../state/session';
import { useActions } from '../state/actions';
import { walletSigner } from '../lib/sign';
import { walletError } from '../wallet/wallet';
import { RISK_POINTS } from './notices';
import { attestationMessage, keepAttestation, signAttestation, STATEMENT_DOCUMENT, STATEMENT_TEXT } from './eligibility';

export function Attestation({ account, onDone }: { account: string; onDone: () => void }) {
  const { wallet } = useActions();
  const geo = useStore(geoStore);
  const [country, setCountry] = useState(geo?.country && /^[A-Z]{2}$/.test(geo.country) ? geo.country : '');
  const [read, setRead] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const sign = async () => {
    const provider = wallet.provider;
    if (!provider) return;
    setBusy(true);
    setError('');
    try {
      const record = await signAttestation(walletSigner(account, (a) => provider.request(a)), attestationMessage(account, country, Date.now()), signingDomain.get());
      keepAttestation(record);
      onDone();
    } catch (e) {
      setError(e instanceof Error && !('code' in e) ? e.message : `${walletError(e)} Nothing was signed.`);
    } finally {
      setBusy(false);
    }
  };
  return (
    <div className="attestation">
      <h2 className="attestation-title">Before your first order</h2>
      <details open>
        <summary>Risk notice</summary>
        <ul>
          {RISK_POINTS.map((p) => (
            <li key={p}>{p}</li>
          ))}
        </ul>
      </details>
      <div className="statement">
        <span className="eyebrow">Statement you sign ({STATEMENT_DOCUMENT})</span>
        <pre>{STATEMENT_TEXT}</pre>
      </div>
      <label className="field field-inline">
        <span>Country of residence</span>
        <input className="country" value={country} maxLength={2} onChange={(e) => setCountry(e.target.value.toUpperCase().replace(/[^A-Z]/g, ''))} placeholder="PA" aria-describedby="country-hint" />
      </label>
      <p className="fine" id="country-hint">
        Two-letter code. A country where the exchange is restricted leaves you exit-only.
      </p>
      <label className="check">
        <input type="checkbox" checked={read} onChange={(e) => setRead(e.target.checked)} />
        <span>I have read the risk notice and the statement above, and they are true for me.</span>
      </label>
      {error && <p className="msg-danger">{error}</p>}
      <button type="button" className="button button-block button-primary" disabled={!read || country.length !== 2 || busy || !wallet.provider} onClick={() => void sign()}>
        {busy ? 'Confirm in your wallet…' : 'Sign the statement in your wallet'}
      </button>
      <p className="fine">A signature moves no funds and is valid for one year. Draft wording for counsel. Kept on this device.</p>
    </div>
  );
}
