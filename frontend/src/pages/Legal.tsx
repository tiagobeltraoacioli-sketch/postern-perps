/** Notices: operator, risk, restrictions, licences. Draft wording for counsel (compliance/notices.ts). */
import { DOCUMENTS, NOTICE_VERSION, OPERATOR_LINE, PROTOTYPE_LINE, RESTRICTED_SUMMARY, RISK_POINTS } from '../compliance/notices';

export function Legal() {
  return (
    <div className="page">
      <header className="page-head">
        <p className="eyebrow">Notices</p>
        <h1 className="display">Before you trade</h1>
        <p className="lead">{OPERATOR_LINE}</p>
        <p className="callout callout-warn">{PROTOTYPE_LINE}</p>
      </header>
      <section className="panel prose">
        <h2>Risk</h2>
        <ul>
          {RISK_POINTS.map((p) => (
            <li key={p}>{p}</li>
          ))}
        </ul>
      </section>
      <section className="panel prose">
        <h2>Where it is not available</h2>
        <p>{RESTRICTED_SUMMARY}</p>
        <p>Where your region cannot be determined, trading is off. Cancelling, reducing, withdrawing and revoking agent keys stay open, so funds can always leave.</p>
      </section>
      <section className="panel prose">
        <h2>Licences</h2>
        <p>
          Fonts: Inter and Fraunces under the SIL Open Font License 1.1 (<a href="./licenses/Inter-OFL-1.1.txt">Inter</a>, <a href="./licenses/Fraunces-OFL-1.1.txt">Fraunces</a>). Icons: Lucide (ISC). Libraries: React (MIT), viem (MIT). Marks and the price chart are drawn in house.
        </p>
      </section>
      <p className="fine">
        Notices {NOTICE_VERSION}; documents {Object.values(DOCUMENTS).map((d) => d.id).join(', ')}. Draft wording for counsel; not legal advice.
      </p>
    </div>
  );
}
