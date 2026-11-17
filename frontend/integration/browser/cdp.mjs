// Minimal Chrome DevTools Protocol client on Node 24 built-ins (global WebSocket and fetch). No dependency:
// Playwright is not part of this project. It launches the installed browser headless with its own temporary profile,
// one isolated browser context per scenario, and exposes just what the wallet scenarios need.
import { spawn } from 'node:child_process';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';

export async function launch(executable, port) {
  const profile = mkdtempSync(join(tmpdir(), 'postern-wallet-harness-'));
  const child = spawn(
    executable,
    [
      '--headless=new',
      `--remote-debugging-port=${port}`,
      '--remote-debugging-address=127.0.0.1',
      `--user-data-dir=${profile}`,
      '--no-first-run',
      '--no-default-browser-check',
      '--disable-extensions',
      '--disable-background-networking',
      '--disable-component-update',
      '--disable-sync',
      '--disable-default-apps',
      // Two tabs of one account must both keep polling, as two visible windows would.
      '--disable-background-timer-throttling',
      '--disable-renderer-backgrounding',
      '--disable-backgrounding-occluded-windows',
      'about:blank',
    ],
    { stdio: 'ignore' },
  );
  let version;
  for (let i = 0; i < 100 && !version; i++) {
    await delay(200);
    try {
      version = await (await fetch(`http://127.0.0.1:${port}/json/version`)).json();
    } catch {
      /* not listening yet */
    }
  }
  if (!version) {
    child.kill('SIGKILL');
    throw new Error('the browser did not open its DevTools port');
  }
  const socket = new WebSocket(version.webSocketDebuggerUrl);
  await new Promise((resolve, reject) => {
    socket.addEventListener('open', resolve, { once: true });
    socket.addEventListener('error', () => reject(new Error('DevTools socket failed')), { once: true });
  });
  let next = 0;
  const waiting = new Map();
  const handlers = new Map(); // sessionId -> (method, params) => void
  socket.addEventListener('message', (event) => {
    const message = JSON.parse(event.data);
    if (message.id !== undefined) {
      const entry = waiting.get(message.id);
      waiting.delete(message.id);
      if (!entry) return;
      if (message.error) entry.reject(new Error(`${entry.method}: ${message.error.message}`));
      else entry.resolve(message.result);
    } else handlers.get(message.sessionId ?? '')?.(message.method, message.params);
  });
  const send = (method, params = {}, sessionId) =>
    new Promise((resolve, reject) => {
      const id = ++next;
      waiting.set(id, { resolve, reject, method });
      socket.send(JSON.stringify({ id, method, params, ...(sessionId ? { sessionId } : {}) }));
    });
  return {
    product: version.Browser,
    userAgent: version['User-Agent'],
    send,
    onSession: (sessionId, handler) => handlers.set(sessionId, handler),
    async close() {
      try {
        await send('Browser.close');
      } catch {
        /* already gone */
      }
      await delay(300);
      child.kill('SIGKILL');
      rmSync(profile, { recursive: true, force: true });
    },
    pid: child.pid,
  };
}

// Page-side helpers, evaluated in the page. Controls are found the way a user finds them: by visible text and label.
const HELPERS = String.raw`
window.__h = {
  visible: (el) => !!el && el.getClientRects().length > 0,
  button(name) {
    return [...document.querySelectorAll('button')].find((b) => this.visible(b) && b.textContent.replace(/\s+/g, ' ').trim().startsWith(name));
  },
  field(label) {
    const l = [...document.querySelectorAll('label')].find((x) => x.textContent.trim().startsWith(label));
    return l ? l.querySelector('input,select') : null;
  },
  set(el, value) {
    const proto = el instanceof HTMLSelectElement ? HTMLSelectElement.prototype : HTMLInputElement.prototype;
    Object.getOwnPropertyDescriptor(proto, 'value').set.call(el, value);
    el.dispatchEvent(new Event(el instanceof HTMLSelectElement ? 'change' : 'input', { bubbles: true }));
  },
  labelled(name) {
    const el = [...document.querySelectorAll('[aria-label]')].find((x) => x.getAttribute('aria-label') === name);
    return el ? el.innerText : null;
  },
  // What a user can read on the page WITHOUT a click (scrolling allowed). document.body.innerText also returns the
  // text of regions that are rendered but cut to no height or inert (a closed disclosure, the closed side panel):
  // those are taken out of rendering for the moment of the read and put back at once, in the same task, so the
  // page never paints or sees the change.
  offscreen() {
    const out = new Set(document.querySelectorAll('[inert]'));
    for (const el of document.querySelectorAll('body *')) {
      const style = getComputedStyle(el);
      if (style.display === 'none') continue;
      const clips = style.overflowX !== 'visible' || style.overflowY !== 'visible';
      const box = el.getBoundingClientRect();
      if ((clips && (box.height < 1 || box.width < 1) && el.textContent.trim()) || Number(style.opacity) === 0) out.add(el);
    }
    return [...out];
  },
  visibleText() {
    const hidden = this.offscreen().map((el) => [el, el.style.getPropertyValue('display'), el.style.getPropertyPriority('display')]);
    for (const [el] of hidden) el.style.setProperty('display', 'none', 'important');
    try {
      return document.body.innerText.replace(/\s+/g, ' ');
    } finally {
      for (const [el, value, priority] of hidden) value ? el.style.setProperty('display', value, priority) : el.style.removeProperty('display');
    }
  },
  // A control the user can reach without another click: rendered, with a box, and not inside an inert or clipped region.
  reachable(el) {
    return this.visible(el) && !this.offscreen().some((region) => region.contains(el));
  },
};`;

export async function openPage(browser, contextId, { initScript, url, onBinding, log }) {
  const { targetId } = await browser.send('Target.createTarget', { url: 'about:blank', browserContextId: contextId });
  const { sessionId } = await browser.send('Target.attachToTarget', { targetId, flatten: true });
  const cdp = (method, params) => browser.send(method, params, sessionId);
  const consoleLines = [];
  browser.onSession(sessionId, (method, params) => {
    if (method === 'Runtime.bindingCalled') onBinding?.(params.name, params.payload, page);
    else if (method === 'Runtime.consoleAPICalled')
      consoleLines.push(`${params.type}: ${params.args.map((a) => a.value ?? a.description ?? '').join(' ')}`);
    else if (method === 'Runtime.exceptionThrown')
      consoleLines.push(`pageerror: ${params.exceptionDetails.exception?.description ?? params.exceptionDetails.text}`);
  });
  await cdp('Page.enable');
  await cdp('Runtime.enable');
  await cdp('Runtime.addBinding', { name: '__walletBridge' });
  await cdp('Page.addScriptToEvaluateOnNewDocument', { source: `${HELPERS}\n${initScript}` });
  const evaluate = async (expression) => {
    const { result, exceptionDetails } = await cdp('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true });
    if (exceptionDetails) throw new Error(`page evaluation failed: ${exceptionDetails.exception?.description ?? exceptionDetails.text}`);
    return result.value;
  };
  const waitFor = async (what, expression, timeoutMs = 30_000) => {
    const end = Date.now() + timeoutMs;
    for (;;) {
      let value;
      try {
        value = await evaluate(expression);
      } catch {
        value = null; // navigation in progress
      }
      if (value) return value;
      if (Date.now() > end) {
        log?.(`timeout waiting for ${what}; page text:\n${await evaluate('document.body.innerText').catch(() => '(unavailable)')}`);
        throw new Error(`timeout waiting for ${what}`);
      }
      await delay(150);
    }
  };
  const page = {
    targetId,
    console: consoleLines,
    evaluate,
    waitFor,
    cdp,
    goto: async (to) => {
      await cdp('Page.navigate', { url: to });
      await waitFor('document', 'document.readyState === "complete" && !!window.__h');
    },
    reload: async () => {
      await cdp('Page.reload');
      await delay(300);
      await waitFor('document', 'document.readyState === "complete" && !!window.__h');
    },
    text: () => evaluate('document.body.innerText'),
    /** The text on screen without a click (see `visibleText` above); `text()` also returns clipped and inert regions. */
    visibleText: () => evaluate('window.__h.visibleText()'),
    waitVisibleText: (pattern, timeoutMs) => waitFor(`visible text ${pattern}`, `(${pattern}).test(window.__h.visibleText())`, timeoutMs),
    /** A button by its visible name that the user can reach without another click. */
    reachableButton: (name) =>
      evaluate(`(() => { const b = [...document.querySelectorAll('button')].find((x) => x.textContent.replace(/\\s+/g, ' ').trim().startsWith(${JSON.stringify(name)}) && window.__h.reachable(x)); return b ? { disabled: b.disabled } : null; })()`),
    labelled: (name) => evaluate(`window.__h.labelled(${JSON.stringify(name)})`),
    waitText: (pattern, timeoutMs) =>
      waitFor(`text ${pattern}`, `(${pattern}).test(document.body.innerText.replace(/\\s+/g, ' '))`, timeoutMs),
    button: (name) =>
      evaluate(`(() => { const b = window.__h.button(${JSON.stringify(name)}); return b ? { disabled: b.disabled } : null; })()`),
    click: async (name, timeoutMs) => {
      await waitFor(`enabled button "${name}"`, `(() => { const b = window.__h.button(${JSON.stringify(name)}); return !!b && !b.disabled; })()`, timeoutMs);
      await evaluate(`window.__h.button(${JSON.stringify(name)}).click()`);
    },
    fill: async (label, value) => {
      await waitFor(`field "${label}"`, `!!window.__h.field(${JSON.stringify(label)})`);
      await evaluate(`window.__h.set(window.__h.field(${JSON.stringify(label)}), ${JSON.stringify(value)})`);
    },
    select: async (label, optionText) => {
      await waitFor(`field "${label}"`, `!!window.__h.field(${JSON.stringify(label)})`);
      await evaluate(`(() => { const s = window.__h.field(${JSON.stringify(label)}); const o = [...s.options].find((x) => x.textContent.trim() === ${JSON.stringify(optionText)}); if (!o) throw new Error('no option ' + ${JSON.stringify(optionText)}); window.__h.set(s, o.value); })()`);
    },
    screenshot: async (file) => {
      const { data } = await cdp('Page.captureScreenshot', { format: 'png', captureBeyondViewport: true });
      (await import('node:fs')).writeFileSync(file, Buffer.from(data, 'base64'));
    },
    close: () => browser.send('Target.closeTarget', { targetId }),
  };
  await page.goto(url);
  return page;
}
