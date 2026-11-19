#!/usr/bin/env node
// Pins the PerpsVault ABI this interface calls. UNAUDITED prototype.
//
//   node scripts/pin-vault-abi.mjs <forge out dir of contracts/>        (re-pin; writes config/vault-abi.json)
//   node scripts/pin-vault-abi.mjs ../contracts/abi/PerpsVault.abi.json (re-pin from the committed ABI)
//   node scripts/pin-vault-abi.mjs <forge out dir of contracts/> --check (exit 1 when the pin differs)
//
// The pin records the sha256 of contracts/src/PerpsVault.sol it was generated from; src/test/vault-abi.test.ts fails
// when that source changes, so a contract change can never reach users through a stale interface in silence: re-pin,
// re-read docs/VAULT.md, re-run integration/vault/anvil.mjs.
import { createHash } from 'node:crypto';
import { readFileSync, writeFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const frontend = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const out = process.argv[2];
if (!out) {
  console.error('usage: pin-vault-abi.mjs <forge out dir> [--check]');
  process.exit(2);
}
// <forge out dir>, or the ABI file the contracts area commits (contracts/abi/PerpsVault.abi.json, which its own
// script/export_abi.py --check proves equal to the build).
const artifact = out.endsWith('.json') ? { abi: JSON.parse(readFileSync(resolve(out), 'utf8')) } : JSON.parse(readFileSync(resolve(out, 'PerpsVault.sol/PerpsVault.json'), 'utf8'));
const source = readFileSync(resolve(frontend, '../contracts/src/PerpsVault.sol'));
const abi = artifact.abi.filter((e) => e.type !== 'constructor');
const pin = {
  note: 'UNAUDITED prototype. PerpsVault ABI pinned by scripts/pin-vault-abi.mjs from a forge build of contracts/src/PerpsVault.sol. Do not edit by hand.',
  contract: 'PerpsVault',
  sourceSha256: createHash('sha256').update(source).digest('hex'),
  abi,
};
const text = `${JSON.stringify(pin, null, 1)}\n`;
const path = resolve(frontend, 'config/vault-abi.json');
if (process.argv.includes('--check')) {
  const same = readFileSync(path, 'utf8') === text;
  console.log(same ? 'vault ABI pin: equal to the build' : 'vault ABI pin: DIFFERS from the build');
  process.exit(same ? 0 : 1);
}
writeFileSync(path, text);
console.log(`pinned ${abi.length} ABI entries, source sha256 ${pin.sourceSha256}`);
