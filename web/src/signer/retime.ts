// The rule for a transaction re-timed after the user approved it. Pure.
//
// A re-timed order is planned again with the current clock, because the approval came after its
// auction should have started (wallet/sign.ts `submitByDaa`). The signer frame signs the re-timed
// one without asking again only when its own decoding of both shows that nothing the user agreed
// to got worse:
// - the same orders of the same token, side and amount,
// - prices unchanged or better for the user,
// - only the time fields later,
// - the KAS the wallet spends, the fee and what goes to other keys within a tiny tolerance.
// The signer frame also takes a re-time only of an approved auction (`retimable`), and only one
// that spends exactly the approved wallet inputs (`signedOutpoints`), so copies cannot add up.
//
// Anything else is shown again. The app's popup for an external wallet uses the same rule.
import type { DecodedOrder, SigningSummary } from '../kob/decode';
import type { OrderDescription } from '../kob/order-facts';
import type { BuiltTx } from '../kob/types';

/**
 * How much more KAS, in sompi, a re-timed transaction may take from the wallet or pay as fee than
 * the approved one. Inputs and change move by a few sompi.
 */
export const RETIME_TOLERANCE_SOMPI = 10_000n;
/** How long after an approval a re-timed transaction of it is still taken without asking again. */
export const RETIME_WINDOW_MS = 120_000;
/** How many re-timed transactions one approval covers without asking again. */
export const RETIME_MAX_COUNT = 8;
/**
 * How much later, in DAA, a re-timed order may start than the approved one. It is 150 s at 10
 * DAA/s, a little more than the 120 s window. A slow approval moves times by seconds, never by
 * minutes or hours.
 */
export const RETIME_MAX_DELAY_DAA = 1_500n;

const json = (v: unknown): string => JSON.stringify(v, (_k, x) => (typeof x === 'bigint' ? `${x}n` : x));

/** The terms of an order that must not change at all, which are all but its times and prices. */
function fixedTerms(d: OrderDescription): string {
  const { activeFrom: _a, expiryDaa: _e, price: _p, auction, ...rest } = d;
  void _a;
  void _e;
  void _p;
  // The decay speed (`slope` per `decayStep`) is checked in `orderKept`. It may be slower, never
  // faster.
  return json({ ...rest, auction: auction ? { interval: auction.interval, maxFill: auction.maxFill } : null });
}

/**
 * Checks one order of the re-timed transaction against the approved one. It needs the same terms,
 * times not earlier and prices not worse for its side.
 */
function orderKept(a: DecodedOrder, b: DecodedOrder): boolean {
  const x = a.description;
  const y = b.description;
  if (fixedTerms(x) !== fixedTerms(y) || json(a.delivery) !== json(b.delivery)) return false;
  if (y.activeFrom < x.activeFrom || (x.expiryDaa > 0n && y.expiryDaa < x.expiryDaa) || (x.expiryDaa === 0n) !== (y.expiryDaa === 0n)) return false;
  // The times also move by no more than a slow approval can explain. Without an auction, an expiry
  // moves with the start, for a lifetime counted from it, or not at all, for an explicit date. It
  // never moves alone by its own amount. An auction's expiry follows the length of the auction,
  // which the price it starts from changes, so it is only capped.
  const dStart = y.activeFrom - x.activeFrom;
  if (dStart > RETIME_MAX_DELAY_DAA) return false;
  if (x.expiryDaa > 0n) {
    const dExpiry = y.expiryDaa - x.expiryDaa;
    if (dExpiry > RETIME_MAX_DELAY_DAA || (!x.auction && dExpiry !== 0n && dExpiry !== dStart)) return false;
  }
  const buy = x.side === 'buy';
  const better = (old: bigint, now: bigint) => (buy ? now <= old : now >= old);
  if (!better(x.price, y.price)) return false;
  if (!!x.auction !== !!y.auction) return false;
  if (x.auction && y.auction && !better(x.auction.priceEnd, y.auction.priceEnd)) return false;
  // The price may not fall, for a sell, or rise, for a buy, faster per DAA than approved. The same
  // auction from a better start is slower, and one with a steeper slope would reach the bound
  // sooner. The speeds `slope / decayStep` are compared by cross-multiplication.
  //
  // A slope of 0 is no decay at all. The covenant asks for a decay step only when there is a
  // slope, so an approved order with slope 0 and step 0 is flat. A step of 0 counts as 1 in the
  // comparison, so that a flat order cannot become a steep one.
  if (x.auction && y.auction) {
    const step = (a: { decayStep: bigint }) => (a.decayStep > 0n ? a.decayStep : 1n);
    if (y.auction.slope * step(x.auction) > x.auction.slope * step(y.auction)) return false;
  }
  // What the order holds. A buy's escrow, its most to pay, is never larger beyond the tolerance. A
  // sell's token custody amount stays the same.
  return b.value <= a.value + RETIME_TOLERANCE_SOMPI && (a.custody?.amount ?? null) === (b.custody?.amount ?? null);
}

/**
 * Tells whether `next`, the re-timed transaction, keeps everything the user approved in
 * `approved`, as the two decodings say.
 */
export function sameOrBetter(approved: SigningSummary, next: SigningSummary): boolean {
  // No finding the approved one did not have. The frame decodes without the app's claims, so a
  // send to another key is always a finding, and the user saw it.
  const known = new Set(approved.blocking.map((b) => b.code));
  if (next.blocking.some((b) => !known.has(b.code)) || approved.kind !== next.kind) return false;
  const ao = approved.orders.filter((o) => o.sweptFrom === undefined && o.claimedFrom === undefined);
  const bo = next.orders.filter((o) => o.sweptFrom === undefined && o.claimedFrom === undefined);
  if (ao.length !== bo.length || !ao.every((o, i) => orderKept(o, bo[i]!))) return false;
  // The orders it closes or claims, and the tokens it moves for the wallet and to others, stay the
  // same.
  if (json(approved.spends.map((s) => [s.covenantId, s.action])) !== json(next.spends.map((s) => [s.covenantId, s.action]))) return false;
  const tokens = (s: SigningSummary) => json(s.net.tokens.map((t) => [t.ref.covenantId, t.walletDelta, t.toOthers, t.escrowed]).sort());
  if (tokens(approved) !== tokens(next)) return false;
  // So does every output to another key, with its recipient and what it carries.
  const others = (s: SigningSummary) => json(s.outputs.filter((o) => o.recipient !== null).map((o) => [o.kind, o.recipient, o.value, o.token?.ref.covenantId ?? null, o.token?.amount ?? null]).sort());
  if (others(approved) !== others(next)) return false;
  const a = approved.net.kas;
  const b = next.net.kas;
  return b.toOthers <= a.toOthers && b.walletDelta >= a.walletDelta - RETIME_TOLERANCE_SOMPI && b.fee <= a.fee + RETIME_TOLERANCE_SOMPI;
}

/**
 * Whether an approved transaction may stand for re-timed ones at all. It must place an auction (a
 * start and a decay), the only order whose start a slow approval can pass (wallet/sign.ts
 * `auctionSubmitBy`). Anything else is never re-timed by the app.
 */
export function retimable(s: SigningSummary): boolean {
  return s.orders.some((o) => o.sweptFrom === undefined && o.claimedFrom === undefined && o.description.activeFrom > 0n && !!o.description.auction && o.description.auction.slope > 0n);
}

/**
 * The inputs the wallet signs in `built`, as sorted outpoints. A re-timed copy must spend exactly
 * the approved transaction's, so the approval and its copies conflict and at most one confirms.
 */
export function signedOutpoints(built: Pick<BuiltTx, 'tx' | 'sign'>): string[] {
  return built.sign.map((r) => {
    const i = built.tx.inputs[r.inputIndex];
    return i ? `${i.transactionId.toLowerCase()}:${i.index}` : `?${r.inputIndex}`;
  }).sort();
}
