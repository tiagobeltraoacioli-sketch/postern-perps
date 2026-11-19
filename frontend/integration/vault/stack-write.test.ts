/**
 * Dry run of integration/browser/stack-write.mjs (the page writes to a real stack) WITHOUT the real node: the real
 * PerpsVault bytecode on the local anvil, the reference mock as the exchange (started with a typed-data domain
 * whose verifyingContract is this vault, so the page's signing pin and the mock's signature check both hold), the
 * page from the dev server, and the script itself with --rehearse. Everything but the real node is exercised: two
 * wallets signing through anvil, reconnects after reloads, both deposits on the vault, both attestations, the
 * maker's resting order, the taker's fill, the withdrawal action, the cancel. NOT exercised (the script says so in
 * its results): the node crediting a deposit, the checkpoint reaching the vault and the vault payout.
 * UNAUDITED prototype, loopback only. Needs PERPS_BROWSER=1 like browser.test.ts.
 */
import { afterAll, beforeAll, describe, expect, it } from 'vitest';
import { spawn, type ChildProcess } from 'node:child_process';
import { mkdirSync, mkdtempSync, readFileSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import { standIn, U, type StandIn } from './stand-in';

const RPC = process.env.PERPS_ANVIL_RPC ?? '';
const OUT = process.env.PERPS_FORGE_OUT ?? '';
const SHOTS = process.env.PERPS_SHOTS ?? '';
const ON = !!RPC && !!OUT && process.env.PERPS_BROWSER === '1';
const MOCK_PORT = 49817;
const APP_PORT = 49818;
const CDP_PORT = 49857;
const TAKER = 9;
const MAKER = 5;

let s: StandIn;
const children: ChildProcess[] = [];
const up = async (url: string, what: string) => {
  for (let i = 0; i < 150; i++) {
    try {
      if ((await fetch(url)).ok) return;
    } catch {
      /* not yet */
    }
    await delay(200);
  }
  throw new Error(`${what} did not start`);
};

describe.skipIf(!ON)('stack-write.mjs, rehearsed against the mock and PerpsVault on anvil', () => {
  beforeAll(async () => {
    s = await standIn(RPC, OUT);
    await s.mint(s.accounts[TAKER], 5_000n * U);
    await s.mint(s.accounts[MAKER], 5_000n * U);
    const frontend = process.cwd();
    const node = process.execPath;
    const scratch = mkdtempSync(join(tmpdir(), 'perps-stack-write-'));
    // The mock's typed-data fixture with THIS vault as verifying contract (a development deployment has its own).
    const fixture = JSON.parse(readFileSync(resolve(frontend, '../node/mock/fixtures/eip712.json'), 'utf8'));
    fixture.domain.verifyingContract = s.deployment.vault;
    writeFileSync(join(scratch, 'eip712.json'), JSON.stringify(fixture));
    children.push(spawn(node, [resolve(frontend, '../node/mock/server.mjs'), '--port', String(MOCK_PORT), '--start-time', 'now', '--eip712', join(scratch, 'eip712.json')], { stdio: 'ignore' }));
    await up(`http://127.0.0.1:${MOCK_PORT}/v1/status`, 'the mock API');
    await fetch(`http://127.0.0.1:${MOCK_PORT}/mock/scenario?geo=ZZ`);
    children.push(
      spawn(node, ['scripts/dev-local.mjs', '--api', `http://127.0.0.1:${MOCK_PORT}`, '--l2', RPC, '--vault', s.deployment.vault, '--token', s.deployment.token, '--port', String(APP_PORT), '--config-out', resolve(frontend, '.local/stack-write.json')], { cwd: frontend, stdio: 'ignore' }),
    );
    await up(`http://127.0.0.1:${APP_PORT}/`, 'the dev server');
  });
  afterAll(() => {
    for (const c of children) c.kill('SIGTERM');
  });

  it('runs every step but the real node\'s, with no failed assertion', async () => {
    const out = SHOTS ? join(SHOTS, 'stack-write-rehearsal') : mkdtempSync(join(tmpdir(), 'perps-stack-write-out-'));
    mkdirSync(out, { recursive: true });
    const run = spawn(
      process.execPath,
      ['integration/browser/stack-write.mjs', '--rehearse', '--app', `http://127.0.0.1:${APP_PORT}/`, '--api', `http://127.0.0.1:${MOCK_PORT}`, '--l2', RPC, '--vault', s.deployment.vault, '--token', s.deployment.token, '--out', out, '--account-index', String(TAKER), '--maker-index', String(MAKER), '--port', String(CDP_PORT)],
      { cwd: process.cwd(), stdio: ['ignore', 'pipe', 'pipe'] },
    );
    let log = '';
    run.stdout.on('data', (d) => (log += d));
    run.stderr.on('data', (d) => (log += d));
    const code = await new Promise<number>((done) => run.on('exit', (c) => done(c ?? 1)));
    const results = JSON.parse(readFileSync(join(out, 'results.json'), 'utf8')) as { failures: string[]; results: { step: string; ok: boolean; what: string }[]; facts: { notRehearsed: string[]; transactionsThePageSent: { taker: { call: string }[]; maker: { call: string }[] }; typedDataThePageSigned: { taker: { primaryType: string }[]; maker: { primaryType: string }[] } } };
    expect(results.failures, log).toEqual([]);
    expect(code, log).toBe(0);
    // What the page itself sent and signed, by name.
    expect(results.facts.transactionsThePageSent.taker.map((t) => t.call)).toEqual(['approve', 'deposit']);
    expect(results.facts.transactionsThePageSent.maker.map((t) => t.call)).toEqual(['approve', 'deposit']);
    expect(results.facts.typedDataThePageSigned.taker.map((t) => t.primaryType).slice(0, 3)).toEqual(['Attestation', 'PlaceOrder', 'Withdraw']);
    expect(results.facts.typedDataThePageSigned.maker.map((t) => t.primaryType).slice(0, 2)).toEqual(['Attestation', 'PlaceOrder']);
    // The vault holds both deposits.
    expect(await s.balanceOf(s.accounts[TAKER])).toBe(4_000n * U);
    expect(await s.balanceOf(s.accounts[MAKER])).toBe(4_000n * U);
    // The two steps only the real node can do are named, not passed over in silence.
    expect(results.facts.notRehearsed.join(' | ')).toMatch(/credits the deposit/);
    expect(results.facts.notRehearsed.join(' | ')).toMatch(/vault accepts it and pays/);
    expect(results.results.some((r) => /FILLED/.test(r.what) && r.ok)).toBe(true);
  });
});
