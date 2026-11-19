// "Unlock" of a creator lock on the Portfolio (kob/creator-lock.ts). It pays the tokens the
// wallet's lock holds to the wallet once the lock is open. The unlock spends a plain KAS coin at
// the lock's address, as input 0 signed by the wallet key, with the locked token UTXOs. Its lock
// time is the lock's time, and the node takes it once its past median time passes that time. A coin
// already there is spent. Otherwise the run first sends one, 1 KAS from the wallet with the largest
// coins first, and builds the unlock from its output 0 once the node lists it. Both go through
// <TxFlow>. Every check of the send (ui/confirm/send.ts) runs on each, and the wallet approves
// each.
import type { Services } from '../../app/services';
import { readFees } from '../../app/env';
import { pendingSpends } from '../../app/pending-spends';
import { t } from '../../i18n';
import type { CancelPlan } from '../../kob/cancel';
import { LOCK_COIN, buildLockCoin, buildUnlock, creatorLock, type CreatorLockInfo } from '../../kob/creator-lock';
import { pickFor } from '../../kob/fee-policy';
import { KobError } from '../../kob/wasm';
import { TOKEN_PROGRAMS } from '../../kob/programs.generated';
import type { BuiltTx, Hex, KeyUtxo, Utxo } from '../../kob/types';
import { covenantSigningKnown } from '../../wallet/covenant-probe';
import type { WalletAdapter } from '../../wallet/types';
import { waitForAcceptance } from '../../wallet/sign';
import type { LockedHolding } from './balances-data';
import { spkToAddressFor } from './orders-data';
import type { FlowStep } from './TxFlow';

/**
 * Whole minutes from the node's clock to the lock's time, at least 1. The node takes the unlock only
 * once its clock is past that time.
 */
export const minutesBefore = (until: bigint, median: bigint): bigint => (until - median > 0n ? (until - median + 59_999n) / 60_000n : 1n);

/** How long the unlock waits for the node to list the lock coin the run sent. */
export const LOCK_COIN_TIMEOUT_MS = 60_000;

/**
 * Whether the connected wallet can sign the unlock here. Its coin is a script (P2SH) input, which a
 * wallet that signs covenant inputs signs. KasWare and Kaspire are proven on TN10, and Kastle once
 * its test signature passed. The answer is 'cli' otherwise (`kob token unlock`).
 */
export function unlockSigner(adapter: Pick<WalletAdapter, 'id' | 'covenantSigning'> | null | undefined, pubkey: Hex | null | undefined): 'wallet' | 'cli' {
  if (!adapter || !pubkey) return 'cli';
  return adapter.covenantSigning === 'proven' || covenantSigningKnown(adapter, pubkey) === 'ok' ? 'wallet' : 'cli';
}

/**
 * A built transaction as the plan of a flow step. Nothing is cancelled, and the send's decoder
 * derives everything from the transaction.
 */
const planOf = (built: BuiltTx): CancelPlan => ({ ok: true, issues: [], request: null, built, cancelIds: [], expected: {}, tokensReturned: 0n, kasReleased: 0n, fundingUsed: [] });

export interface UnlockFlow {
  steps: FlowStep[];
  /**
   * The unlock built again once the lock coin the run sent is listed by the node, for a run of two
   * steps.
   */
  next?: (index: number, txids: readonly string[]) => Promise<FlowStep>;
}

/**
 * The lock coins already at the lock's address on the node, largest first. None is a transaction of
 * this page spending it.
 */
async function coinsAt(s: Services, lock: CreatorLockInfo): Promise<Utxo[]> {
  const address = spkToAddressFor(s)(lock.scriptPublicKey);
  const found = await s.node.getUtxosByAddresses([address]);
  return found
    .filter((u) => u.scriptPublicKey === lock.scriptPublicKey && !u.covenantId && !pendingSpends.has(u.transactionId, u.index))
    .sort((a, b) => (BigInt(b.amount) > BigInt(a.amount) ? 1 : BigInt(b.amount) < BigInt(a.amount) ? -1 : 0))
    .map((u) => ({ transactionId: u.transactionId, index: u.index, amount: u.amount, blockDaaScore: u.blockDaaScore }));
}

/**
 * Plans the unlock of `h` for `pubkey`. It is one step when a coin is at the lock's address, else
 * the lock coin and then the unlock.
 */
export async function planUnlock(s: Services, pubkey: Hex, h: LockedHolding, ticker: string): Promise<UnlockFlow> {
  const me = pubkey.toLowerCase();
  if (h.key !== me) throw new Error(t('orders.unlock.notYours'));
  const lock = creatorLock(s.kob, { until: h.until, key: h.key });
  // The node's clock, which the lock time is checked against. A node that does not say leaves it to
  // the node's answer.
  const median = s.node.getPastMedianTime ? await s.node.getPastMedianTime().catch(() => null) : null;
  if (median !== null && median <= h.until) throw new Error(t('orders.unlock.notOpen', { minutes: String(minutesBefore(h.until, median)) }));
  const terms = { until: h.until.toString(), key: h.key };
  const locks = [terms];
  const fee = { feeRate: pickFor(await readFees(s), 'normal').rate.toString() };
  const slots = TOKEN_PROGRAMS.find((p) => p.name === h.program)?.maxTokenInputs ?? 1;
  const tokens = h.utxos.slice(0, slots);
  const unlockStep = (coin: Utxo): FlowStep => {
    const built = buildUnlock(s.kob, { token: { covenantId: h.token, program: h.program }, lock: terms, tokens, coin, fee, ownKeys: [me] });
    return { title: t('orders.unlock.step'), plan: planOf(built), spends: [], toast: t('orders.unlock.submitted', { ticker }) };
  };
  const coins = await coinsAt(s, lock);
  if (coins.length > 0) return { steps: [unlockStep(coins[0]!)] };

  // No coin there, so the wallet's KAS sends one first, the fewest of its largest coins that pay it
  // and the fee.
  const funding: KeyUtxo[] = pendingSpends.without(await s.utxos.fundingFor(me));
  let coinTx: BuiltTx | null = null;
  for (let n = 1; n <= funding.length && !coinTx; n++) {
    try {
      coinTx = buildLockCoin(s.kob, { lock: terms, funding: funding.slice(0, n), fee, ownKeys: [me] });
    } catch (e) {
      if (!(e instanceof KobError) || !/insufficient/i.test(e.message) || n === funding.length) throw e;
    }
  }
  if (!coinTx) throw new Error(t('orders.unlock.noKas'));
  const amount = coinTx.tx.outputs[0]?.value ?? LOCK_COIN.toString();
  const coinStep: FlowStep = { title: t('orders.unlock.coinStep'), plan: planOf(coinTx), spends: [], locks, silent: true };
  return {
    steps: [coinStep, unlockStep({ transactionId: coinTx.tx.id, index: 0, amount })],
    next: async (k, txids) => {
      if (k !== 1 || !txids[0]) throw new Error(t('orders.unlock.notAccepted'));
      const address = spkToAddressFor(s)(lock.scriptPublicKey);
      const res = await waitForAcceptance(s.node, { address, transactionId: txids[0], index: 0 }, { timeoutMs: LOCK_COIN_TIMEOUT_MS });
      const got = res.accepted ? res.found.find((f) => f.transactionId === txids[0] && f.index === 0) : undefined;
      if (!got) throw new Error(t('orders.unlock.notAccepted'));
      return unlockStep({ transactionId: got.transactionId, index: got.index, amount: got.amount, blockDaaScore: got.blockDaaScore });
    },
  };
}
