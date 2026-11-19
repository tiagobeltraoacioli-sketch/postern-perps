import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import './styles/tokens.css';
import './styles/app.css';
import { App } from './App';
import { LOCAL_RENDER } from './lib/config';
import { feed } from './state/session';
import { feedStats } from './lib/feed';

// Local render builds only (loopback mock): the harness reads the feed and its frame statistics. The chart is not
// imported here (it belongs to the terminal's chunk, not to the first page): its statistics are attached when the
// chart module has loaded.
if (LOCAL_RENDER) {
  const perps: Record<string, unknown> = { feed, feedStats, chartStats: null };
  (window as unknown as Record<string, unknown>).__perps = perps;
  void import('./chart').then((m) => (perps.chartStats = m.chartStats));
}

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
