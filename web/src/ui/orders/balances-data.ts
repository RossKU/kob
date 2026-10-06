// Loads everything the balances panels need for one wallet key: KAS (node), free tokens (tracker: indexer proposal + local candidates, verified
// on the node), live orders (indexer) for the escrowed part, and the stray list. Every source may fail on its own: the result carries what it could
// get plus `errors`, so a down indexer degrades the panel instead of blanking it.
import type { Services } from '../../app/services';
import type { OrderView, StrayView } from '../../data/indexer-types';
import type { OrderSnapshot } from '../../kob/cancel';
import type { TokenInfo } from '../../kob/registry';
import type { Hex, TokenUtxo } from '../../kob/types';
import { toError } from '../kit/hooks';
import { combineBalances, type CombinedBalances } from './balances-model';

export type BalanceSource = 'kas' | 'tokens' | 'orders' | 'strays';
export interface BalanceError { source: BalanceSource; message: string }

export interface FreeBalances {
  /** null when the node could not be read */
  kasFree: bigint | null;
  freeTokens: TokenUtxo[];
  errors: BalanceError[];
}

/** Spendable KAS (node) and free token UTXOs (tracker, verified on the node) of a key. Only tokens with a known program can be read. */
export async function loadFreeBalances(s: Services, pubkey: Hex, tokens: readonly TokenInfo[]): Promise<FreeBalances> {
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
  const [kasFree, freeTokens] = await Promise.all([kasP, tokensP]);
  return { kasFree, freeTokens, errors };
}

export interface BalancesData {
  combined: CombinedBalances;
  /** the wallet's spendable KAS could be read (false: the node failed, the KAS figure is not real) */
  kasKnown: boolean;
  /** live orders came from the indexer (false: the escrowed figures come only from `extraSnapshots`) */
  ordersKnown: boolean;
  errors: BalanceError[];
  /** live orders used */
  liveOrders: OrderView[];
}

export interface BalancesInput {
  /** tokens whose FREE balance to read */
  tokens: readonly TokenInfo[];
  /** live orders resolved elsewhere (e.g. from placement records on the node); used next to the indexer's */
  extraSnapshots?: readonly OrderSnapshot[];
  /** restrict the indexer queries to one token */
  token?: Hex;
  signal?: AbortSignal;
}

/** Free + escrowed + strays for a key, querying the indexer itself (the market page: one token). */
export async function loadBalances(s: Services, pubkey: Hex, o: BalancesInput): Promise<BalancesData> {
  const sig = o.signal ? { signal: o.signal } : {};
  const errors: BalanceError[] = [];
  const fail = (source: BalanceSource, e: unknown) => errors.push({ source, message: toError(e).message });
  const freeP = loadFreeBalances(s, pubkey, o.tokens);
  const ordersP: Promise<OrderView[] | null> = s.indexer
    ? s.indexer.allOrders({ maker: pubkey, status: 'active', ...(o.token ? { token: o.token } : {}) }, sig).catch((e) => (fail('orders', e), null))
    : Promise.resolve(null);
  const straysP: Promise<StrayView[]> = s.indexer ? s.indexer.strays({ maker: pubkey }, sig).catch((e) => (fail('strays', e), [] as StrayView[])) : Promise.resolve([]);
  const [free, orders, strays] = await Promise.all([freeP, ordersP, straysP]);
  const liveOrders = orders ?? [];
  // snapshots of orders the indexer does not list (fresh placements, indexer outage) count as well, without duplicating an indexer entry
  const known = new Set(liveOrders.map((v) => v.covenant_id));
  const extra = (o.extraSnapshots ?? []).filter((x) => !known.has(x.covenantId));
  const scoped = o.token ? strays.filter((x) => x.token === o.token) : strays;
  return {
    combined: combineBalances({ maker: pubkey, freeKas: free.kasFree ?? 0n, freeTokens: free.freeTokens, liveOrders: [...liveOrders, ...extra], strays: scoped }),
    kasKnown: free.kasFree !== null,
    ordersKnown: orders !== null,
    errors: [...free.errors, ...errors],
    liveOrders,
  };
}
