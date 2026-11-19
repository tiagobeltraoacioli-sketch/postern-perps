#!/usr/bin/env node
// The production build as published today (config/production.json: vault null, API not deployed) must fail closed:
// "Not live", markets unreadable said so, nothing to sign, no script error. Serves <dist> on loopback itself.
//   node integration/browser/production.mjs <dist> <out-dir> [port]
import { createServer } from 'node:http';
import { existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { extname, join, normalize } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import { launch, openPage } from './cdp.mjs';

const [dist, out, portArg] = process.argv.slice(2);
const port = Number(portArg ?? 49112);
mkdirSync(out, { recursive: true });
const types = { '.html': 'text/html', '.js': 'text/javascript', '.css': 'text/css', '.woff2': 'font/woff2', '.svg': 'image/svg+xml', '.txt': 'text/plain' };
const server = createServer((req, res) => {
  const path = normalize(decodeURIComponent(new URL(req.url, 'http://x').pathname)).replace(/^\/+/, '');
  const file = join(dist, path || 'index.html');
  if (!file.startsWith(normalize(dist)) || !existsSync(file)) return res.writeHead(404).end();
  res.writeHead(200, { 'content-type': types[extname(file)] ?? 'application/octet-stream' });
  res.end(readFileSync(file));
}).listen(port, '127.0.0.1');
const browser = await launch('/Applications/Google Chrome.app/Contents/MacOS/Google Chrome', port + 40);
const failures = [];
try {
  const { browserContextId } = await browser.send('Target.createBrowserContext');
  const page = await openPage(browser, browserContextId, { initScript: '', url: `http://127.0.0.1:${port}/#/trade/BTC-USD` });
  await page.cdp('Emulation.setDeviceMetricsOverride', { width: 1440, height: 900, deviceScaleFactor: 1, mobile: false });
  await page.waitVisibleText(/Not live: the exchange's vault is not deployed/, 20_000);
  await page.waitVisibleText(/Markets could not be read/, 20_000);
  const text = await page.visibleText();
  for (const p of [/signs nothing/, /Nothing can be signed until the markets are read/, /Bloch Inc \(Panama\)/]) if (!p.test(text)) failures.push(`missing ${p}`);
  const errors = page.console.filter((l) => /^pageerror/.test(l));
  if (errors.length) failures.push(`page errors: ${errors.join(' | ')}`);
  await delay(300);
  await page.screenshot(join(out, '18-production-not-live.png'));
  await page.goto(`http://127.0.0.1:${port}/#/transfers`);
  // The page is its own chunk since design pass 3: wait for the page's own words (the footer also says "Not live"),
  // then assert the same sentence as before.
  await page.waitVisibleText(/The vault is not deployed/, 10_000).catch(() => {});
  await page.screenshot(join(out, '19-production-transfers-not-live.png'));
  // Round 2: with no vault in the published configuration the deposit and the exit build nothing either, and the
  // page still explains the exit; an unknown address is the 404 page, not another page.
  if (!/The vault is not deployed\. This page builds no transaction and asks your wallet for nothing\./.test(await page.visibleText())) failures.push('deposit tab does not say it builds nothing');
  await page.goto(`http://127.0.0.1:${port}/#/transfers/exit`);
  await page.waitVisibleText(/Your funds can always leave, even if the exchange is silent/, 10_000);
  if (!/The vault is not deployed/.test(await page.visibleText())) failures.push('exit tab is not marked not live');
  if (await page.evaluate(`!!window.__h.button('Review request')`)) failures.push('the exit tab offers a request without a vault');
  await page.screenshot(join(out, '19b-production-exit-not-live.png'));
  await page.goto(`http://127.0.0.1:${port}/#/nowhere`);
  await page.waitVisibleText(/No gate at this address/, 10_000);
} catch (e) {
  failures.push(e.message);
} finally {
  await browser.close();
  server.close();
}
writeFileSync(join(out, 'production-results.json'), JSON.stringify({ failures }, null, 2));
console.log(`production fail-closed: ${failures.length ? 'FAIL' : 'PASS'}`);
for (const f of failures) console.log(`FAIL ${f}`);
process.exit(failures.length ? 1 : 0);
