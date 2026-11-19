/** An address this application does not have. It says so and offers the ways back; it never shows another page in its place. */
import { go } from '../state/session';
import { GateGlyph } from '../ui/BrandMark';

export function NotFound({ path }: { path: string }) {
  return (
    <div className="page-message notfound">
      <GateGlyph size={56} />
      <p className="eyebrow">404</p>
      <h1 className="display">No gate at this address</h1>
      <p className="lead">
        There is no page at <code className="num">#/{path}</code>. The link may be mistyped or from an older version.
      </p>
      <div className="notfound-actions">
        <button type="button" className="button button-primary" onClick={() => go({ page: 'trade', symbol: 'BTC-USD' })}>
          Go to trading
        </button>
        <button type="button" className="button" onClick={() => go({ page: 'markets' })}>
          All markets
        </button>
        <button type="button" className="button" onClick={() => go({ page: 'transfers', tab: 'deposit' })}>
          Funds
        </button>
      </div>
    </div>
  );
}
