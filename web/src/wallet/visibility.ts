// The visibility gate.
//
// An extension wallet looks every input it is asked to sign up on its own node, and refuses one it
// does not find there unspent. KasWare 0.11.5 does so off mainnet, and KOB assumes every extension
// wallet does on every network. KOB's node and the wallet's node are not the same node. An output
// of a transaction KOB's node accepted a moment ago may not have reached the wallet's node yet.
// So:
//
// - Before an extension wallet is asked to sign, every input it signs that spends an output of a
//   transaction this page sent recently must be visible to the wallet's node (`awaitVisible`).
//   Older inputs are on chain for both nodes.
// - After KOB's node accepted a transaction, `noteSent` remembers it. Where the adapter can
//   (`announce`, KasWare `pushTx`), it hands the transaction to the wallet's node at once. That
//   keeps the transaction in the wallet node's mempool while it waits for its block. KOB's node is
//   the broadcast that counts.
// - Every fresh input waits until KOB's node lists it in its UTXO set, which means the transaction
//   is in a block. Then it waits `SETTLE_MS` more, announced or not.
//
// This is probabilistic, not a proof. The wallet finds an output in its node's mempool or in its
// UTXO index. A transaction moving from one to the other can for a moment be in neither, about 1 s
// after it was sent. So an accepted announce does not count on its own. Once KOB's node lists the
// output, the margin leaves the wallet's node time to take the same block into its index, which
// makes a refusal rare. A refusal is safe. Nothing is sent for that step, and asking again a few
// seconds later succeeds (wallet/sign.ts `rejected-fresh`).
//
// The private-key signer (`inputPolicy: 'offline'`) signs what it is shown and skips all of this.
import type { NodeApi } from '../data/node';
import type { Hex, TxInputJson, TxJson } from '../kob/types';
import type { WalletAdapter } from './types';

/**
 * How long `announce` may take. KasWare `pushTx` has no popup, so it answers at once or not at
 * all.
 */
export const ANNOUNCE_TIMEOUT_MS = 3000;
/**
 * How long the gate waits for an input to become visible before it gives up. The wallet is then
 * not asked.
 */
export const VISIBLE_TIMEOUT_MS = 60_000;
/** How often the gate asks KOB's node, in ms. */
export const VISIBLE_POLL_MS = 500;
/**
 * The margin for the wallet's node after KOB's node lists the output. The wallet's node takes the
 * same block into its UTXO index a moment later.
 */
export const SETTLE_MS = 2000;
/**
 * How long a transaction this page sent counts as fresh. Its outputs may not be visible to the
 * wallet's node yet.
 */
export const FRESH_MS = 3 * 60_000;
/**
 * An output older than this when KOB's node lists it needs no margin, because the wallet's node
 * has long seen its block.
 */
const SETTLED_AFTER_MS = 30_000;

interface Sent {
  at: number;
}

const sent = new Map<Hex, Sent>();

const now0 = () => Date.now();

/** Tells whether `adapter` needs its inputs visible to its own node before it is asked to sign. */
export const needsVisibility = (adapter: Pick<WalletAdapter, 'inputPolicy'>): boolean => (adapter.inputPolicy ?? 'node-visible') === 'node-visible';

function prune(now: number): void {
  for (const [id, s] of sent) if (now - s.at > FRESH_MS) sent.delete(id);
}

/**
 * Notes that KOB's node accepted `tx`, which this page sent. It is remembered as fresh, and handed
 * to the wallet's own node when the adapter can (`announce`).
 *
 * It resolves with whether the wallet's node has it, and never rejects. Call it only with a
 * transaction that was finalized, validated and accepted.
 */
export function noteSent(adapter: WalletAdapter | null, tx: TxJson, o: { now?: () => number; timeoutMs?: number } = {}): Promise<boolean> {
  const now = (o.now ?? now0)();
  prune(now);
  sent.set(tx.id.toLowerCase(), { at: now });
  if (!adapter || !needsVisibility(adapter) || typeof adapter.announce !== 'function') return Promise.resolve(false);
  try {
    return adapter.announce(tx, { timeoutMs: o.timeoutMs ?? ANNOUNCE_TIMEOUT_MS }).then(
      () => true,
      () => false,
    );
  } catch {
    return Promise.resolve(false);
  }
}

/**
 * When this page sent `txid`, in ms since the epoch. It is null when it was not sent here, or was
 * sent longer ago than `FRESH_MS`.
 */
export function sentAt(txid: string, now: number = now0()): number | null {
  const s = sent.get(txid.toLowerCase());
  return s && now - s.at <= FRESH_MS ? s.at : null;
}

/**
 * The transactions this page sent less than `FRESH_MS` ago (lower case ids). A coin they made is
 * one the gate may have to wait for, so planning spends it only when the settled coins cannot cover
 * an order (kob/funding.ts `selectFunding`, `selectTokens`).
 */
export function freshTxids(now: number = now0()): Set<string> {
  prune(now);
  return new Set([...sent.keys()].filter((id) => sentAt(id, now) !== null));
}

/** Forgets every transaction noted. For tests only. */
export function resetVisibility(): void {
  sent.clear();
}

/** An input that spends an output of a fresh transaction. */
export interface FreshInput {
  input: TxInputJson;
  /** When its parent was sent. */
  at: number;
}

/**
 * The inputs of `tx` that spend an output of a transaction this page sent less than `FRESH_MS` ago.
 * It adds those of `alsoFresh`, which maps a txid to when it was sent. That serves a creation
 * resumed after a reload, whose first step an earlier page sent.
 */
export function freshInputs(tx: Pick<TxJson, 'inputs'>, o: { alsoFresh?: ReadonlyMap<string, number>; now?: number } = {}): FreshInput[] {
  const now = o.now ?? now0();
  const out: FreshInput[] = [];
  for (const input of tx.inputs) {
    const id = input.transactionId.toLowerCase();
    const extra = o.alsoFresh?.get(id);
    const at = sentAt(id, now) ?? (extra !== undefined && now - extra <= FRESH_MS ? extra : null);
    if (at !== null) out.push({ input, at });
  }
  return out;
}

/** The inputs of `awaitVisible`. */
export interface VisibleArgs {
  adapter: WalletAdapter;
  node: NodeApi;
  /** The transaction the wallet is about to be asked to sign. */
  tx: Pick<TxJson, 'inputs'>;
  /**
   * Maps a script public key to an address of the app's network. KOB's node is asked by address.
   */
  addressOf: (spk: string) => string;
  alsoFresh?: ReadonlyMap<string, number>;
  /**
   * Called with true when the gate starts waiting and with false when it stops, so the UI can say
   * why the wallet is not asked yet.
   */
  onWait?: (waiting: boolean) => void;
  timeoutMs?: number;
  pollMs?: number;
  settleMs?: number;
  sleep?: (ms: number) => Promise<void>;
  now?: () => number;
  signal?: AbortSignal;
}

/** The outcome of `awaitVisible`. */
export type VisibleResult =
  | { ok: true; waitedMs: number }
  /** Not visible in time. The wallet must not be asked now. */
  | { ok: false; waitedMs: number; missing: { transactionId: Hex; index: number }[]; lastError?: string };

const outKey = (txid: string, index: number) => `${txid.toLowerCase()}:${index}`;

/**
 * Resolves once every input of `tx` that spends a fresh output (`freshInputs`) is listed by KOB's
 * node and the margin for the wallet's node passed. It does nothing for an offline signer and for a
 * transaction with no fresh input. It never throws for node hiccups.
 */
export async function awaitVisible(a: VisibleArgs): Promise<VisibleResult> {
  const now = a.now ?? now0;
  const sleep = a.sleep ?? ((ms: number) => new Promise<void>((r) => setTimeout(r, ms)));
  const t0 = now();
  if (!needsVisibility(a.adapter)) return { ok: true, waitedMs: 0 };
  const fresh = freshInputs(a.tx, { ...(a.alsoFresh ? { alsoFresh: a.alsoFresh } : {}), now: t0 });
  if (fresh.length === 0) return { ok: true, waitedMs: 0 };
  try {
    a.onWait?.(true);
  } catch {
    /* a UI callback must not abort the flow */
  }
  try {
    const want = fresh.map((f) => ({ key: outKey(f.input.transactionId, f.input.index), address: a.addressOf(f.input.utxo.scriptPublicKey) }));
    const addresses = [...new Set(want.map((w) => w.address))];
    const timeoutMs = a.timeoutMs ?? VISIBLE_TIMEOUT_MS;
    const pollMs = a.pollMs ?? VISIBLE_POLL_MS;
    let lastError: string | undefined;
    let missing = want;
    for (;;) {
      try {
        const have = new Set((await a.node.getUtxosByAddresses(addresses)).map((u) => outKey(u.transactionId, u.index)));
        missing = want.filter((w) => !have.has(w.key));
        lastError = undefined;
        if (missing.length === 0) break;
      } catch (e) {
        lastError = e instanceof Error ? e.message : String(e);
      }
      if (a.signal?.aborted || now() - t0 + pollMs > timeoutMs) {
        const left = new Set(missing.map((m) => m.key));
        return {
          ok: false,
          waitedMs: now() - t0,
          missing: fresh.filter((f) => left.has(outKey(f.input.transactionId, f.input.index))).map((f) => ({ transactionId: f.input.transactionId, index: f.input.index })),
          ...(lastError ? { lastError } : {}),
        };
      }
      await sleep(pollMs);
    }
    const youngest = Math.max(...fresh.map((f) => f.at));
    if (now() - youngest < SETTLED_AFTER_MS) await sleep(a.settleMs ?? SETTLE_MS);
    return { ok: true, waitedMs: now() - t0 };
  } finally {
    try {
      a.onWait?.(false);
    } catch {
      /* a UI callback must not abort the flow */
    }
  }
}
