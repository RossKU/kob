// The visibility gate. An extension wallet looks every input it is asked to sign up on its OWN node and refuses one it does not find there
// unspent (KasWare 0.11.5 off mainnet; KOB assumes every extension wallet does, on every network). KOB's node and the wallet's node are not the
// same node: an output of a transaction KOB's node accepted a moment ago may not have reached the wallet's node yet. So:
//
//   * Before an extension wallet is asked to sign, every input it signs that spends an output of a transaction this page sent recently must be
//     visible to the wallet's node: `awaitVisible`. (Older inputs are on chain for both nodes.)
//   * After KOB's node accepted a transaction, `noteSent` remembers it and, where the adapter can (`announce`: KasWare `pushTx`), hands it to the
//     wallet's node at once. KOB's node is the broadcast that counts; a failed announce only means the gate waits below.
//   * Where the adapter can ask the wallet's node itself (`walletSees`: KasWare `getUtxoEntries`),
//     the gate polls it until that node lists every fresh input, announced or not. Neither an
//     announce nor KOB's node proves what the wallet will find: on TN10 KasWare once refused a
//     genesis input that KOB's node listed. A transaction moving from the mempool into a block can
//     for a moment be in neither place the wallet looks.
//   * Otherwise, or once `walletSees` fails, an announced output counts as visible. One not
//     announced waits until KOB's node lists it in its UTXO set (the transaction is in a block),
//     then a short margin for the wallet's node to see that block too.
//
// The private-key signer (`inputPolicy: 'offline'`) signs what it is shown and skips all of this.
import type { NodeApi } from '../data/node';
import type { Hex, TxInputJson, TxJson } from '../kob/types';
import type { WalletAdapter } from './types';

/** How long `announce` may take (KasWare `pushTx` has no popup: it answers at once or not at all). */
export const ANNOUNCE_TIMEOUT_MS = 3000;
/** How long the gate waits for an input to become visible before it gives up (the wallet is then not asked). */
export const VISIBLE_TIMEOUT_MS = 60_000;
export const VISIBLE_POLL_MS = 500;
/** The margin for the wallet's node after KOB's node lists the output: it sees the same block a moment later. */
export const SETTLE_MS = 1500;
/**
 * How long one `walletSees` call may take. A slower wallet is not asked again: the gate falls back
 * to the announce and KOB's node.
 */
export const WALLET_SEES_TIMEOUT_MS = 5000;
/** How long a transaction this page sent counts as fresh (its outputs may not be visible to the wallet's node yet). */
export const FRESH_MS = 3 * 60_000;
/** An output older than this when KOB's node lists it needs no margin: the wallet's node has long seen its block. */
const SETTLED_AFTER_MS = 30_000;

interface Sent {
  at: number;
  /** the adapter that announced it (another wallet's node may not have it) */
  walletId: string | null;
  announced: Promise<boolean> | null;
}

const sent = new Map<Hex, Sent>();

const now0 = () => Date.now();

/** Whether `adapter` needs its inputs visible to its own node before it is asked to sign. */
export const needsVisibility = (adapter: Pick<WalletAdapter, 'inputPolicy'>): boolean => (adapter.inputPolicy ?? 'node-visible') === 'node-visible';

function prune(now: number): void {
  for (const [id, s] of sent) if (now - s.at > FRESH_MS) sent.delete(id);
}

/**
 * KOB's node accepted `tx`, which this page sent: remembered as fresh, and handed to the wallet's own node when the adapter can (`announce`).
 * Resolves whether the wallet's node has it; never rejects. Call it only with a transaction that was finalized, validated and accepted.
 */
export function noteSent(adapter: WalletAdapter | null, tx: TxJson, o: { now?: () => number; timeoutMs?: number } = {}): Promise<boolean> {
  const now = (o.now ?? now0)();
  prune(now);
  const entry: Sent = { at: now, walletId: adapter?.id ?? null, announced: null };
  sent.set(tx.id.toLowerCase(), entry);
  if (!adapter || !needsVisibility(adapter) || typeof adapter.announce !== 'function') return Promise.resolve(false);
  let p: Promise<boolean>;
  try {
    p = adapter.announce(tx, { timeoutMs: o.timeoutMs ?? ANNOUNCE_TIMEOUT_MS }).then(
      () => true,
      () => false,
    );
  } catch {
    p = Promise.resolve(false);
  }
  entry.announced = p;
  return p;
}

/** When this page sent `txid` (ms since the epoch), or null: not sent here, or longer ago than `FRESH_MS`. */
export function sentAt(txid: string, now: number = now0()): number | null {
  const s = sent.get(txid.toLowerCase());
  return s && now - s.at <= FRESH_MS ? s.at : null;
}

/** Tests only: forgets every transaction noted. */
export function resetVisibility(): void {
  sent.clear();
}

export interface FreshInput {
  input: TxInputJson;
  /** when its parent was sent */
  at: number;
}

/**
 * The inputs of `tx` that spend an output of a transaction this page sent less than `FRESH_MS` ago, plus those of `alsoFresh` (txid -> when it
 * was sent: a creation resumed after a reload, whose first step an earlier page sent).
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

export interface VisibleArgs {
  adapter: WalletAdapter;
  node: NodeApi;
  /** the transaction the wallet is about to be asked to sign */
  tx: Pick<TxJson, 'inputs'>;
  /** script public key -> address of the app's network (KOB's node is asked by address) */
  addressOf: (spk: string) => string;
  alsoFresh?: ReadonlyMap<string, number>;
  /** the gate starts waiting (true) and stops (false): the UI says why the wallet is not asked yet */
  onWait?: (waiting: boolean) => void;
  timeoutMs?: number;
  pollMs?: number;
  settleMs?: number;
  sleep?: (ms: number) => Promise<void>;
  now?: () => number;
  signal?: AbortSignal;
}

export type VisibleResult =
  | { ok: true; waitedMs: number }
  /** not visible in time: the wallet must not be asked now */
  | { ok: false; waitedMs: number; missing: { transactionId: Hex; index: number }[]; lastError?: string };

const outKey = (txid: string, index: number) => `${txid.toLowerCase()}:${index}`;

/**
 * Resolves once every input of `tx` that spends a fresh output (`freshInputs`) is visible to the
 * wallet's own node. It is listed there when the wallet can say so (`walletSees`); otherwise it was
 * announced to that node, or KOB's node lists it and the margin passed. A no-op for an offline
 * signer and for a transaction with no fresh input. Never throws for node or wallet hiccups.
 */
export async function awaitVisible(a: VisibleArgs): Promise<VisibleResult> {
  const now = a.now ?? now0;
  const sleep = a.sleep ?? ((ms: number) => new Promise<void>((r) => setTimeout(r, ms)));
  const t0 = now();
  if (!needsVisibility(a.adapter)) return { ok: true, waitedMs: 0 };
  const fresh = freshInputs(a.tx, { ...(a.alsoFresh ? { alsoFresh: a.alsoFresh } : {}), now: t0 });
  if (fresh.length === 0) return { ok: true, waitedMs: 0 };
  const timeoutMs = a.timeoutMs ?? VISIBLE_TIMEOUT_MS;
  const pollMs = a.pollMs ?? VISIBLE_POLL_MS;
  const outOf = (f: FreshInput) => ({ transactionId: f.input.transactionId, index: f.input.index });
  const late = () => a.signal?.aborted || now() - t0 + pollMs > timeoutMs;
  const notInTime = (left: FreshInput[], lastError?: string): VisibleResult => ({ ok: false, waitedMs: now() - t0, missing: left.map(outOf), ...(lastError ? { lastError } : {}) });
  let waiting = false;
  const wait = (on: boolean) => {
    if (on === waiting) return;
    waiting = on;
    try {
      a.onWait?.(on);
    } catch {
      /* a UI callback must not abort the flow */
    }
  };

  try {
    // 1. The wallet's own node, asked through the wallet: an output it lists passes the wallet's
    // own check, whatever became of the announce. The UI hears of a wait only after a first "not
    // yet", so an output that is already visible opens the wallet at once.
    if (typeof a.adapter.walletSees === 'function') {
      const outs = fresh.map((f) => ({ ...outOf(f), address: a.addressOf(f.input.utxo.scriptPublicKey) }));
      for (;;) {
        let seen: boolean[];
        try {
          seen = await a.adapter.walletSees(outs, { timeoutMs: WALLET_SEES_TIMEOUT_MS });
          if (!Array.isArray(seen) || seen.length !== outs.length) throw new Error('walletSees answered for other outpoints');
        } catch {
          // The wallet cannot tell after all: the steps below decide, within the same time limit.
          break;
        }
        const left = fresh.filter((_, k) => seen[k] !== true);
        if (left.length === 0) return { ok: true, waitedMs: now() - t0 };
        if (late()) return notInTime(left);
        wait(true);
        await sleep(pollMs);
      }
    }

    // 2. announced to this wallet's node: visible there now
    const pending: FreshInput[] = [];
    for (const f of fresh) {
      const s = sent.get(f.input.transactionId.toLowerCase());
      if (s && s.walletId === a.adapter.id && s.announced && (await s.announced)) continue;
      pending.push(f);
    }
    if (pending.length === 0) return { ok: true, waitedMs: now() - t0 };

    // 3. listed by KOB's node (in a block), then the margin for the wallet's node
    wait(true);
    const want = pending.map((f) => ({ key: outKey(f.input.transactionId, f.input.index), address: a.addressOf(f.input.utxo.scriptPublicKey) }));
    const addresses = [...new Set(want.map((w) => w.address))];
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
      if (late()) {
        const left = new Set(missing.map((m) => m.key));
        return notInTime(pending.filter((f) => left.has(outKey(f.input.transactionId, f.input.index))), lastError);
      }
      await sleep(pollMs);
    }
    const youngest = Math.max(...pending.map((f) => f.at));
    if (now() - youngest < SETTLED_AFTER_MS) await sleep(a.settleMs ?? SETTLE_MS);
    return { ok: true, waitedMs: now() - t0 };
  } finally {
    wait(false);
  }
}
