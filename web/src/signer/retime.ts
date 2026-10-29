// A transaction re-timed after the user approved it (the order was planned again with the current clock because the approval came after its
// auction should have started: wallet/sign.ts `submitByDaa`). The signer frame signs the re-timed one WITHOUT asking again only when its own
// decoding of both shows that nothing the user agreed to got worse: the same orders of the same token, side and amount, prices unchanged or
// better for the user, only the time fields later, and the KAS the wallet spends, the fee and what goes to other keys within a tiny tolerance.
// Anything else is shown again (the app's popup for an external wallet uses the same rule). Pure.
import type { DecodedOrder, SigningSummary } from '../kob/decode';
import type { OrderDescription } from '../kob/order-facts';

/** How much more KAS (sompi) a re-timed transaction may take from the wallet, or pay as fee, than the approved one: inputs and change move by a few sompi. */
export const RETIME_TOLERANCE_SOMPI = 10_000n;
/** How long after an approval a re-timed transaction of it is still taken without asking again. */
export const RETIME_WINDOW_MS = 120_000;
/** How many re-timed transactions one approval covers without asking again (audit 2026-10-29 S3: the count was unbounded). */
export const RETIME_MAX_COUNT = 8;
/**
 * How far later (DAA, 150 s at 10 DAA/s: a little more than the 120 s window) a re-timed order may start than the approved one: a slow approval moves
 * times by seconds, never by minutes or hours (S3; the first cap was 36,000 DAA, an hour, 30 times the window: audit 2026-10-30 G3).
 */
export const RETIME_MAX_DELAY_DAA = 1_500n;

const json = (v: unknown): string => JSON.stringify(v, (_k, x) => (typeof x === 'bigint' ? `${x}n` : x));

/** The terms of an order that must not change at all (everything but its times and its auction prices). */
function fixedTerms(d: OrderDescription): string {
  const { activeFrom: _a, expiryDaa: _e, price: _p, auction, ...rest } = d;
  void _a;
  void _e;
  void _p;
  // the decay speed (`slope` per `decayStep`) is checked in `orderKept`: it may be slower, never faster (audit 2026-10-30 G3)
  return json({ ...rest, auction: auction ? { interval: auction.interval, maxFill: auction.maxFill } : null });
}

/** One order of the re-timed transaction against the approved one: same terms, times not earlier, prices not worse for its side. */
function orderKept(a: DecodedOrder, b: DecodedOrder): boolean {
  const x = a.description;
  const y = b.description;
  if (fixedTerms(x) !== fixedTerms(y)) return false;
  if (y.activeFrom < x.activeFrom || (x.expiryDaa > 0n && y.expiryDaa < x.expiryDaa) || (x.expiryDaa === 0n) !== (y.expiryDaa === 0n)) return false;
  // and not by more than a slow approval can explain (S3). Without an auction an expiry moves with the start (a lifetime counted from it) or not at
  // all (an explicit date), never alone by its own amount; an auction's expiry follows the length of the auction, which the price it starts from
  // changes, so it is only capped (G3)
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
  // the price may not fall (a sell) or rise (a buy) faster per DAA than approved: the same auction from a better start is slower, one with a steeper
  // slope would have reached the bound sooner (G3; compared by cross-multiplication, `slope / decayStep` DAA)
  if (x.auction && y.auction && y.auction.slope * x.auction.decayStep > x.auction.slope * y.auction.decayStep) return false;
  // what the order holds: a buy's escrow (its most to pay) never larger, beyond the tolerance; the KAS of a sell's carriers the same
  return b.value <= a.value + RETIME_TOLERANCE_SOMPI && (a.custody?.amount ?? null) === (b.custody?.amount ?? null);
}

/** Whether `next` (re-timed) keeps everything the user approved in `approved`, as the two decodings say. */
export function sameOrBetter(approved: SigningSummary, next: SigningSummary): boolean {
  // no finding the approved one did not have (the frame decodes without the app's claims: a send to another key is always one, and was shown)
  const known = new Set(approved.blocking.map((b) => b.code));
  if (next.blocking.some((b) => !known.has(b.code)) || approved.kind !== next.kind) return false;
  const ao = approved.orders.filter((o) => o.sweptFrom === undefined);
  const bo = next.orders.filter((o) => o.sweptFrom === undefined);
  if (ao.length !== bo.length || !ao.every((o, i) => orderKept(o, bo[i]!))) return false;
  // the orders it closes, the tokens it moves for the wallet and to others: the same
  if (json(approved.spends.map((s) => [s.covenantId, s.action])) !== json(next.spends.map((s) => [s.covenantId, s.action]))) return false;
  const tokens = (s: SigningSummary) => json(s.net.tokens.map((t) => [t.ref.covenantId, t.walletDelta, t.toOthers, t.escrowed]).sort());
  if (tokens(approved) !== tokens(next)) return false;
  // and to whom: every output to another key, its recipient and what it carries
  const others = (s: SigningSummary) => json(s.outputs.filter((o) => o.recipient !== null).map((o) => [o.kind, o.recipient, o.value, o.token?.ref.covenantId ?? null, o.token?.amount ?? null]).sort());
  if (others(approved) !== others(next)) return false;
  const a = approved.net.kas;
  const b = next.net.kas;
  return b.toOthers <= a.toOthers && b.walletDelta >= a.walletDelta - RETIME_TOLERANCE_SOMPI && b.fee <= a.fee + RETIME_TOLERANCE_SOMPI;
}
