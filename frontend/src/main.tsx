import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import './styles/tokens.css';
import './styles/app.css';
import { App } from './App';
import { LOCAL_RENDER } from './lib/config';
import { feed } from './state/session';
import { feedStats } from './lib/feed';

// Local render builds only (loopback mock): the harness reads the feed and its frame statistics.
if (LOCAL_RENDER) (window as unknown as Record<string, unknown>).__perps = { feed, feedStats };

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
