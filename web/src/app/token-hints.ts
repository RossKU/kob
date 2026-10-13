// The labels the app gives the private-key signer frame for the tokens of a signing request (`signer/protocol.ts` TokenHint): the registry's
// ticker and decimals of every token the transaction names. They are labels only: the frame decodes the transaction itself and shows each
// token by its covenant id next to the label.
import { decodeSigning } from '../kob/decode';
import type { TokenRegistry } from '../kob/registry';
import type { BuiltTx, Hex } from '../kob/types';
import type { KobWasm } from '../kob/wasm';
import type { TokenHint } from '../signer/protocol';
import { tokensOf } from '../ui/confirm/confirm-model';

/**
 * The tokens moved by the transaction (its token inputs and outputs) and the tokens of the orders it creates, amends or spends, decoded for
 * `maker` as the frame decodes them (a buy holds none of its token, a pair order names two): a buy's token was unnamed in the frame. A
 * transaction the decoder refuses keeps the moved tokens only.
 */
export function signerTokenHints(kob: KobWasm, built: BuiltTx, maker: Hex | null, registry: TokenRegistry): TokenHint[] {
  const ids = new Set<string>();
  for (const i of built.tx.inputs) if (i.utxo.covenantId) ids.add(i.utxo.covenantId.toLowerCase());
  for (const o of built.tx.outputs) if (o.covenant) ids.add(o.covenant.covenantId.toLowerCase());
  if (maker) {
    try {
      for (const id of tokensOf(decodeSigning({ kob, built, maker, registry: null }))) ids.add(id.toLowerCase());
    } catch {
      // the frame shows what it can decode itself; the hints are labels
    }
  }
  return [...ids].flatMap((id) => {
    const tk = registry.byCovenantId.get(id);
    return tk && /^[\x20-\x7e]{1,24}$/.test(tk.ticker) ? [{ covenantId: id, ticker: tk.ticker, decimals: tk.decimals }] : [];
  });
}
