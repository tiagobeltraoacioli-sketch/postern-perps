/** The application shell: header, banners that must be seen, the page, the footer, the review sheet. */
import { useEffect, useRef, useState } from 'react';
import { Wallet, Copy, LogOut } from 'lucide-react';
import { feed, route as routeStore, go, eip712Problem, geo as geoStore, geoRead, startSession, type Route } from './state/session';
import { ActionProvider, useActions } from './state/actions';
import { useStore } from './lib/store';
import { shortAddress } from './lib/format';
import { LOCAL_RENDER } from './lib/config';
import { BrandMark } from './ui/BrandMark';
import { ThemeToggle } from './ui/ThemeToggle';
import { ReviewSheet } from './ui/ReviewSheet';
import { Toaster } from './ui/toasts';
import { TradePage } from './trade/TradePage';
import { Portfolio } from './pages/Portfolio';
import { Transfers } from './pages/Transfers';
import { Agents } from './pages/Agents';
import { System } from './pages/System';
import { Legal } from './pages/Legal';
import { OPERATOR_LINE, PROTOTYPE_LINE, restrictedText } from './compliance/notices';

const NAV: [Route['page'], string][] = [
  ['trade', 'Trade'],
  ['portfolio', 'Portfolio'],
  ['transfers', 'Transfers'],
  ['agents', 'Agent keys'],
  ['system', 'System'],
];

export function App() {
  useEffect(() => startSession(), []);
  return (
    <ActionProvider>
      <Shell />
    </ActionProvider>
  );
}

function Shell() {
  const route = useStore(routeStore);
  const { account } = useActions();
  useEffect(() => feed.follow(account), [account]);
  return (
    <div className="app" data-network="bloch-l2">
      <a className="skip-link" href="#main-content">
        Skip to content
      </a>
      <Header route={route} />
      <Banners />
      <main id="main-content" tabIndex={-1}>
        {route.page === 'trade' && <TradePage symbol={route.symbol} />}
        {route.page === 'portfolio' && <Portfolio />}
        {route.page === 'transfers' && <Transfers />}
        {route.page === 'agents' && <Agents />}
        {route.page === 'system' && <System />}
        {route.page === 'legal' && <Legal />}
      </main>
      <footer className="footer">
        <span>{OPERATOR_LINE}</span>
        <span>{PROTOTYPE_LINE}</span>
        <button type="button" className="link-button" onClick={() => go({ page: 'legal' })}>
          Notices, risk and licences
        </button>
      </footer>
      <ReviewSheet />
      <Toaster events={feed.events} />
    </div>
  );
}

function Header({ route }: { route: Route }) {
  const connection = useStore(feed.connection);
  const symbol = route.page === 'trade' ? route.symbol : 'BTC-USD';
  return (
    <header className="header">
      <button type="button" className="brand" onClick={() => go({ page: 'trade', symbol })} aria-label="PosternDEX Perpetuals, trade">
        <BrandMark size={30} />
        <span className="brand-name">PosternDEX</span>
        <span className="brand-sub">Perpetuals</span>
      </button>
      <nav className="nav" aria-label="Main">
        {NAV.map(([page, label]) => (
          <button key={page} type="button" aria-current={route.page === page ? 'page' : undefined} onClick={() => go(page === 'trade' ? { page, symbol } : { page } as Route)}>
            {label}
          </button>
        ))}
      </nav>
      <div className="header-tools">
        <span className={`conn conn-${connection.state}`} title={connection.latencyMs !== null ? `${connection.latencyMs} ms` : undefined}>
          <span className="conn-dot" aria-hidden="true" />
          {connection.state === 'open' ? 'Live' : connection.state === 'connecting' ? 'Connecting' : connection.state === 'offline' ? 'Offline' : 'Reconnecting'}
        </span>
        <ThemeToggle />
        <WalletButton />
      </div>
    </header>
  );
}

function WalletButton() {
  const { wallet, agentAddress } = useActions();
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!open) return;
    const away = (e: MouseEvent) => !ref.current?.contains(e.target as Node) && setOpen(false);
    document.addEventListener('mousedown', away);
    return () => document.removeEventListener('mousedown', away);
  }, [open]);
  const a = wallet.account;
  return (
    <div className="wallet" ref={ref}>
      <button type="button" className={`button ${a ? 'button-quiet' : 'button-primary'} wallet-button`} aria-expanded={open} onClick={() => setOpen((o) => !o)} disabled={wallet.pending}>
        <Wallet size={16} aria-hidden="true" />
        <span className="num">{wallet.pending ? 'Connecting…' : a ? shortAddress(a.address) : 'Connect'}</span>
        {agentAddress && <span className="agent-dot" title="An agent key on this device can sign orders" />}
      </button>
      {open && (
        <div className="wallet-menu" role="menu">
          {a ? (
            <>
              <p className="num wallet-full">{a.address}</p>
              <p className="fine">{a.name}</p>
              <button type="button" role="menuitem" onClick={() => void navigator.clipboard?.writeText(a.address)}>
                <Copy size={14} aria-hidden="true" /> Copy address
              </button>
              <button
                type="button"
                role="menuitem"
                onClick={() => {
                  wallet.disconnect();
                  setOpen(false);
                }}
              >
                <LogOut size={14} aria-hidden="true" /> Disconnect
              </button>
            </>
          ) : wallet.wallets.length ? (
            wallet.wallets.map((w) => (
              <button
                key={w.id}
                type="button"
                role="menuitem"
                onClick={() => {
                  setOpen(false);
                  void wallet.connect(w);
                }}
              >
                {w.name}
              </button>
            ))
          ) : (
            <p className="fine">No browser wallet found. Install or unlock one, then reload.</p>
          )}
          {wallet.error && <p className="msg-danger">{wallet.error}</p>}
        </div>
      )}
    </div>
  );
}

function Banners() {
  const problem = useStore(eip712Problem);
  const geo = useStore(geoStore);
  const read = useStore(geoRead);
  const restricted = read !== 'pending' && (!geo || geo.restricted || geo.policy !== 'open');
  return (
    <div className="banners">
      {LOCAL_RENDER && <p className="banner banner-local">Local render build against a loopback mock: not for publication.</p>}
      {problem && (
        <p className="banner banner-danger" role="alert">
          This interface is out of date with the exchange ({problem}). Signing is refused until it is updated.
        </p>
      )}
      {geo?.sanctioned ? (
        <p className="banner banner-danger" role="alert">
          This address cannot use PosternDEX Perpetuals.
        </p>
      ) : (
        restricted && (
          <p className="banner banner-warn" role="status">
            {restrictedText(geo?.country ?? null)}
          </p>
        )
      )}
    </div>
  );
}
