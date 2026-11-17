/**
 * Agent (API) keys: approve a key generated in this browser so orders are signed without a wallet prompt, list the
 * account's approved keys, revoke them. Approval and revocation are signed by the main wallet only.
 */
import { useState } from 'react';
import { feed } from '../state/session';
import { useStore } from '../lib/store';
import { useActions } from '../state/actions';
import { agentFor, createAgent, forgetAgent, keepAgent, type AgentRecord } from '../lib/agent';
import { formatDateTime, shortAddress } from '../lib/format';
import { MAX_AGENT_MS } from '../lib/sign';

const DAYS = [1, 7, 30, 90, 180];

export function Agents() {
  const { account, request, refreshAgent, agentAddress } = useActions();
  const state = useStore(feed.account);
  const [name, setName] = useState('this browser');
  const [days, setDays] = useState(7);
  const [persist, setPersist] = useState(false);
  const [draft, setDraft] = useState<AgentRecord | null>(null);
  const local = account ? agentFor(account) : null;
  const nameOk = /^[\x20-\x7e]{1,32}$/.test(name);

  const approve = () => {
    if (!account) return;
    const expiresAt = Date.now() + Math.min(days * 86_400_000, Number(MAX_AGENT_MS));
    const record = createAgent(account, name, expiresAt);
    setDraft(record);
    request(
      'ApproveAgent',
      (nonce, acct) => ({ account: acct, nonce: nonce.toString(), agent: record.agent, name, expiresAt: String(expiresAt) }),
      (o) => {
        if (o.ok) {
          keepAgent(record, persist);
          refreshAgent();
        }
        setDraft(null);
      },
    );
  };
  const revoke = (agent: string) =>
    request('RevokeAgent', (nonce, acct) => ({ account: acct, nonce: nonce.toString(), agent }), (o) => {
      if (o.ok && local?.agent === agent && account) {
        forgetAgent(account);
        refreshAgent();
      }
    });

  return (
    <div className="page">
      <header className="page-head">
        <p className="eyebrow">Agent keys</p>
        <h1 className="display">Trade without a wallet prompt</h1>
        <p className="lead">An agent key is a second key, made in this browser, that your wallet approves for a limited time. It can place and cancel orders and change leverage and isolated margin. It can never withdraw, never change where funds go and never approve another key.</p>
      </header>
      {!account ? (
        <p className="page-message">Connect a wallet to manage agent keys.</p>
      ) : (
        <div className="two-col">
          <section className="panel" aria-label="New agent key">
            <header className="panel-head">
              <h2>{local ? 'This device has a key' : 'Approve a key for this device'}</h2>
            </header>
            {local ? (
              <div className="dialog-body">
                <p className="num">
                  {shortAddress(local.agent)} · “{local.name}” · until {formatDateTime(local.expiresAt)} UTC · {local.persisted ? 'kept on this device' : 'this tab only'}
                </p>
                <div className="review-actions">
                  <button type="button" className="button button-quiet" onClick={() => { forgetAgent(account); refreshAgent(); }}>
                    Forget on this device
                  </button>
                  <button type="button" className="button button-danger" onClick={() => revoke(local.agent)}>
                    Revoke
                  </button>
                </div>
                <p className="fine">Forgetting removes the key here; the approval stays until it expires or you revoke it.</p>
              </div>
            ) : (
              <div className="dialog-body">
                <label className="field">
                  <span>Name (shown in your list)</span>
                  <input value={name} maxLength={32} onChange={(e) => setName(e.target.value)} />
                </label>
                {!nameOk && <p className="msg-danger">1 to 32 plain characters.</p>}
                <label className="field">
                  <span>Valid for</span>
                  <select value={days} onChange={(e) => setDays(Number(e.target.value))}>
                    {DAYS.map((d) => (
                      <option key={d} value={d}>
                        {d === 1 ? '1 day' : `${d} days`}
                      </option>
                    ))}
                  </select>
                </label>
                <label className="check">
                  <input type="checkbox" checked={persist} onChange={(e) => setPersist(e.target.checked)} />
                  <span>Keep the key on this device after the tab closes. Anyone who can use this browser profile could then trade this account until the key expires (never withdraw).</span>
                </label>
                <button type="button" className="button button-block button-primary" disabled={!nameOk || !!draft} onClick={approve}>
                  Review approval
                </button>
                <p className="fine">The key is generated here and never leaves this browser. The exchange sees only its address.</p>
              </div>
            )}
          </section>
          <section className="panel" aria-label="Approved keys">
            <header className="panel-head">
              <h2>Approved keys</h2>
            </header>
            <ul className="records">
              {(state?.agents ?? []).map((a) => (
                <li key={a.agent}>
                  <span className="num rec-amount">{shortAddress(a.agent)}</span>
                  <span className="badge">{a.agent.toLowerCase() === agentAddress ? 'this device' : 'elsewhere'}</span>
                  <span className="fine">“{a.name}” · approved {formatDateTime(a.approvedAt)} · until {formatDateTime(a.expiresAt)} UTC</span>
                  <button type="button" className="button button-small button-quiet" onClick={() => revoke(a.agent)}>
                    Revoke
                  </button>
                </li>
              ))}
              {state && state.agents.length === 0 && <li className="empty">No approved keys.</li>}
            </ul>
          </section>
        </div>
      )}
    </div>
  );
}
