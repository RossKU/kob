// The labels the app gives the private-key signer frame for the tokens of a signing request (`signer/protocol.ts` TokenHint): the registry's
// ticker and decimals of every token the transaction names, and whether the app shows the token's KAS market inverted (KAS/TOKEN, the
// orientation the user chose on its page: the frame words an order the way the ticket did). They are labels only: the frame decodes the
// transaction itself and shows each token by its covenant id next to the label.
import { decodeSigning } from '../kob/decode';
import { displayToken } from './token-display';
import type { TokenRegistry } from '../kob/registry';
import type { BuiltTx, Hex } from '../kob/types';
import type { KobWasm } from '../kob/wasm';
import type { TokenHint } from '../signer/protocol';
import { tokensOf } from '../ui/confirm/confirm-model';
import { marketKey, readInverted } from '../ui/market/orientation';

/**
 * The tokens moved by the transaction (its token inputs and outputs) and the tokens of the orders it creates, amends or spends, decoded for
 * `maker` as the frame decodes them (a buy holds none of its token, a pair order names two): a buy's token was unnamed in the frame. A
 * transaction the decoder refuses keeps the moved tokens only.
 */
export function signerTokenHints(kob: KobWasm, built: BuiltTx, maker: Hex | null, registry: TokenRegistry, network?: string): TokenHint[] {
  const ids = new Set<string>();
  // the tokens the decoder names (not the order covenants among the outputs): a token the registry does not list is hinted as it is shown
  const tokens = new Set<string>();
  for (const i of built.tx.inputs) if (i.utxo.covenantId) ids.add(i.utxo.covenantId.toLowerCase());
  for (const o of built.tx.outputs) if (o.covenant) ids.add(o.covenant.covenantId.toLowerCase());
  if (maker) {
    try {
      for (const id of tokensOf(decodeSigning({ kob, built, maker, registry: null }))) {
        ids.add(id.toLowerCase());
        tokens.add(id.toLowerCase());
      }
    } catch {
      // the frame shows what it can decode itself; the hints are labels
    }
  }
  return [...ids].flatMap((id): TokenHint[] => {
    const inverted = readInverted(marketKey('token', id)) === true ? { inverted: true } : {};
    const tk = registry.byCovenantId.get(id);
    if (tk) return /^[\x20-\x7e]{1,24}$/.test(tk.ticker) ? [{ covenantId: id, ticker: tk.ticker, decimals: tk.decimals, ...inverted }] : [];
    if (!network || !tokens.has(id)) return [];
    // shown on its KCC-23 declaration or this browser's issuance of it, else on the assumed decimals (no ticker: the frame names it by its covenant id)
    const shown = displayToken(registry, network, id)!;
    const source = shown.decimalsSource === 'declared' || shown.decimalsSource === 'issued' ? shown.decimalsSource : null;
    return source && /^[\x20-\x7e]{1,24}$/.test(shown.ticker)
      ? [{ covenantId: id, ticker: shown.ticker, decimals: shown.decimals, source, ...inverted }]
      : [{ covenantId: id, ticker: '', decimals: shown.decimals, source: 'assumed', ...inverted }];
  });
}
