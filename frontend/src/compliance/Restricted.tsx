/**
 * The screen a visitor from a restricted region (or one whose region cannot be confirmed) gets in place of the order
 * form, and once, on arrival, in place of the page. UNAUDITED prototype; wording is a draft for counsel.
 *
 * It never traps funds: cancelling, reducing, withdrawing, revoking keys and the L2 exit stay reachable from it
 * (docs/API.md 6 "exit-only"). The decision itself is the exchange's (GET /v1/geo); this screen only shows it. A
 * sanctioned address gets the same screen without the trading exits the exchange refuses.
 */
import { ShieldAlert } from 'lucide-react';
import { go } from '../state/session';
import { restrictedText, RESTRICTED_SUMMARY } from './notices';
import type { WireGeo } from '../lib/types';

/**
 * 'unreachable': the exchange did not answer the region question at all (API down, offline). That is not a verdict
 * about the visitor: no restricted screen is shown for it; the banner says the region could not be determined and the
 * signing guard allows only exits, as before. 'unknown': the exchange answered and could not place the visitor.
 */
export type GeoVerdict = 'pending' | 'open' | 'restricted' | 'unknown' | 'sanctioned' | 'unreachable';
export function geoVerdict(geo: WireGeo | null, read: 'pending' | 'ok' | 'failed'): GeoVerdict {
  if (read === 'pending') return 'pending';
  if (!geo) return 'unreachable';
  if (geo.sanctioned) return 'sanctioned';
  if (geo.restricted || geo.policy !== 'open') return geo.country ? 'restricted' : 'unknown';
  return 'open';
}

export function RestrictedScreen({ verdict, country, onContinue }: { verdict: Exclude<GeoVerdict, 'pending' | 'open' | 'unreachable'>; country: string | null; onContinue?: () => void }) {
  const sanctioned = verdict === 'sanctioned';
  return (
    <section className="restricted" aria-labelledby="restricted-title">
      <ShieldAlert size={40} aria-hidden="true" className="restricted-icon" />
      <p className="eyebrow">{sanctioned ? 'Not available' : verdict === 'unknown' ? 'Region not confirmed' : 'Not available in your region'}</p>
      <h1 className="display" id="restricted-title">
        {sanctioned ? 'This address cannot use PosternDEX Perpetuals' : verdict === 'unknown' ? 'Trading is off until your region is confirmed' : 'PosternDEX Perpetuals is not offered where you are'}
      </h1>
      <p className="lead">{sanctioned ? 'The exchange refuses every action from this address.' : restrictedText(verdict === 'unknown' ? null : country)}</p>
      {!sanctioned && (
        <>
          <div className="restricted-cols">
            <div>
              <h2>What is off</h2>
              <ul>
                <li>Opening or growing a position</li>
                <li>Changing leverage or adding margin</li>
                <li>Approving a new agent key</li>
                <li>New deposits through this interface</li>
              </ul>
            </div>
            <div>
              <h2>What always stays open</h2>
              <ul>
                <li>Cancelling orders and closing or reducing positions</li>
                <li>Withdrawing your collateral</li>
                <li>Revoking agent keys</li>
                <li>The exit on the Bloch L2, which no region rule can block</li>
              </ul>
            </div>
          </div>
          <div className="notfound-actions">
            {onContinue && (
              <button type="button" className="button button-primary" onClick={onContinue}>
                Continue to manage my account
              </button>
            )}
            <button type="button" className="button" onClick={() => go({ page: 'transfers', tab: 'withdraw' })}>
              Withdraw
            </button>
            <button type="button" className="button" onClick={() => go({ page: 'transfers', tab: 'exit' })}>
              Exit without the exchange
            </button>
          </div>
        </>
      )}
      <details className="restricted-list">
        <summary>Where it is not available</summary>
        <p>{RESTRICTED_SUMMARY}</p>
      </details>
      <p className="fine">
        The list and its wording are drafts for counsel.{' '}
        <button type="button" className="link-button" onClick={() => go({ page: 'legal' })}>
          Notices and terms
        </button>
      </p>
    </section>
  );
}
