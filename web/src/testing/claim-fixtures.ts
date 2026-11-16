// Claims of a mode-1 order's bought-custody built by kob-wasm `claimOrder` (bid-delivery-spec §7) and run in the script engine.
import { boughtOf } from '../kob/decode';
import type { OrderSnapshot } from '../kob/cancel';
import type { BuiltTx, Kcc20State, OrderState, TokenUtxo } from '../kob/types';
import { loadKobNode } from '../kob/wasm.node';
import { MAKER, keyUtxo, placeGolden, signAndValidate, strayFor } from './chain-fixtures';

const kob = loadKobNode();
const KAS = 100_000_000n;
/** What every fixture order has bought. */
export const BOUGHT = 700n;
const clone = <T>(v: T): T => JSON.parse(JSON.stringify(v)) as T;

/** A placed order of a golden placement, holding a bought-custody of `bought` (mode 1). */
export function holding(name: string, bought = BOUGHT): { snap: OrderSnapshot; custody: TokenUtxo } {
  const placed = placeGolden(kob, name).snapshots[0]!;
  const order = clone(placed.order);
  Object.assign(order.state.state, { mode: '1', recvThreshold: '0', topupTip: '0', bought: bought.toString() });
  const snap = { ...placed, order };
  const b = boughtOf(order.state)!;
  const custody = strayFor(snap, bought, 90, 20_000_000n);
  custody.covenantId = b.token;
  (custody.state as Kcc20State).extension_commitment = b.ext;
  return { snap, custody };
}

/** The golden ask of the same token: `amount` of the claimed tokens placed for sale. */
export function askOf(amount: bigint): OrderState {
  const ask = clone(placeGolden(kob, 'create.ask').recovered[0]!.order as OrderState);
  (ask.state as unknown as Record<string, string>).amountLeft = amount.toString();
  return ask;
}

export interface ClaimOpts { tokens?: TokenUtxo[]; place?: OrderState; end?: boolean }
export function claim(name: string, o: ClaimOpts = {}): { built: BuiltTx; snap: OrderSnapshot; custody: TokenUtxo } {
  const { snap, custody } = holding(name);
  const req = {
    action: 'claimOrder', order: snap.order, boughtCustody: custody, tokens: o.tokens ?? [], end: !!o.end,
    ...(o.place ? { place: { order: o.place, value: (10n * KAS).toString() } } : {}),
    funding: [keyUtxo(MAKER.pk, 50n * KAS, 201)],
  };
  const built = kob.build(req as never);
  signAndValidate(kob, built, [MAKER.sk]);
  return { built, snap, custody };
}

