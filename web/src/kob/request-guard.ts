// Ties every destination key of a build request to the wallet.
//
// The Rust builders do not know who the wallet is. An order `maker`, a `change` key or a
// replacement maker that belongs to someone else builds fine, and only the funder signs. The web
// layer therefore checks before `kob.build` that every such key is the wallet key or the change
// address the user chose. It passes the same list to kob-wasm as `ownKeys`, so the Rust side
// refuses a mismatch too.
import type { Hex, OrderState } from './types';
import { KobError } from './wasm';

/** The keys a build request may name as destinations. */
export interface OwnKeys {
  /** The wallet's x-only key. */
  maker: Hex;
  /** A change address the user chose explicitly. It is allowed as a change destination only. */
  changeTo?: Hex | null;
}

/** A request as the planners assemble it, for any builder action including the refund request. */
type AnyRequest = Record<string, unknown> & { action?: unknown };

const asKey = (v: unknown): Hex | null => (typeof v === 'string' ? v : null);
const makerOf = (o: unknown): Hex | null => asKey(((o as OrderState | undefined)?.state as { maker?: unknown } | undefined)?.maker);

/** Returns a readable problem when a destination key of `req` is not the wallet's, else null. */
export function requestKeyProblem(req: unknown, keys: OwnKeys): string | null {
  const r = req as AnyRequest;
  const own = new Set<Hex>([keys.maker]);
  const dest = new Set<Hex>([keys.maker]);
  if (keys.changeTo) dest.add(keys.changeTo);
  const bad = (what: string, k: Hex | null, allowed: ReadonlySet<Hex> = dest): string | null => (k !== null && !allowed.has(k) ? `${what} ${k.slice(0, 8)}... is not a key of the connected wallet` : null);

  const problems: (string | null)[] = [];
  const order = r.order as { state?: unknown } | undefined;
  if (r.action === 'createOrder') problems.push(bad('order maker', makerOf(order), own));
  const replace = r.replace as { order?: unknown } | null | undefined;
  if (replace) problems.push(bad('replacement order maker', makerOf(replace.order), own));
  // A cancel signs with the wallet key only, so the order must belong to the wallet.
  if (r.action === 'cancelOrder' || r.action === 'sweepOrder') {
    const m = makerOf((r.order as { state?: unknown } | undefined)?.state);
    problems.push(m !== null && m !== keys.maker ? `the order to ${r.action === 'sweepOrder' ? 'sweep' : 'cancel'} belongs to ${m.slice(0, 8)}..., not to the wallet` : null);
  }
  // An amend must start from the wallet's order and leave it with the wallet.
  if (r.action === 'amendOrder') {
    const m = makerOf((r.order as { state?: unknown } | undefined)?.state);
    problems.push(m !== null && m !== keys.maker ? `the order to amend belongs to ${m.slice(0, 8)}..., not to the wallet` : null);
    problems.push(bad('amended order maker', makerOf(r.amended), own));
  }
  if (r.action === 'cancelPosition') {
    for (const it of (r.orders as { order?: unknown }[] | undefined) ?? []) {
      const m = makerOf((it.order as { state?: unknown } | undefined)?.state);
      if (m !== null && m !== keys.maker) problems.push(`an order to cancel belongs to ${m.slice(0, 8)}..., not to the wallet`);
    }
  }
  for (const f of (r.funding as { pubkey?: unknown }[] | undefined) ?? []) if (asKey(f.pubkey) !== null && !own.has(f.pubkey as Hex)) problems.push(`funding input of ${String(f.pubkey).slice(0, 8)}... is not a key of the connected wallet`);
  problems.push(bad('change key', asKey(r.change)));
  problems.push(bad('token change key', asKey(r.tokenChange)));
  return problems.find((p) => p !== null) ?? null;
}

/**
 * Checks the request and returns it with `ownKeys` set for kob-wasm.
 * Throws a KobError, which the planners turn into a plan issue, when a destination key is foreign.
 */
export function guardRequest<T extends object>(req: T, keys: OwnKeys): T & { ownKeys: Hex[] } {
  const problem = requestKeyProblem(req, keys);
  if (problem) throw new KobError(`refused before building: ${problem}`, 'build');
  const ownKeys = [keys.maker, ...(keys.changeTo && keys.changeTo !== keys.maker ? [keys.changeTo] : [])];
  return { ...req, ownKeys };
}
