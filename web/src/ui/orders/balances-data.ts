// Loads everything the balances panels need for one wallet key: KAS (node), free tokens (tracker: indexer proposal + local candidates, verified
// on the node), live orders (indexer) for the escrowed part, and the stray list. Every source may fail on its own: the result carries what it could
// get plus `errors`, so a down indexer degrades the panel instead of blanking it.
import type { Services } from '../../app/services';
import type { OrderView, StrayView } from '../../data/indexer-types';
import type { OrderSnapshot } from '../../kob/cancel';
import type { TokenInfo } from '../../kob/registry';
import type { Hex, TokenProgram, TokenUtxo } from '../../kob/types';
import { noteTokenProgram } from '../../data/token-declarations';
import { toError } from '../kit/hooks';
import { combineBalances, type CombinedBalances } from './balances-model';

export type BalanceSource = 'kas' | 'tokens' | 'orders' | 'strays';
export interface BalanceError { source: BalanceSource; message: string }

export interface FreeBalances {
  /** null when the node could not be read */
  kasFree: bigint | null;
  freeTokens: TokenUtxo[];
  /** the program of each token outside `tokens` (or without a program there) whose UTXOs were read (`anyToken`): the one its scripts verified under */
  programs: Map<Hex, TokenProgram>;
  /** the program of every token read (registry tokens and `programs`) */
  programOf?: Map<Hex, TokenProgram>;
  errors: BalanceError[];
}

/**
 * Spendable KAS (node) and free token UTXOs (tracker, verified on the node) of a key. The registry `tokens` with a known program are read; with
 * `anyToken` every other token the key holds as well (the indexer's holdings of the key and the local candidates, verified the same way: tokens
 * from other DEXes, launchpads, bridges), and KRON UTXOs KOB cannot spend (`id_type` 0), which only a balance shows.
 */
export async function loadFreeBalances(s: Services, pubkey: Hex, tokens: readonly TokenInfo[], o: { anyToken?: boolean } = {}): Promise<FreeBalances> {
  const errors: BalanceError[] = [];
  const fail = (source: BalanceSource, e: unknown) => errors.push({ source, message: toError(e).message });
  const kasP = s.utxos.fundingFor(pubkey).then(
    (u) => u.reduce((sum, x) => sum + BigInt(x.amount), 0n),
    (e) => (fail('kas', e), null),
  );
  const tokensP = Promise.all(
    tokens
      .filter((t) => t.program)
      .map((t) =>
        s.tracker
          .tokenUtxosFor(pubkey, { covenantId: t.covenantId, program: t.program!, ...(t.extensionCommitment ? { extensionCommitment: t.extensionCommitment } : {}) })
          .catch((e) => (fail('tokens', e), [] as TokenUtxo[])),
      ),
  ).then((lists) => lists.flat());
  const read = new Set(tokens.filter((t) => t.program).map((t) => t.covenantId));
  const heldP = o.anyToken ? s.tracker.heldTokens(pubkey, { skip: read }).catch((e) => (fail('tokens', e), [])) : Promise.resolve([]);
  const [kasFree, freeTokens, held] = await Promise.all([kasP, tokensP, heldP]);
  const programs = new Map<Hex, TokenProgram>();
  for (const u of held) if (!read.has(u.covenantId)) programs.set(u.covenantId, u.program);
  // the program the node verified each held token under: the display of a token without a registry entry takes its program's default decimals
  const network = s.config?.network;
  if (network) for (const [id, p] of programs) noteTokenProgram(network, id, p);
  const programOf = new Map<Hex, TokenProgram>(programs);
  for (const t of tokens) if (t.program) programOf.set(t.covenantId, t.program);
  return { kasFree, freeTokens: [...freeTokens, ...held.map(({ program: _p, ...u }) => u)], programs, programOf, errors };
}

/**
 * The combined balances of one key from its free balances (`loadFreeBalances`), live orders and strays, with the program of every token read: a
 * KRON program's holder owner decides which key-held states are spendable (KForge: id_type 0), so without the programs a KForge holding reads
 * "not spendable by KOB" (TN10 2026-10-15 B-15: My orders' Balances did not pass them).
 */
export function combineFreeBalances(free: FreeBalances, o: { maker: Hex; liveOrders: (OrderView | OrderSnapshot)[]; strays: StrayView[] }): CombinedBalances {
  return combineBalances({
    maker: o.maker,
    freeKas: free.kasFree ?? 0n,
    freeTokens: free.freeTokens,
    liveOrders: o.liveOrders,
    strays: o.strays,
    ...(free.programOf ? { programs: free.programOf } : {}),
  });
}
