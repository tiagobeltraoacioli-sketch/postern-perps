/** The application shell: header, banners that must be seen, the page, the footer, the two review sheets. */
import { useEffect, useRef, useState } from 'react';
import { Wallet, Copy, LogOut, CandlestickChart, PieChart, ArrowLeftRight, KeyRound, Activity, LayoutGrid, History as HistoryIcon, Percent, MoreHorizontal, ScrollText, TriangleAlert, type LucideIcon } from 'lucide-react';
import { feed, route as routeStore, go, signingDomain, eip712Problem, geo as geoStore, geoRead, startSession, type Route } from './state/session';
import { ActionProvider, useActions } from './state/actions';
import { deployment, FundsProvider, useFunds, useProofKeeper, useVault } from './state/funds';
import { useStore } from './lib/store';
import { shortAddress } from './lib/format';
import { config, LOCAL_RENDER } from './lib/config';
import { BrandMark } from './ui/BrandMark';
import { ThemeToggle } from './ui/ThemeToggle';
import { ReviewSheet } from './ui/ReviewSheet';
import { TxSheet } from './ui/TxSheet';
import { Modal } from './ui/Modal';
import { Toaster } from './ui/toasts';
import { TradePage } from './trade/TradePage';
import { Markets } from './pages/Markets';
import { Portfolio } from './pages/Portfolio';
import { Funds } from './pages/Funds';
import { History } from './pages/History';
import { Funding } from './pages/Funding';
import { Agents } from './pages/Agents';
import { System } from './pages/System';
import { Legal } from './pages/Legal';
import { NotFound } from './pages/NotFound';
import { OPERATOR_LINE, PROTOTYPE_LINE, restrictedText } from './compliance/notices';
import { geoVerdict, RestrictedScreen } from './compliance/Restricted';

type Page = Route['page'];
type NavItem = { page: Page; label: string; short: string; icon: LucideIcon };
const PRIMARY: NavItem[] = [
  { page: 'trade', label: 'Trade', short: 'Trade', icon: CandlestickChart },
  { page: 'markets', label: 'Markets', short: 'Markets', icon: LayoutGrid },
  { page: 'portfolio', label: 'Portfolio', short: 'Portfolio', icon: PieChart },
  { page: 'transfers', label: 'Funds', short: 'Funds', icon: ArrowLeftRight },
];
/** Shown inline on a wide header, inside "More" on a narrow one. */
const SECONDARY: NavItem[] = [
  { page: 'history', label: 'History', short: 'History', icon: HistoryIcon },
  { page: 'funding', label: 'Funding', short: 'Funding', icon: Percent },
];
const MORE: NavItem[] = [
  { page: 'agents', label: 'Agent keys', short: 'Keys', icon: KeyRound },
  { page: 'system', label: 'System', short: 'System', icon: Activity },
  { page: 'legal', label: 'Notices and risk', short: 'Notices', icon: ScrollText },
];

function routeFor(page: Page, symbol: string): Route {
  if (page === 'trade') return { page, symbol };
  if (page === 'funding') return { page, symbol };
  if (page === 'transfers') return { page, tab: 'deposit' };
  if (page === 'history') return { page, tab: 'orders' };
  if (page === 'notfound') return { page, path: '' };
  return { page };
}

export function App() {
  useEffect(() => startSession(), []);
  return (
    <ActionProvider>
      <FundsProvider>
        <Shell />
      </FundsProvider>
    </ActionProvider>
  );
}

function Shell() {
  const route = useStore(routeStore);
  const { account } = useActions();
  const geo = useStore(geoStore);
  const read = useStore(geoRead);
  const verdict = geoVerdict(geo, read);
  // The restricted screen is shown once per visit in place of the trading pages; "continue" leaves the account in
  // exit-only mode (the order form says so and the signing guard refuses everything else).
  const [acknowledged, setAcknowledged] = useState(false);
  useEffect(() => feed.follow(account), [account]);
  // On every page, while a wallet is connected: keep the account's exit proof for the vault's latest checkpoint on
  // this device (R436-11 c). A slow read of the vault is enough; the Funds page reads it faster while open.
  const vault = useVault(deployment ? account : null, 60_000);
  useProofKeeper(vault.view, account);
  useEffect(() => {
    const titles: Partial<Record<Page, string>> = { markets: 'Markets', portfolio: 'Portfolio', transfers: 'Funds', history: 'History', funding: 'Funding', agents: 'Agent keys', system: 'System', legal: 'Notices', notfound: 'Not found' };
    if (route.page !== 'trade') document.title = `${titles[route.page]} · PosternDEX Perpetuals`;
  }, [route.page]);
  const blocked = (verdict === 'restricted' || verdict === 'unknown' || verdict === 'sanctioned') && (route.page === 'trade' || route.page === 'markets') && (verdict === 'sanctioned' || !acknowledged);
  return (
    <div className="app" data-network="bloch-l2" data-page={route.page}>
      <a className="skip-link" href="#main-content">
        Skip to content
      </a>
      <Header route={route} />
      <Banners quiet={blocked} />
      <main id="main-content" tabIndex={-1}>
        {blocked ? (
          <RestrictedScreen verdict={verdict as 'restricted' | 'unknown' | 'sanctioned'} country={geo?.country ?? null} onContinue={verdict === 'sanctioned' ? undefined : () => setAcknowledged(true)} />
        ) : (
          <>
            {route.page === 'trade' && <TradePage symbol={route.symbol} />}
            {route.page === 'markets' && <Markets />}
          </>
        )}
        {route.page === 'portfolio' && <Portfolio />}
        {route.page === 'transfers' && <Funds tab={route.tab} />}
        {route.page === 'history' && <History tab={route.tab} />}
        {route.page === 'funding' && <Funding symbol={route.symbol} />}
        {route.page === 'agents' && <Agents />}
        {route.page === 'system' && <System />}
        {route.page === 'legal' && <Legal />}
        {route.page === 'notfound' && <NotFound path={route.path} />}
      </main>
      <footer className="footer">
        <span>{OPERATOR_LINE}</span>
        <span>{PROTOTYPE_LINE}</span>
        <button type="button" className="link-button" onClick={() => go({ page: 'legal' })}>
          Notices, risk and licences
        </button>
      </footer>
      <ReviewSheet />
      <TxSheet />
      <Toaster events={feed.events} />
    </div>
  );
}

function Header({ route }: { route: Route }) {
  const connection = useStore(feed.connection);
  const symbol = route.page === 'trade' || route.page === 'funding' ? route.symbol : 'BTC-USD';
  const [more, setMore] = useState(false);
  const moreRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!more) return;
    const away = (e: MouseEvent) => !moreRef.current?.contains(e.target as Node) && setMore(false);
    const key = (e: KeyboardEvent) => e.key === 'Escape' && setMore(false);
    document.addEventListener('mousedown', away);
    document.addEventListener('keydown', key);
    return () => {
      document.removeEventListener('mousedown', away);
      document.removeEventListener('keydown', key);
    };
  }, [more]);
  const item = ({ page, label, short, icon: Icon }: NavItem, extra = '') => (
    <button key={page} type="button" className={extra} aria-current={route.page === page ? 'page' : undefined} aria-label={label} onClick={() => (setMore(false), go(routeFor(page, symbol)))}>
      <Icon className="nav-icon" size={18} aria-hidden="true" />
      <span className="nav-long">{label}</span>
      <span className="nav-short" aria-hidden="true">
        {short}
      </span>
    </button>
  );
  // "More" is marked current for a page inside it: always for MORE, and for SECONDARY only where those are inside
  // it (a narrow header; the CSS decides).
  const moreClass = MORE.some((n) => n.page === route.page) ? 'nav-more-current' : SECONDARY.some((n) => n.page === route.page) ? 'nav-more-current-narrow' : '';
  return (
    <header className="header">
      <button type="button" className="brand" onClick={() => go({ page: 'trade', symbol })} aria-label="PosternDEX Perpetuals, trade">
        <BrandMark size={30} />
        <span className="brand-name">PosternDEX</span>
        <span className="brand-sub">Perpetuals</span>
      </button>
      <nav className="nav" aria-label="Main">
        {PRIMARY.map((n) => item(n))}
        {SECONDARY.map((n) => item(n, 'nav-wide'))}
        <div className="nav-more" ref={moreRef}>
          <button type="button" aria-expanded={more} aria-haspopup="menu" aria-label="More pages" className={moreClass} onClick={() => setMore((o) => !o)}>
            <MoreHorizontal className="nav-icon" size={18} aria-hidden="true" />
            <span className="nav-long">More</span>
            <span className="nav-short" aria-hidden="true">
              More
            </span>
          </button>
          {more && (
            <div className="nav-menu" role="menu">
              {SECONDARY.map((n) => item(n, 'nav-narrow'))}
              {MORE.map((n) => item(n))}
            </div>
          )}
        </div>
      </nav>
      <div className="header-tools">
        <span className={`conn conn-${connection.state}`} title={connection.latencyMs !== null ? `${connection.latencyMs} ms` : undefined}>
          <span className="conn-dot" aria-hidden="true" />
          <span className="conn-text">{connection.state === 'open' ? 'Live' : connection.state === 'connecting' ? 'Connecting' : connection.state === 'offline' ? 'Offline' : 'Reconnecting'}</span>
        </span>
        <ThemeToggle />
        <WalletButton />
      </div>
    </header>
  );
}

function WalletButton() {
  const { wallet, agentAddress } = useActions();
  const { onL2, switchNetwork, switching, networkError } = useFunds();
  const [open, setOpen] = useState(false);
  const [choosing, setChoosing] = useState(false);
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!open) return;
    const away = (e: MouseEvent) => !ref.current?.contains(e.target as Node) && setOpen(false);
    const key = (e: KeyboardEvent) => e.key === 'Escape' && setOpen(false);
    document.addEventListener('mousedown', away);
    document.addEventListener('keydown', key);
    return () => {
      document.removeEventListener('mousedown', away);
      document.removeEventListener('keydown', key);
    };
  }, [open]);
  const a = wallet.account;
  useEffect(() => {
    if (a) setChoosing(false);
  }, [a]);
  const wrong = !!a && !onL2;
  return (
    <div className="wallet" ref={ref}>
      <button
        type="button"
        className={`button ${a ? (wrong ? 'button-warn' : 'button-quiet') : 'button-primary'} wallet-button`}
        aria-expanded={a ? open : choosing}
        onClick={() => (a ? setOpen((o) => !o) : setChoosing(true))}
        disabled={wallet.pending}
      >
        {wrong ? <TriangleAlert size={16} aria-hidden="true" /> : <Wallet size={16} aria-hidden="true" />}
        <span className="num">{wallet.pending ? 'Connecting…' : a ? (wrong ? 'Wrong network' : shortAddress(a.address)) : 'Connect'}</span>
        {agentAddress && !wrong && <span className="agent-dot" title="An agent key on this device can sign orders" />}
      </button>
      {open && a && (
        <div className="wallet-menu" role="menu">
          <p className="num wallet-full">{a.address}</p>
          <p className="fine">{a.name}</p>
          <p className={`wallet-net ${wrong ? 'wallet-net-wrong' : ''}`}>
            <span className="conn-dot" aria-hidden="true" />
            {wrong ? `On chain ${safeChain(a.chainId)}, not the Bloch L2` : `Bloch L2 (chain ${config.l2ChainId})`}
          </p>
          {wrong && (
            <>
              <p className="fine">Signing and vault transactions need the wallet on the Bloch L2. The page adds the network if the wallet does not know it.</p>
              <button type="button" role="menuitem" className="menu-primary" onClick={() => void switchNetwork()} disabled={switching}>
                {switching ? 'Check your wallet…' : 'Switch to Bloch L2'}
              </button>
              {networkError && <p className="msg-danger">{networkError}</p>}
            </>
          )}
          {agentAddress && (
            <p className="fine">
              Agent key <span className="num">{shortAddress(agentAddress)}</span> on this device signs orders without a wallet prompt.
            </p>
          )}
          <button type="button" role="menuitem" onClick={() => void navigator.clipboard?.writeText(a.address)}>
            <Copy size={14} aria-hidden="true" /> Copy address
          </button>
          <button type="button" role="menuitem" onClick={() => (setOpen(false), go({ page: 'agents' }))}>
            <KeyRound size={14} aria-hidden="true" /> Agent keys
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
        </div>
      )}
      {choosing && !a && (
        <Modal title="Connect a wallet" eyebrow="Your keys stay in your wallet" onClose={() => setChoosing(false)}>
          <div className="connect">
            {wallet.wallets.length ? (
              <ul className="connect-list">
                {wallet.wallets.map((w) => (
                  <li key={w.id}>
                    <button type="button" className="connect-option" onClick={() => void wallet.connect(w)} disabled={wallet.pending}>
                      <Wallet size={18} aria-hidden="true" />
                      <strong>{w.name}</strong>
                      <span className="fine">{w.rdns ?? 'Browser wallet'}</span>
                    </button>
                  </li>
                ))}
              </ul>
            ) : (
              <p className="callout callout-warn">No browser wallet found. Install or unlock one, then reload this page.</p>
            )}
            {wallet.error && <p className="msg-danger">{wallet.error}</p>}
            <ul className="review-notes">
              <li>Connecting only shows your address to this page. It moves nothing and signs nothing.</li>
              <li>Every order, withdrawal and vault transaction is shown in words first, and your wallet asks you again.</li>
              <li>Wallet names above are what each wallet says about itself; check you chose the one you meant.</li>
            </ul>
          </div>
        </Modal>
      )}
    </div>
  );
}

const safeChain = (hex: string) => {
  try {
    return BigInt(hex).toString();
  } catch {
    return 'unknown';
  }
};

function Banners({ quiet }: { quiet: boolean }) {
  const domain = useStore(signingDomain);
  const problem = useStore(eip712Problem);
  const geo = useStore(geoStore);
  const read = useStore(geoRead);
  const restricted = read !== 'pending' && (!geo || geo.restricted || geo.policy !== 'open');
  return (
    <div className="banners">
      {LOCAL_RENDER && <p className="banner banner-local">Local build against a loopback mock or stack: not for publication.</p>}
      {!domain && (
        <p className="banner banner-warn" role="status">
          Not live: the exchange's vault is not deployed, so this page signs nothing. Prices and accounts shown are read only.
        </p>
      )}
      {problem && (
        <p className="banner banner-danger" role="alert">
          This interface is out of date with the exchange ({problem}). Signing is refused until it is updated.
        </p>
      )}
      {/* The restricted screen says the same in full: the banner is for every other page. */}
      {!quiet &&
        (geo?.sanctioned ? (
          <p className="banner banner-danger" role="alert">
            This address cannot use PosternDEX Perpetuals.
          </p>
        ) : (
          restricted && (
            <p className="banner banner-warn" role="status">
              {restrictedText(geo?.country ?? null)}
            </p>
          )
        ))}
    </div>
  );
}
