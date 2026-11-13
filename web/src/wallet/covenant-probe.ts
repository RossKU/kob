// One-time covenant-signing check of a wallet. Placing an order needs only P2PK signatures, but
// cancelling it needs a signature on a covenant (P2SH) input. A wallet that signs the first and
// not the second, as some Kastle builds do, would place orders its user cannot cancel. They end
// only by a fill or the expiry refund.
//
// Before the first order of such a wallet the app asks it to sign a test cancel of a synthetic
// order, which spends an outpoint that does not exist and is never broadcast. It checks the
// signature with kob-wasm `finalize`. The verdict is remembered per wallet and key.
import type { PlanEnv } from '../kob/plan-types';
import { makeBid } from '../kob/orders/common';
import type { BuiltTx, Hex } from '../kob/types';
import type { KobWasm } from '../kob/wasm';
import { WalletError, type WalletAdapter } from './types';

/** The verdict of a covenant-signing check: the wallet signs covenant inputs or it does not. */
export type ProbeVerdict = 'ok' | 'unsupported';
/** The outcome of a check. 'declined' and 'error' say nothing about the wallet. */
export type ProbeOutcome = ProbeVerdict | 'declined' | 'error';

const KEY = (adapter: string, pubkey: Hex) => `kob.covenantSign.v1:${adapter}:${pubkey}`;

interface StorageLike { getItem(k: string): string | null; setItem(k: string, v: string): void }
const storage = (): StorageLike | null => {
  try {
    return typeof localStorage === 'undefined' ? null : localStorage;
  } catch {
    return null;
  }
};

/**
 * The remembered verdict for this wallet and key. Null when never checked or storage is
 * unavailable.
 */
export function storedVerdict(adapter: string, pubkey: Hex, s: StorageLike | null = storage()): ProbeVerdict | null {
  try {
    const v = s?.getItem(KEY(adapter, pubkey));
    return v === 'ok' || v === 'unsupported' ? v : null;
  } catch {
    return null;
  }
}

function remember(adapter: string, pubkey: Hex, v: ProbeVerdict, s: StorageLike | null = storage()): void {
  try {
    s?.setItem(KEY(adapter, pubkey), v);
  } catch {
    /* Storage blocked. The check is asked again next time. */
  }
}

/**
 * Whether orders may be placed with this wallet without a check: it is declared capable, or was
 * checked before.
 */
export function covenantSigningKnown(adapter: Pick<WalletAdapter, 'id' | 'covenantSigning'>, pubkey: Hex, s: StorageLike | null = storage()): ProbeVerdict | null {
  if (adapter.covenantSigning === 'proven') return 'ok';
  return storedVerdict(adapter.id, pubkey, s);
}

/**
 * The test transaction: the maker's cancel of a synthetic bid of `env.token` at an outpoint that
 * does not exist. It is unspendable and never sent.
 */
export function probeTx(kob: KobWasm, env: PlanEnv): BuiltTx {
  const order = makeBid(env, { price: env.token.tick > 0n ? env.token.tick : 1n, minFill: 1n, expiryDaa: env.clock.daa + 1_000n });
  return kob.build({
    action: 'cancelOrder',
    order: { transactionId: 'c0'.repeat(32), index: 0, amount: '100000000', blockDaaScore: '0', covenantId: 'c1'.repeat(32), state: order },
    custody: null, strays: [], tokens: [], funding: [], change: null, lockTime: '0', records: [], fee: {}, ownKeys: [env.maker],
  } as unknown as Parameters<KobWasm['build']>[0]);
}

/**
 * Asks the wallet to sign the test cancel and verifies the signature (kob-wasm `finalize` checks it
 * against the digest). 'ok' and 'unsupported' are remembered. 'declined', where the user closed
 * the popup, and 'error' are not.
 */
export async function probeCovenantSigning(kob: KobWasm, adapter: WalletAdapter, env: PlanEnv, network: string, s: StorageLike | null = storage()): Promise<ProbeOutcome> {
  let built: BuiltTx;
  try {
    built = probeTx(kob, env);
  } catch {
    return 'error';
  }
  try {
    const sigs = await adapter.signTx(built, { network });
    kob.finalize(built, sigs, { tightenBudgets: true });
    remember(adapter.id, env.maker, 'ok', s);
    return 'ok';
  } catch (e) {
    if (e instanceof WalletError && e.code === 'rejected') return 'declined';
    // A timeout, a network or provider failure says nothing about the capability. Ask again later.
    if (e instanceof WalletError && e.code !== 'no-signature' && e.code !== 'unsupported') return 'error';
    // No signature, or one that does not verify (finalize): this wallet cannot sign a cancel.
    remember(adapter.id, env.maker, 'unsupported', s);
    return 'unsupported';
  }
}
