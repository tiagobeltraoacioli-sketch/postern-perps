// The rehearsal's cast: throwaway accounts (public labels) and what each deposits. UNAUDITED prototype.
// Copyright (c) 2026 Bloch Inc. All rights reserved.
import { user } from '../lib/keys.mjs';

// label, deposit in USDC (decimal string), role
export const CAST = [
  ['maker', '400000', 'market maker: rests both sides of every book'],
  ['alice', '20000', 'trader: opens, is funded, closes, withdraws with a proof'],
  ['bob', '20000', 'trader: the other side of alice; ADL counterparty'],
  ['liq-book', '1500', 'leveraged long liquidated through the book'],
  ['liq-backstop', '1500', 'leveraged long whose remainder the backstop takes'],
  ['liq-adl', '1500', 'leveraged long closed by auto-deleveraging'],
  ['censored', '5000', 'user the sequencer ignores: exits by the forced path'],
  ['escapee', '5000', 'user who claims through the escape hatch'],
  ['conformance', '100000', 'the conformance suite\'s --funded account'],
].map(([label, deposit, role]) => ({ ...user(label), deposit, role }));

export const who = (label) => {
  const a = CAST.find((c) => c.label === label);
  if (!a) throw new Error(`no rehearsal account ${label}`);
  return a;
};

if (process.argv[2] === '--fake-l2-args') process.stdout.write(CAST.map((c) => `--deposit ${c.address}:${c.deposit}`).join(' ') + '\n');
if (process.argv[2] === '--print') process.stdout.write(JSON.stringify(CAST.map(({ label, address, deposit, role }) => ({ label, address, deposit, role })), null, 1) + '\n');
