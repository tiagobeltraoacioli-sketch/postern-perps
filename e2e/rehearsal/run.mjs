#!/usr/bin/env node
// The scripted rehearsal of the PosternDEX Perpetuals local stack (BRIEF-PERPS-ROUND2, task 435.2).
// UNAUDITED prototype: local processes, loopback only, throwaway keys only, no real network, no real funds.
//
//   node e2e/rehearsal/run.mjs [--stack e2e/.run/stack.json] [--only A,B,C] [--skip K] [--out DIR] [--json]
//
// Numbered checks in phases; every check ends as
//   pass     the property was observed, with the numbers in its evidence
//   fail     the property was NOT observed (the run exits 1)
//   blocked  the check could not run, with the written reason (a stand-in is in place, or a piece is not built);
//            a blocked check is never counted as passed
// The evidence file (JSON) holds every check, its evidence and the stack description.
// Copyright (c) 2026 Bloch Inc. All rights reserved.
import { readFileSync, writeFileSync, mkdirSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join, resolve } from 'node:path';
import { execFile, spawn } from 'node:child_process';
import { promisify } from 'node:util';
import { Api, sleep } from '../lib/client.mjs';
import { Chain } from '../lib/chain.mjs';
import { CAST, who } from './accounts.mjs';
import { Blocked } from './check.mjs';

const HERE = dirname(fileURLToPath(import.meta.url));
const run = promisify(execFile);
const args = process.argv.slice(2);
const opt = (n, d) => (args.includes(`--${n}`) ? args[args.indexOf(`--${n}`) + 1] : d);


const stackPath = resolve(opt('stack', join(HERE, '..', '.run', 'stack.json')));
const stack = JSON.parse(readFileSync(stackPath, 'utf8'));
const foundry = process.env.FOUNDRY_BIN || `${process.env.HOME}/.local/share/dex-tools/foundry`;

const checks = [];
const ctx = {
  stack,
  CAST,
  who,
  api: await new Api(stack.api).init(),
  chain: stack.l2 === 'anvil' ? new Chain(stack.l2Rpc, foundry) : null,
  deployment: stack.l2 === 'anvil' ? JSON.parse(readFileSync(join(dirname(stackPath), 'deployment.json'), 'utf8')) : null,
  state: {},   // facts one phase leaves for the next (never a substitute for a check)
  /** Runs a host-side script of e2e/host over ssh; returns stdout. */
  async host(script, ...a) {
    const { stdout } = await run('bash', [join(HERE, '..', 'hostctl.sh'), script, ...a.map(String)], { maxBuffer: 16 << 20 });
    return stdout.trim();
  },
  /** Same, with `input` on the script's stdin (a tape for the oracle feeder). */
  hostIn(input, script, ...a) {
    return new Promise((res, rej) => {
      const p = spawn('bash', [join(HERE, '..', 'hostctl.sh'), script, ...a.map(String)]);
      let out = '', err = '';
      p.stdout.on('data', (d) => (out += d)); p.stderr.on('data', (d) => (err += d));
      p.on('close', (code) => (code === 0 ? res(out.trim()) : rej(new Error(`${script}: exit ${code}: ${err.trim().slice(0, 300)}`))));
      p.stdin.end(input);
    });
  },
  /** Replaces the oracle tape: rows = array of tape rows (432's format); the feeder restarts on it. */
  async feed(name, rows) {
    if (this.stack.oracle !== 'replay') throw new Blocked('the stack runs the fixed-price dev oracle: no scripted price path');
    return this.hostIn(rows.map((r) => JSON.stringify(r)).join('\n') + '\n', 'feeder-start.sh', '--tape', name);
  },
  async check(id, title, fn) {
    const t0 = Date.now();
    const c = { id, title, status: 'pass', evidence: null, ms: 0 };
    try {
      c.evidence = (await fn()) ?? null;
    } catch (e) {
      if (e instanceof Blocked) { c.status = 'blocked'; c.reason = e.message; } else { c.status = 'fail'; c.reason = String(e?.message ?? e).slice(0, 600); }
    }
    c.ms = Date.now() - t0;
    checks.push(c);
    const mark = { pass: 'PASS   ', fail: 'FAIL   ', blocked: 'BLOCKED' }[c.status];
    process.stdout.write(`${mark} ${id}  ${title}${c.reason ? `\n          ${c.reason}` : ''}\n`);
    return c.status === 'pass';
  },
  blocked(id, title, reason) { return this.check(id, title, () => { throw new Blocked(reason); }); },
};
if (ctx.chain) await ctx.chain.init();

const PHASES = ['A-stack', 'B-deposit', 'C-trade', 'D-funding', 'E-liquidation', 'F-finality', 'G-withdraw', 'H-forced', 'I-validators', 'J-geoblock', 'L-conformance', 'P-page', 'S-feesweep', 'K-escape'];
const only = opt('only', null)?.split(',');
const skip = opt('skip', '')?.split(',').filter(Boolean);
const started = new Date().toISOString();
for (const name of PHASES) {
  const letter = name[0];
  if (only && !only.includes(letter)) continue;
  if (skip.includes(letter)) continue;
  process.stdout.write(`\n== ${name}\n`);
  try {
    const phase = (await import(`./phases/${name}.mjs`)).default;
    await phase(ctx);
  } catch (e) {
    // a phase that dies between checks is a failure of the rehearsal, never a silent stop
    await ctx.check(`${letter}-XX`, `phase ${name} ran to its end`, () => { throw e; });
  }
}

const n = (s) => checks.filter((c) => c.status === s).length;
let tunnelRestarts = null;
try { tunnelRestarts = readFileSync(join(dirname(stackPath), 'tunnel-restarts.log'), 'utf8').trim().split('\n').filter(Boolean); } catch { /* no supervised tunnel */ }
const summary = { total: checks.length, passed: n('pass'), failed: n('fail'), blocked: n('blocked'), networkRetries: ctx.api.networkRetries ?? 0, tunnelRestarts: tunnelRestarts?.length ?? null, variant: process.env.REHEARSAL_ESCAPE === 'forced' ? 'escape by an overdue forced request' : 'escape by silence' };
const outDir = resolve(opt('out', `${process.env.HOME}/Desktop/POSTERNDEX-PERPS-20261117/agents/435-evidence`));
mkdirSync(outDir, { recursive: true });
const stamp = started.replace(/[-:]/g, '').slice(0, 15);
const file = join(outDir, `rehearsal-${stack.label}-${(stack.commit || 'unknown').slice(0, 8)}-${stamp}Z.json`);
writeFileSync(file, JSON.stringify({ unaudited: true, note: 'local stack, throwaway keys, no real network', stack, started, finished: new Date().toISOString(), phases: only ?? PHASES.map((p) => p[0]).filter((l) => !skip.includes(l)), summary, checks }, null, 1) + '\n');
process.stdout.write(`\nrehearsal: ${summary.passed}/${summary.total} checks passed, ${summary.failed} failed, ${summary.blocked} blocked (network retries ${summary.networkRetries}, tunnel restarts ${summary.tunnelRestarts})\nevidence: ${file}\n`);
process.exit(summary.failed ? 1 : 0);
