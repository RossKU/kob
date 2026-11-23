// The connected wallet's node-verified token outputs, for the market list (held-list.ts). They come
// from the source the Balances panel reads (`TokenTracker.heldTokens`: the indexer's holdings of
// the key and this browser's candidates, each confirmed on the node), so the list adds no fetch
// path of its own. Without a wallet, or with one on another network, there are none: the rows they
// add are only ever in the connected wallet's own view.
import { useEffect, useRef, useState } from 'preact/hooks';
import { useServices, useWallet } from '../../app/context';
import { loadDeclaration, noteTokenProgram } from '../../data/token-declarations';
import type { HeldTokenUtxo } from '../../data/token-tracker';
import { logoGate } from '../../data/logo-gate';
import type { Hex } from '../../kob/types';
import { useAsync } from '../kit';
import { heldIds, holdingVerdict, throttledKey } from './held-list';
import { ownTokenIdsOf } from '../../kob/order-facts';
import type { PlacementRecord } from '../../kob/records';
import type { KobWasm } from '../../kob/wasm';

/** The tokens a record's order trades (its token, a pair's A and B, an unbound curve's), lower case. */
function recordTokens(kob: KobWasm | null, r: PlacementRecord): Set<string> {
  const out = new Set<string>();
  if (r.token) out.add(r.token.toLowerCase());
  for (const hex of [r.state, r.last?.state]) {
    if (!hex || !kob) continue;
    try {
      for (const t of ownTokenIdsOf(kob.decodeState(r.kind as never, hex))) out.add(t.toLowerCase());
    } catch {
      /* a state this build cannot decode names nothing */
    }
  }
  return out;
}

export interface WalletHoldings {
  /** Null without a connected wallet. Empty while it is read or when it could not be. */
  holdings: readonly HeldTokenUtxo[] | null;
  /** A wallet is connected on this network. */
  connected: boolean;
}

const NONE: readonly HeldTokenUtxo[] = [];
/**
 * A failed holdings read is tried again after these delays, then left: until one answers, the
 * gated logos wait. At most two more reads per wallet and list key.
 */
const RETRY_MS = [15_000, 60_000] as const;

/** `key`, taken at most once every `ms`: the newest value once the wait is over. */
export function useThrottledKey<T>(key: T, ms: number): T {
  const [shown, setShown] = useState<{ key: T; at: number }>(() => ({ key, at: Number.NEGATIVE_INFINITY }));
  const step = throttledKey(shown, key, Date.now(), ms);
  useEffect(() => {
    if (step.shown !== shown) return setShown(step.shown);
    if (step.wait === null) return;
    const h = setTimeout(() => setShown({ key, at: Date.now() }), step.wait);
    return () => clearTimeout(h);
  }, [shown, key, ms]);
  return step.shown.key;
}

/** `reloadKey`: a new value reads the holdings again, as the list's refresh does. */
export function useWalletHoldings(reloadKey: unknown): WalletHoldings {
  const { tracker, config } = useServices();
  const wallet = useWallet();
  const pubkey = wallet.networkMismatch ? null : (wallet.info?.pubkey ?? null);
  // A failed read leaves the list as the indexer gives it: the holdings only add to it. Each answer
  // names its key, so the last answer of a wallet that was switched away from is never shown as the
  // new one's while that one is read. The logo gate learns the answer too (data/logo-gate.ts): a
  // creator-hosted logo of a token the wallet holds is gated in the list as well, and every one
  // waits while the holdings are read. A failed read tells it nothing, so they keep waiting, and
  // it is tried again (RETRY_MS).
  const [retry, setRetry] = useState({ key: null as unknown, n: 0 });
  const tries = retry.key === reloadKey ? retry.n : 0;
  const read = useAsync(
    async (_signal) => {
      if (!pubkey || !tracker) return null;
      let ok = true;
      const list = await tracker.heldTokens(pubkey).catch((): HeldTokenUtxo[] => ((ok = false), []));
      if (ok) logoGate.noteHoldings(pubkey, '*', heldIds(list));
      return { pubkey, list, ok };
    },
    [pubkey, tracker, reloadKey, tries],
  );
  const failed = !!read.data && !read.data.ok && read.data.pubkey === pubkey;
  useEffect(() => {
    if (!failed || tries >= RETRY_MS.length) return;
    const h = setTimeout(() => setRetry({ key: reloadKey, n: tries + 1 }), RETRY_MS[tries]);
    return () => clearTimeout(h);
  }, [failed, tries, reloadKey]);
  const mine = read.data && read.data.pubkey === pubkey ? read.data.list : null;
  // The gate waits from the render that starts a read, before any icon of it asks for a logo.
  const asked = useRef<readonly unknown[]>([]);
  if (pubkey && tracker && (asked.current[0] !== pubkey || asked.current[1] !== tracker || asked.current[2] !== reloadKey)) {
    asked.current = [pubkey, tracker, reloadKey];
    logoGate.expectHoldings(pubkey, '*');
  }
  // The program each held token verified under: a token without a registry entry is then shown with
  // its program's default decimals elsewhere in the app, as the Balances panel notes them
  // (balances-data.ts).
  useEffect(() => {
    for (const h of mine ?? []) noteTokenProgram(config.network, h.covenantId, h.program);
  }, [mine, config.network]);
  return { holdings: pubkey ? (mine ?? NONE) : null, connected: !!pubkey };
}

/**
 * Asks the indexer once for the KCC-23 declaration of each of `ids` (verified before anything of it
 * is shown, data/token-declarations.ts), as the token page does for a token only the wallet's
 * holdings show. The list re-renders when one becomes known (`useDeclarationLookup`).
 */
export function useHeldDeclarations(ids: readonly string[]): void {
  const { kob, indexer, config } = useServices();
  const key = ids.join(',');
  useEffect(() => {
    for (const id of ids) void loadDeclaration(kob, indexer, config.network, id);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key, kob, indexer, config.network]);
}

/**
 * Asks whether the connected wallet holds `covenantId`, for the logo gate (data/logo-gate.ts), each
 * time a market page of it opens: a listed token's page reads no holdings of its own, and a unit
 * may have arrived since the list read them. It asks the indexer for the key's outputs of that
 * token, and the node-verified holdings when the indexer cannot answer. Any output named counts,
 * verified or not, since it only gates. A failed read tells the gate nothing, so its logo waits.
 *
 * A token in an order naming the key as its maker counts as held too: anyone can fund such an
 * order, and its custody is owned by the order, not the key. The check asks the indexer for one
 * order of the key under the token (base or a pair's quote) and reads the wallet's records, which
 * keep the orders it adopted from the indexer. A failed order check is no answer: unless another
 * check found the token, the gate is told nothing and the logo waits, and the checks run again
 * after RETRY_MS (at most two more times per page open).
 */
export function useHoldingCheck(covenantId: Hex): void {
  const { tracker, indexer, kob } = useServices();
  const wallet = useWallet();
  const pubkey = wallet.networkMismatch ? null : (wallet.info?.pubkey ?? null);
  const id = covenantId.toLowerCase();
  const key = pubkey ? `${pubkey}|${id}` : null;
  const asked = useRef<string | null>(null);
  if (pubkey && key !== asked.current) {
    asked.current = key;
    logoGate.expectHoldings(pubkey, id);
  }
  const records = wallet.records;
  const [retry, setRetry] = useState({ key: null as string | null, n: 0 });
  const tries = retry.key === key ? retry.n : 0;
  useEffect(() => {
    if (!pubkey) return;
    let timer: ReturnType<typeof setTimeout> | null = null;
    let live = true;
    const fromIndexer = indexer ? indexer.tokenUtxos({ owner: pubkey, token: id, spent: false }).catch(() => null) : Promise.resolve(null);
    const inOrders = indexer ? indexer.orders({ maker: pubkey, token: id, limit: 1 }).then((p) => p.items.some((o) => o.maker === pubkey), (): null => null) : Promise.resolve(false);
    const inRecords = records.list().then((rs) => rs.some((r) => recordTokens(kob, r).has(id)), () => false);
    void Promise.all([inOrders, inRecords]).then(([o, r]) => {
      if (o === true || r) logoGate.noteRelated(pubkey, [id]);
    });
    void fromIndexer
      // A list cut at its page bound that does not name the token is no answer: the tracker decides.
      .then(async (views) => {
        if (views?.some((v) => !v.spent && v.token.toLowerCase() === id && v.amount !== '0')) return true;
        return views && !views.truncated ? false : heldIds(await tracker.heldTokens(pubkey)).has(id);
      })
      .then(async (holds) => {
        // The gate is told once the order checks answered too, so the logo waits for them.
        const [o, r] = await Promise.all([inOrders, inRecords]);
        const held = holdingVerdict(holds, o, r);
        if (held !== null) return logoGate.noteHoldings(pubkey, id, held ? [id] : []);
        if (live && tries < RETRY_MS.length) timer = setTimeout(() => setRetry({ key, n: tries + 1 }), RETRY_MS[tries]);
      }, () => undefined);
    return () => {
      live = false;
      if (timer) clearTimeout(timer);
    };
  }, [pubkey, id, indexer, tracker, records, kob, tries]);
}
