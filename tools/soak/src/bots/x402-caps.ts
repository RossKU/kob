// The soak's x402 price list and the payer's spend authorisation, kept together (pure, unit-tested): the SDK pays an offer only when
// the payer's capabilities carry an explicit ceiling (`maxAmount`) for the offer's merchant asset (and a `maxPay` bound, by pay asset, for a swap).
const KAS = 100_000_000n;

export interface SoakTokenFacts {
  covenantId: string;
  decimals: number;
}

/** base units of one whole token */
const whole = (m: SoakTokenFacts): bigint => 10n ** BigInt(m.decimals);

/** merchant asset and amount of each paid resource (`KAS` in sompi, the token in base units: a quarter of a whole token) */
export function soakPrices(m: SoakTokenFacts): Record<'/native' | '/token' | '/swap', { kind: 'native' | 'kcc20' | 'swap'; asset: string; amount: bigint }> {
  return {
    '/native': { kind: 'native', asset: 'KAS', amount: KAS / 2n },
    '/token': { kind: 'kcc20', asset: m.covenantId, amount: whole(m) / 4n },
    '/swap': { kind: 'swap', asset: 'KAS', amount: KAS / 2n },
  };
}

/** the KAS carrier a token payment delivers with the tokens (the paywall's default for a kcc20 offer) */
export const TOKEN_CARRIER_SOMPI = KAS;

/**
 * per merchant asset spend ceilings of the payer: 1 whole token (four times the token price), and a KAS ceiling that covers the
 * dearest KAS cost of one payment. The SDK holds everything a payment takes in KAS to the KAS ceiling: the price of a native
 * payment, and the carrier plus the network fee of a token payment (a token payment with a 1 KAS carrier is refused under a
 * 1 KAS ceiling: `spend_not_authorized`, seen on the 2026-10-08 redeploy). `maxFeeSompi` is the payer's fee cap.
 */
export function payerMaxAmount(m: SoakTokenFacts, maxFeeSompi: bigint): Record<string, string> {
  const native = soakPrices(m)['/native'].amount + maxFeeSompi;
  const token = TOKEN_CARRIER_SOMPI + maxFeeSompi;
  const kas = [KAS, native, token].reduce((a, b) => (b > a ? b : a));
  return { KAS: kas.toString(), [m.covenantId]: whole(m).toString() };
}

/** the swap's pay-side ceiling (token base units the payer may sell into bids for one payment): 5 whole tokens */
export function payerMaxPayAmount(m: SoakTokenFacts): string {
  return (whole(m) * 5n).toString();
}
