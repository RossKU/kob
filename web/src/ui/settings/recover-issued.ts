// "Find a token you created earlier" (Settings > Token tracker): the issuance outputs of a token created before its genesis named their states
// (docs/spec/token-metadata.md, 3) are known to the browser that created them and to nobody else. The issuer can say what the state is (the amount
// the creation gave their key), and the node confirms it: the script of the state either holds the output or it does not.
import type { TokenTracker, TrackedToken } from '../../data/token-tracker';
import { loadDeclaration, type DeclaredToken } from '../../data/token-declarations';
import type { Hex } from '../../kob/types';
import type { KobWasm } from '../../kob/wasm';
import { tryParseUnits } from '../../kob/units';

export type RecoverResult =
  | { kind: 'found'; found: TrackedToken[] }
  /** `id`: not a covenant id; `amount`: not an amount of the token's decimals; `decimals`: the token declares none, ask for them; `none`: the node holds no such output */
  | { kind: 'error'; code: 'id' | 'amount' | 'decimals' | 'none' };

export interface RecoverDeps {
  kob: KobWasm;
  tracker: Pick<TokenTracker, 'recoverIssued'>;
  /** where a declaration is asked for (the indexer) */
  declarations: Parameters<typeof loadDeclaration>[1];
  network: string;
  /** looks the declaration up (tests); default: the indexer's, verified */
  declaration?: (id: Hex) => Promise<DeclaredToken | null>;
}

/** Decimals the user typed (a whole number 0..18), else null. */
export function parseDecimalsInput(text: string): number | null {
  const s = text.trim();
  if (!/^\d{1,2}$/.test(s)) return null;
  const n = Number(s);
  return n <= 18 ? n : null;
}

/**
 * Looks for the issuance outputs of `idText` that gave `pubkey` `amountText` tokens (whole tokens, as the create form showed it). The decimals
 * are the token's declared ones, else `decimalsText` (asked when the token declares none).
 */
export async function recoverIssuedToken(d: RecoverDeps, pubkey: Hex, idText: string, amountText: string, decimalsText = ''): Promise<RecoverResult> {
  const id = idText.trim().toLowerCase();
  if (!/^[0-9a-f]{64}$/.test(id)) return { kind: 'error', code: 'id' };
  const decl = await (d.declaration ? d.declaration(id) : loadDeclaration(d.kob, d.declarations, d.network, id));
  const decimals = decl?.decimals ?? parseDecimalsInput(decimalsText);
  if (decimals === null || decimals === undefined) return { kind: 'error', code: 'decimals' };
  const amount = tryParseUnits(amountText, decimals);
  if (amount === null || amount <= 0n) return { kind: 'error', code: 'amount' };
  const found = await d.tracker.recoverIssued(pubkey, { covenantId: id }, [amount]);
  return found.length > 0 ? { kind: 'found', found } : { kind: 'error', code: 'none' };
}
