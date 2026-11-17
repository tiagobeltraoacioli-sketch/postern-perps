import { readFileSync } from 'node:fs';
import { loadList } from '../src/list.mjs';
import { loadSanctions } from '../src/sanctions.mjs';
import { sha256, hex } from '../src/sha256.mjs';

export const here = (p) => new URL(`../${p}`, import.meta.url);
export const readJson = (p) => JSON.parse(readFileSync(here(p), 'utf8'));
export const readText = (p) => readFileSync(here(p), 'utf8');

export function loadListFile(p) {
  const bytes = new Uint8Array(readFileSync(here(p)));
  return loadList(JSON.parse(new TextDecoder().decode(bytes)), { sha256: hex(sha256(bytes)) });
}
export const fixtureList = () => loadListFile('fixtures/geoblock.fixture.json');
export const realList = () => loadListFile('../geoblock.json');
export const fixtureSanctions = () => loadSanctions(readJson('fixtures/sanctions-fixture.json'));
