// The x402 payer's turns and the merchant's injected faults that drive the SDK's retry paths (docs/spec/x402-retry.md). Pure (no `@/` imports):
// unit-tested with plain `node --test`.
//
//   /retry/drop      0.5 KAS (native): the merchant DROPS the first paid request of each payment before the paywall sees it (the connection is
//                    closed): the payer has no answer, RE-SENDS the same signed artifact, and the paywall settles it then
//   /retry/lost      0.25 TUSD (KCC-20 exact): the paywall settles the first paid request, then the merchant LOSES the answer (500): the payer
//                    RE-SENDS the same artifact and the paywall answers it from memory (`replayed`), the payment is not settled twice
//   /retry/conflict  0.5 KAS paid with TUSD (swap): right after the payer's swap quote names a market-maker bid, the market maker amends that bid
//                    (cancel-replace in place, the web's amend): the facilitator's broadcast meets the conflict (`order_conflict`), the payer
//                    REBUILDS with a fresh quote under a new payment id, spending the first attempt's anchor, so at most one attempt is accepted

/** The payer's turn kinds: the three paid resources, the invoice paid by an intent, and the three retry scenarios. */
export const X402_KINDS = ['/native', '/token', '/swap', 'invoice', '/retry/drop', '/retry/lost', '/retry/conflict'] as const;
export type X402Kind = (typeof X402_KINDS)[number];

/** Default weights of the payer's turns: the plain payments as before (in turn: equal weights), a retry scenario every ~5 turns. */
export const DEFAULT_X402_WEIGHTS: Readonly<Record<X402Kind, number>> = {
  '/native': 3,
  '/token': 3,
  '/swap': 3,
  invoice: 3,
  '/retry/drop': 1,
  '/retry/lost': 1,
  '/retry/conflict': 1,
};

/** The weights of the payer's turns (config `x402.weights` over the defaults); `invoice` only when invoice intents are enabled. Unknown kinds are dropped. */
export function x402Weights(o: { weights?: Record<string, number>; invoices: boolean }): Record<string, number> {
  const out: Record<string, number> = {};
  for (const k of X402_KINDS) {
    const w = o.weights?.[k] ?? DEFAULT_X402_WEIGHTS[k];
    if (k === 'invoice' && !o.invoices) continue;
    if (Number.isFinite(w) && w > 0) out[k] = w;
  }
  return out;
}

/** The merchant resource a retry path is priced as (its offers). */
export const RETRY_BASE: Readonly<Record<string, '/native' | '/token' | '/swap'>> = {
  '/retry/drop': '/native',
  '/retry/lost': '/token',
  '/retry/conflict': '/swap',
};

/** The fault the merchant injects on a path: `drop` before the paywall, `lose` the answer after it settled. */
export const RETRY_FAULT: Readonly<Record<string, 'drop' | 'lose'>> = {
  '/retry/drop': 'drop',
  '/retry/lost': 'lose',
};

export type Fault = 'none' | 'drop' | 'lose';

/**
 * Decides the merchant's fault per paid request: the FIRST time a payment signature (the `PAYMENT-SIGNATURE` header: one signed artifact) is seen
 * on a fault path, its fault; every later send of the same artifact goes through. Unpaid requests (no header) and other paths: none. Remembers
 * at most `max` signatures (oldest forgotten first).
 */
export class FaultInjector {
  private readonly seen = new Set<string>();
  private readonly faults: Readonly<Record<string, 'drop' | 'lose'>>;
  private readonly max: number;
  constructor(faults: Readonly<Record<string, 'drop' | 'lose'>> = RETRY_FAULT, max = 2000) {
    this.faults = faults;
    this.max = max;
  }

  decide(path: string, signature: string | null | undefined): Fault {
    const f = this.faults[path];
    if (!f || !signature) return 'none';
    if (this.seen.has(signature)) return 'none';
    this.seen.add(signature);
    if (this.seen.size > this.max) this.seen.delete(this.seen.values().next().value as string);
    return f;
  }
}

/**
 * The race of `/retry/conflict`: armed before the payment, consumed by the FIRST swap quote (which names the bid to amend away); the outpoints
 * it raced are left out of every later quote (the rebuild must not quote the bid it lost).
 */
export class ConflictRace {
  private armed = false;
  readonly raced = new Set<string>();
  arm(): void {
    this.armed = true;
  }
  disarm(): void {
    this.armed = false;
  }
  /** true once after `arm`: this quote races its first eligible bid */
  take(): boolean {
    const a = this.armed;
    this.armed = false;
    return a;
  }
}
