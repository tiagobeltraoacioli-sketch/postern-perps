#!/usr/bin/env node
// Runs integration/vault/anvil.test.ts: starts anvil (chain 700770, loopback), runs the suite, stops anvil BY PID.
//
//   node integration/vault/run.mjs --anvil <command that runs anvil> --forge-out <forge out dir of contracts/> \
//        [--port 49830] [--evidence FILE.json]
//
// On this Mac anvil needs DYLD_LIBRARY_PATH, so --anvil is a wrapper script. Keys: the committee and guardian keys
// are generated in the test process at run time and never written; users are anvil's public development accounts
// (unlocked on the node; their keys are never read). UNAUDITED prototype; never point this at a real network.
import { spawn } from 'node:child_process';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { setTimeout as delay } from 'node:timers/promises';

const frontend = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const args = process.argv.slice(2);
const opt = (name, fallback) => {
  const i = args.indexOf(`--${name}`);
  return i >= 0 ? args[i + 1] : fallback;
};
const anvilCmd = opt('anvil');
const forgeOut = opt('forge-out');
const port = Number(opt('port', '49830'));
if (!anvilCmd || !forgeOut) {
  console.error('usage: run.mjs --anvil <cmd> --forge-out <dir> [--port 49830] [--evidence FILE]');
  process.exit(2);
}
const [cmd, ...rest] = anvilCmd.split(' ');
const anvil = spawn(cmd, [...rest, '--host', '127.0.0.1', '--port', String(port), '--chain-id', '700770', '--silent'], { stdio: 'ignore' });
console.log(`anvil pid ${anvil.pid} on 127.0.0.1:${port}`);
const rpc = `http://127.0.0.1:${port}`;
let up = false;
for (let i = 0; i < 50 && !up; i++) {
  await delay(200);
  try {
    const r = await fetch(rpc, { method: 'POST', headers: { 'content-type': 'application/json' }, body: '{"jsonrpc":"2.0","id":1,"method":"eth_chainId","params":[]}' });
    up = (await r.json()).result === '0xab162';
  } catch {
    /* not yet */
  }
}
let code = 1;
try {
  if (!up) throw new Error('anvil did not start on chain 700770');
  const test = spawn(process.execPath, [resolve(frontend, 'node_modules/vitest/vitest.mjs'), 'run', '--config', 'integration/vault/vitest.config.mts'], {
    cwd: frontend,
    stdio: 'inherit',
    env: { ...process.env, PERPS_ANVIL_RPC: rpc, PERPS_FORGE_OUT: resolve(forgeOut), PERPS_EVIDENCE: opt('evidence', '') },
  });
  code = await new Promise((done) => test.on('exit', (c) => done(c ?? 1)));
} catch (e) {
  console.error(String(e));
} finally {
  anvil.kill('SIGTERM');
}
process.exit(code);
