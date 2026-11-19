/** The ABIs this interface calls, PINNED (config/vault-abi.json from a forge build; scripts/pin-vault-abi.mjs). */
import type { Abi } from 'viem';
import pin from '../../config/vault-abi.json';

export const VAULT_ABI = pin.abi as Abi;
export const VAULT_SOURCE_SHA256 = pin.sourceSha256;

/** The four ERC-20 entries used: read balance and allowance, approve an exact amount. */
export const TOKEN_ABI = [
  { type: 'function', name: 'balanceOf', stateMutability: 'view', inputs: [{ name: 'owner', type: 'address' }], outputs: [{ name: '', type: 'uint256' }] },
  { type: 'function', name: 'allowance', stateMutability: 'view', inputs: [{ name: 'owner', type: 'address' }, { name: 'spender', type: 'address' }], outputs: [{ name: '', type: 'uint256' }] },
  { type: 'function', name: 'decimals', stateMutability: 'view', inputs: [], outputs: [{ name: '', type: 'uint8' }] },
  { type: 'function', name: 'approve', stateMutability: 'nonpayable', inputs: [{ name: 'spender', type: 'address' }, { name: 'amount', type: 'uint256' }], outputs: [{ name: '', type: 'bool' }] },
] as const satisfies Abi;

/** What each vault revert means for the person reading it (docs/VAULT.md). Unknown reverts are said to be unknown. */
export const REVERT_WORDS: Record<string, string> = {
  InvalidDeposit: 'the vault refuses this deposit (below the minimum, above the vault cap, or to an account that cannot be credited)',
  InvalidTransfer: 'the token did not move exactly the amount (the vault refuses tokens that take a fee or misreport)',
  InvalidWithdrawal: 'the vault refuses this record (nothing left to pay, an amount of zero, or a recipient it never pays)',
  InvalidProof: 'the proof does not match the root the vault holds',
  AlreadyRegistered: 'this claim is already registered at the vault',
  NotRegistered: 'this payout is not registered at the vault',
  NotReady: 'the vault is not ready to pay this yet (a large amount waits out its delay, or the system shares are not proven)',
  RateLimited: 'the vault has paid its limit for this period; the rest becomes payable as the limit refills',
  Paused: 'the vault is paused by a guardian; payments and deposits resume when the pause ends',
  InvalidForced: 'the vault refuses this request (only an account that deposited may ask, and only one request may be open)',
  EscapeActive: 'the vault is in escape mode: deposits, requests and checkpoints have stopped for good',
  NotEscape: 'the vault is not in escape mode',
  EscapeNotDue: 'escape is not due: the exchange is still within its deadlines',
  InvalidRefund: 'this deposit cannot be refunded (already credited by the exchange, already refunded, or not as recorded)',
  Insolvent: 'the vault refuses: paying this would exceed what the validators certified',
  Reentrancy: 'the vault refused a nested call',
};
