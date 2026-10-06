// One-time covenant-signing check of a wallet (C5-10). Placing an order needs only P2PK signatures, but CANCELLING it needs a signature on a covenant
// (P2SH) input: a wallet that signs the first and not the second (Kastle builds with issue #353) would place orders its user cannot cancel (they end
// only by a fill or the expiry refund). Before the first order of such a wallet the app asks it to sign a TEST cancel of a synthetic order (an
// outpoint that does not exist, never broadcast) and checks the signature with kob-wasm `finalize`. The verdict is remembered per wallet and key.
import type { PlanEnv } from '../kob/plan-types';
import { makeBid } from '../kob/orders/common';
import type { BuiltTx, Hex } from '../kob/types';
import type { KobWasm } from '../kob/wasm';
import { WalletError, type WalletAdapter } from './types';

export type ProbeVerdict = 'ok' | 'unsupported';
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

/** The remembered verdict for this wallet and key (null: never checked, or storage unavailable). */
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
    /* storage blocked: the check is asked again next time */
  }
}

/** Whether orders may be placed with this wallet without a check: declared capable, or checked before. */
export function covenantSigningKnown(adapter: Pick<WalletAdapter, 'id' | 'covenantSigning'>, pubkey: Hex, s: StorageLike | null = storage()): ProbeVerdict | null {
  if (adapter.covenantSigning === 'proven') return 'ok';
  return storedVerdict(adapter.id, pubkey, s);
}

/** The test transaction: the maker's cancel of a synthetic bid of `env.token` at an outpoint that does not exist (unspendable, never sent). */
export function probeTx(kob: KobWasm, env: PlanEnv): BuiltTx {
  const order = makeBid(env, { price: env.token.tick > 0n ? env.token.tick : 1n, minFill: 1n, expiryDaa: env.clock.daa + 1_000n });
  return kob.build({
    action: 'cancelOrder',
    order: { transactionId: 'c0'.repeat(32), index: 0, amount: '100000000', blockDaaScore: '0', covenantId: 'c1'.repeat(32), state: order },
    custody: null, strays: [], tokens: [], funding: [], change: null, lockTime: '0', records: [], fee: {}, ownKeys: [env.maker],
  } as unknown as Parameters<KobWasm['build']>[0]);
}

/**
 * Asks the wallet to sign the test cancel and verifies the signature (kob-wasm `finalize` checks it against the digest). 'ok' and 'unsupported' are
 * remembered; 'declined' (the user closed the popup) and 'error' are not.
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
    // a timeout, a network or provider failure says nothing about the capability: ask again later
    if (e instanceof WalletError && e.code !== 'no-signature' && e.code !== 'unsupported') return 'error';
    // no signature, or one that does not verify (finalize): this wallet cannot sign a cancel
    remember(adapter.id, env.maker, 'unsupported', s);
    return 'unsupported';
  }
}
