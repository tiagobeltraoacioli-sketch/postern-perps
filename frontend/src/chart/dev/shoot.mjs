// Screenshots and frame measurements of the chart harness in headless Chrome (CDP on Node 24 built-ins; no
// dependency). SYNTHETIC data. Usage (from frontend/, after building the harness into <dir>):
//   node src/chart/dev/shoot.mjs <harness-dir> <out-dir> [screens|measure|all] [port]
// It serves <harness-dir> on 127.0.0.1:<port> (default 49600), launches Chrome with a temporary profile, and stops
// both before it exits. Results: PNGs in <out-dir>, measurements in <out-dir>/measure.json.
import { spawn } from 'node:child_process';
import { createServer } from 'node:http';
import { mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync, existsSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { extname, join, normalize } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';

const [dir, out, mode = 'all', portArg] = process.argv.slice(2);
if (!dir || !out) {
  console.error('usage: shoot.mjs <harness-dir> <out-dir> [screens|measure|all] [port]');
  process.exit(2);
}
const port = Number(portArg ?? 49600);
const CHROME = '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome';
mkdirSync(out, { recursive: true });

const types = { '.html': 'text/html', '.js': 'text/javascript', '.css': 'text/css', '.woff2': 'font/woff2' };
const server = createServer((req, res) => {
  const path = normalize(decodeURIComponent(new URL(req.url, 'http://x').pathname)).replace(/^\/+/, '');
  const file = join(dir, path || 'index.html');
  if (!file.startsWith(normalize(dir)) || !existsSync(file)) {
    res.writeHead(404).end();
    return;
  }
  res.writeHead(200, { 'content-type': types[extname(file)] ?? 'application/octet-stream' });
  res.end(readFileSync(file));
});
await new Promise((r) => server.listen(port, '127.0.0.1', r));

const profile = mkdtempSync(join(tmpdir(), 'a396-chrome-'));
const cdpPort = port + 1;
const chrome = spawn(CHROME, ['--headless=new', `--remote-debugging-port=${cdpPort}`, '--remote-debugging-address=127.0.0.1', `--user-data-dir=${profile}`, '--no-first-run', '--no-default-browser-check', '--disable-extensions', '--disable-background-networking', '--disable-sync', '--hide-scrollbars', 'about:blank'], { stdio: 'ignore' });
writeFileSync(join(out, 'pids.txt'), `chrome ${chrome.pid}\nnode ${process.pid}\n`);

let version;
for (let i = 0; i < 100 && !version; i++) {
  await delay(200);
  try {
    version = await (await fetch(`http://127.0.0.1:${cdpPort}/json/version`)).json();
  } catch {}
}
if (!version) throw new Error('no DevTools port');
const ws = new WebSocket(version.webSocketDebuggerUrl);
await new Promise((r, j) => {
  ws.addEventListener('open', r, { once: true });
  ws.addEventListener('error', j, { once: true });
});
let id = 0;
const waiting = new Map();
const consoleErrors = [];
ws.addEventListener('message', (e) => {
  const m = JSON.parse(e.data);
  if (m.id !== undefined) {
    const w = waiting.get(m.id);
    waiting.delete(m.id);
    if (m.error) w?.reject(new Error(`${w.method}: ${m.error.message}`));
    else w?.resolve(m.result);
  } else if (m.method === 'Runtime.exceptionThrown') consoleErrors.push(m.params.exceptionDetails.text + ' ' + (m.params.exceptionDetails.exception?.description ?? ''));
  else if (m.method === 'Runtime.consoleAPICalled' && (m.params.type === 'error' || m.params.type === 'warning')) consoleErrors.push(m.params.args.map((a) => a.value ?? a.description).join(' '));
});
const send = (method, params = {}, sessionId) =>
  new Promise((resolve, reject) => {
    const i = ++id;
    const timer = setTimeout(() => {
      if (waiting.delete(i)) reject(new Error(`${method}: no answer in 90 s`));
    }, 90_000);
    waiting.set(i, { resolve: (v) => (clearTimeout(timer), resolve(v)), reject: (e) => (clearTimeout(timer), reject(e)), method });
    ws.send(JSON.stringify({ id: i, method, params, ...(sessionId ? { sessionId } : {}) }));
  });

async function page(w, h, dpr, mobile, reduced) {
  const { targetId } = await send('Target.createTarget', { url: 'about:blank' });
  const { sessionId } = await send('Target.attachToTarget', { targetId, flatten: true });
  const s = (m, p) => send(m, p, sessionId);
  await s('Runtime.enable');
  await s('Page.enable');
  await s('Page.bringToFront');
  await s('Emulation.setDeviceMetricsOverride', { width: w, height: h, deviceScaleFactor: dpr, mobile });
  await s('Emulation.setEmulatedMedia', { features: [{ name: 'prefers-reduced-motion', value: reduced ? 'reduce' : 'no-preference' }] });
  return {
    s,
    async go(query) {
      await s('Page.bringToFront');
      await s('Page.navigate', { url: `http://127.0.0.1:${port}/index.html?${query}` });
      await delay(1500);
      await s('Runtime.evaluate', { expression: 'Promise.race([document.fonts.ready.then(() => new Promise(r => requestAnimationFrame(() => requestAnimationFrame(r)))), new Promise(r => setTimeout(r, 5000))])', awaitPromise: true });
      await delay(150);
    },
    async eval(expression) {
      const r = await s('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true });
      if (r.exceptionDetails) throw new Error(r.exceptionDetails.text);
      return r.result.value;
    },
    async shot(name) {
      const { data } = await s('Page.captureScreenshot', { format: 'png' });
      writeFileSync(join(out, `${name}.png`), Buffer.from(data, 'base64'));
    },
    async mouse(type, x, y, extra = {}) {
      await s('Input.dispatchMouseEvent', { type, x, y, button: type === 'mouseMoved' ? 'none' : 'left', buttons: extra.buttons ?? (type === 'mouseMoved' ? 0 : 1), clickCount: 1, ...extra });
    },
    close: () => send('Target.closeTarget', { targetId }),
  };
}

const results = { browser: version.Browser, when: new Date().toISOString(), screens: [], measure: [], consoleErrors };

try {
  if (mode === 'screens' || mode === 'all') {
    const scenarios = ['levels', 'normal', 'far', 'halt', 'stale', 'gaps', 'crowded', 'loading', 'empty', 'solid'];
    for (const theme of ['dark', 'light']) {
      const desk = await page(1440, 820, 2, false, false);
      for (const sc of scenarios) {
        await desk.go(`theme=${theme}&scenario=${sc}`);
        if (sc === 'levels') {
          await desk.mouse('mouseMoved', 900, 300);
          await delay(120);
          await desk.shot(`desktop-${theme}-crosshair`);
          await desk.mouse('mouseMoved', 1430, 815);
        }
        await desk.shot(`desktop-${theme}-${sc}`);
        results.screens.push(`desktop-${theme}-${sc}`);
      }
      // Zoomed in, then panned away from the newest candle (shows the Latest button).
      await desk.go(`theme=${theme}&scenario=levels`);
      for (let i = 0; i < 8; i++) {
        await desk.s('Input.dispatchMouseEvent', { type: 'mouseWheel', x: 1000, y: 300, deltaX: 0, deltaY: -120 });
        await delay(30);
      }
      await desk.mouse('mousePressed', 700, 300);
      await desk.mouse('mouseMoved', 1100, 320, { buttons: 1 });
      await desk.mouse('mouseReleased', 1100, 320);
      await delay(150);
      await desk.shot(`desktop-${theme}-zoomed-panned`);
      results.screens.push(`desktop-${theme}-zoomed-panned`);
      await desk.close();
      const phone = await page(390, 844, 3, true, true);
      for (const sc of ['levels', 'halt', 'far']) {
        await phone.go(`theme=${theme}&scenario=${sc}`);
        await phone.shot(`phone-${theme}-${sc}-reduced`);
        results.screens.push(`phone-${theme}-${sc}-reduced`);
      }
      await phone.close();
    }
  }
  if (mode === 'measure' || mode === 'all') {
    // Live updates: a busy feed (messages per second) for `seconds`, at CPU throttling 1x and 4x.
    for (const [rate, throttle, n] of [[300, 1, 1500], [300, 4, 1500], [1000, 4, 1500], [300, 6, 1500]]) {
      const p = await page(1440, 820, 2, false, false);
      await p.s('Emulation.setCPUThrottlingRate', { rate: throttle });
      await p.go(`theme=dark&scenario=levels&n=${n}`);
      await p.mouse('mouseMoved', 800, 300);
      await p.eval(`(() => { window.__chart.resetChartStats(); window.__raf = []; let last = performance.now(); const f = (t) => { window.__raf.push(t - last); last = t; if (window.__raf.length < 100000) requestAnimationFrame(f); }; requestAnimationFrame(f); window.__chart.startBusy(${rate}, 10); })()`);
      // Move the crosshair during the run as a user would.
      for (let i = 0; i < 50; i++) {
        await p.mouse('mouseMoved', 300 + i * 18, 200 + (i % 10) * 20);
        await delay(200);
      }
      const r = await p.eval(`(() => { const s = window.__chart.chartStats; const f = [...s.frameMs].sort((a, b) => a - b); const q = (k) => f[Math.min(f.length - 1, Math.floor(k * f.length))]; const raf = window.__raf.slice(5); const long = raf.filter((d) => d > 1000 / 60 * 1.5).length; return { messages: window.__chart.messages(), updates: s.updates, baseFrames: s.baseFrames, overlayFrames: s.overlayFrames, frames: f.length, p50: q(0.5), p95: q(0.95), p99: q(0.99), max: f[f.length - 1], rafFrames: raf.length, rafOver25ms: long, rafMaxMs: Math.max(...raf) }; })()`);
      results.measure.push({ scenario: `busy ${rate}/s, ${n} candles, CPU ${throttle}x, 1440x820 @2x, crosshair moving`, ...r });
      await p.close();
    }
    // Interaction: 60 wheel zoom steps and a long drag pan on 1500 candles, CPU 4x.
    const p = await page(1440, 820, 2, false, false);
    await p.s('Emulation.setCPUThrottlingRate', { rate: 4 });
    await p.go('theme=dark&scenario=levels&n=1500');
    await p.eval('window.__chart.resetChartStats()');
    for (let i = 0; i < 30; i++) {
      await p.s('Input.dispatchMouseEvent', { type: 'mouseWheel', x: 900, y: 300, deltaX: 0, deltaY: 100 });
      await delay(20);
    }
    await p.mouse('mousePressed', 1200, 300);
    for (let i = 0; i < 60; i++) {
      await p.mouse('mouseMoved', 1200 - i * 15, 300, { buttons: 1 });
      await delay(16);
    }
    await p.mouse('mouseReleased', 300, 300);
    const r = await p.eval(`(() => { const f = [...window.__chart.chartStats.frameMs].sort((a, b) => a - b); const q = (k) => f[Math.min(f.length - 1, Math.floor(k * f.length))]; return { frames: f.length, p50: q(0.5), p95: q(0.95), max: f[f.length - 1] }; })()`);
    results.measure.push({ scenario: 'zoom out 30 wheel steps + drag pan 60 moves, 1500 candles, CPU 4x', ...r });
    await p.close();
  }
} finally {
  writeFileSync(join(out, 'measure.json'), JSON.stringify(results, null, 2));
  try {
    await send('Browser.close');
  } catch {}
  await delay(300);
  try {
    chrome.kill('SIGKILL');
  } catch {}
  server.close();
  rmSync(profile, { recursive: true, force: true });
}
console.log(JSON.stringify({ screens: results.screens.length, measure: results.measure, consoleErrors }, null, 1));
process.exit(0);
