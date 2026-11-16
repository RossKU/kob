// Tokens a mode-1 order holds for its maker in its bought-custody (bid-delivery-spec §1, §10): the maker's own holdings,
// "bought, held by the order", never sell custody.
import type { Hex, OrderState } from './types';

const int = (v: unknown): bigint => (typeof v === 'string' && /^-?\d+$/.test(v) ? BigInt(v) : typeof v === 'bigint' ? v : 0n);

/** The bought-custody an order's state names: the bought token, its exact amount, the commitment and the next custody's carrier. */
export interface BoughtFact { token: Hex; amount: bigint; ext: Hex | null; carrier: bigint }

/** A mode-1 order with `bought > 0`: what its bought-custody holds; null otherwise (other modes, kinds, or nothing bought yet). */
export function boughtOf(o: OrderState | null | undefined): BoughtFact | null {
  if (!o) return null;
  const s = o.state as unknown as Record<string, unknown>;
  if (String(s.mode ?? '0') !== '1') return null;
  const amount = int(s.bought);
  if (amount <= 0n) return null;
  const carrier = int(s.deliveryCarrier);
  if (o.kind === 'KobBid' || o.kind === 'KobCondBid') {
    return { token: String(s.tokenCovId), amount, ext: typeof s.extensionCommitment === 'string' ? s.extensionCommitment : null, carrier };
  }
  if (o.kind === 'KobPair' || o.kind === 'KobCondPair') {
    // T, the token the order buys: A for a bid, B for an ask
    return { token: String(s.tCovId), amount, ext: typeof s.tExt === 'string' ? s.tExt : null, carrier };
  }
  return null;
}

/** The order's delivery mode ('0' when the state has none: KRON twins, asks, legacy layouts). */
export const deliveryModeOf = (o: OrderState | null | undefined): '0' | '1' | '2' => {
  const m = String((o?.state as { mode?: unknown } | undefined)?.mode ?? '0');
  return m === '1' || m === '2' ? m : '0';
};
