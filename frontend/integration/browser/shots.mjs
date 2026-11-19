#!/usr/bin/env node
// Screenshots only, for design iterations: a list of addresses at a list of viewports, both themes on request.
// No assertion and no signature (the injected wallet is watch-only and declines everything); the acceptance harness
// is screens.mjs. UNAUDITED prototype, loopback only.
//
//   node integration/browser/shots.mjs --app http://127.0.0.1:49811/ --out DIR [--port 49853]
//        [--routes "#/,#/trade/BTC-USD"] [--sizes "1440x900,390x844m"] [--theme dark|light|both] [--account 0x…]
//        [--no-wallet] [--wait 2500]
// A size ending in "m" is a phone (device scale 2, touch). Files: <route>-<size>-<theme>.png, viewport only.
import { mkdirSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import { launch, openPage } from './cdp.mjs';

const args = process.argv.slice(2);
const opt = (name, fallback) => {
  const i = args.indexOf(`--${name}`);
  return i >= 0 ? args[i + 1] : fallback;
};
const APP = opt('app', 'http://127.0.0.1:49811/');
const OUT = opt('out', './.shots');
const PORT = Number(opt('port', '49853'));
const ROUTES = opt('routes', '#/,#/trade/BTC-USD,#/markets,#/portfolio,#/transfers/deposit,#/transfers/exit,#/history/fills,#/funding/BTC-USD').split(',');
const SIZES = opt('sizes', '1440x900').split(',');
const THEMES = opt('theme', 'dark') === 'both' ? ['dark', 'light'] : [opt('theme', 'dark')];
const ACCOUNT = opt('account', '0x1111111111111111111111111111111111111111');
const WAIT = Number(opt('wait', '2500'));
const WALLET = !args.includes('--no-wallet');
const FULL = args.includes('--full');
const CHROME = opt('chrome', '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome');
mkdirSync(OUT, { recursive: true });

const initScript = WALLET
  ? String.raw`
(() => {
  const address = ${JSON.stringify(ACCOUNT)};
  const provider = {
    request: async ({ method }) => {
      if (method === 'eth_requestAccounts' || method === 'eth_accounts') return [address];
      if (method === 'eth_chainId') return '0xab162';
      throw Object.assign(new Error('watch-only wallet'), { code: 4001 });
    },
    on() {},
    removeListener() {},
  };
  const info = { uuid: '9c4f5e6d-7081-4c93-8dae-2f3a4b5c6d7e', name: 'Watch-only wallet', icon: '', rdns: 'local.watch' };
  const announce = () => window.dispatchEvent(new CustomEvent('eip6963:announceProvider', { detail: Object.freeze({ info, provider }) }));
  window.addEventListener('eip6963:requestProvider', announce);
  announce();
})();`
  : '';

const browser = await launch(CHROME, PORT);
const { browserContextId } = await browser.send('Target.createBrowserContext');
const page = await openPage(browser, browserContextId, { initScript, url: 'about:blank', onBinding: async () => {}, log: () => {} });
const name = (route) => route.replace(/^#\/?/, '').replace(/[^A-Za-z0-9]+/g, '-').replace(/^-|-$/g, '') || 'home';
let count = 0;
try {
  for (const theme of THEMES) {
    for (const size of SIZES) {
      const phone = size.endsWith('m');
      const [w, h] = size.replace('m', '').split('x').map(Number);
      await page.cdp('Emulation.setDeviceMetricsOverride', { width: w, height: h, deviceScaleFactor: phone ? 2 : 1, mobile: phone });
      await page.goto(APP);
      await page.evaluate(`localStorage.setItem('postern.theme.v1', ${JSON.stringify(theme)})`);
      for (const route of ROUTES) {
        await page.goto(`${APP}${route}`);
        await page.reload();
        if (WALLET && (await page.evaluate(`!!window.__h.button('Connect')`))) {
          await page.click('Connect');
          await page.click('Watch-only wallet').catch(() => {});
        }
        await delay(WAIT);
        const file = join(OUT, `${name(route)}-${size}-${theme}.png`);
        if (FULL) await page.screenshot(file);
        else {
          const { data } = await page.cdp('Page.captureScreenshot', { format: 'png' });
          writeFileSync(file, Buffer.from(data, 'base64'));
        }
        count++;
      }
    }
  }
} finally {
  await browser.close();
}
console.log(`${count} screenshots in ${OUT}`);
