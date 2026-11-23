// Loads everything the balances panels need for one wallet key. That is KAS from the node, free
// tokens from the tracker (indexer proposal and local candidates, verified on the node), live
// orders from the indexer for the escrowed part, and the stray list. Every source may fail on its
// own. The result carries what it could get plus `errors`, so a down indexer degrades the panel
// instead of blanking it.
import type { Services } from '../../app/services';
import type { OrderView, StrayView } from '../../data/indexer-types';
import type { OrderSnapshot } from '../../kob/cancel';
import type { TokenInfo } from '../../kob/registry';
import type { Hex, TokenProgram, TokenUtxo, Utxo } from '../../kob/types';
import type { LockKind } from '../../kob/creator-lock';
import { knownDeclarations, loadDeclaration, noteTokenProgram, restoreDeclarations } from '../../data/token-declarations';
import { toError } from '../kit/hooks';
import { combineBalances, type CombinedBalances } from './balances-model';

export type BalanceSource = 'kas' | 'tokens' | 'orders' | 'strays';
export interface BalanceError { source: BalanceSource; message: string }

export interface FreeBalances {
  /** Null when the node could not be read. */
  kasFree: bigint | null;
  freeTokens: TokenUtxo[];
  /**
   * The program of each token outside `tokens`, or without a program there, whose UTXOs were read
   * (`anyToken`). It is the one its scripts verified under.
   */
  programs: Map<Hex, TokenProgram>;
  /** The program of every token read, which covers registry tokens and `programs`. */
  programOf?: Map<Hex, TokenProgram>;
  /**
   * The wallet's tokens that its creator locks hold (`loadLockedHoldings`), read with `anyToken`.
   */
  locked?: LockedHolding[];
  errors: BalanceError[];
}

/**
 * The tokens a creator lock of the wallet holds (kob/creator-lock.ts), verified on the node. They
 * are not spendable before `until`.
 */
export interface LockedHolding {
  token: Hex;
  program: TokenProgram;
  /** Unix ms. */
  until: bigint;
  key: Hex;
  authority: Hex;
  utxos: TokenUtxo[];
  /** Base units. */
  amount: bigint;
  /** Manual (the wallet signs the unlock) or automatic (anyone may send the release). */
  kind: LockKind;
  /**
   * An automatic lock's coin, the genesis's plain KAS output at the lock's address, as its outpoint.
   * The release spends it with the locked genesis output. Null for a manual lock.
   */
  coin: Utxo | null;
}

/**
 * The wallet's locked tokens. The indexer names the tokens whose declaration locks a share for the
 * key (`GET /v1/creator-locks`). Their declarations are loaded and verified here, so a lock shows
 * without a visit to the token's page. Then, for every verified declaration known in this browser,
 * from memory and the proofs kept here, whose creator lock is the wallet key's, it reads the UTXOs
 * the lock holds, verified on the node (data/token-tracker.ts `lockedTokenUtxos`). Those are the
 * locked genesis outputs and what the indexer lists for the lock's owner. A token whose lock holds
 * nothing any more is left out.
 */
export async function loadLockedHoldings(s: Pick<Services, 'kob' | 'tracker' | 'config'> & Partial<Pick<Services, 'indexer'>>, pubkey: Hex): Promise<LockedHolding[]> {
  const network = s.config?.network;
  if (!network || typeof s.tracker.lockedTokenUtxos !== 'function') return [];
  restoreDeclarations(s.kob);
  const me = pubkey.toLowerCase();
  // The indexer's hint. Each named declaration is verified before it counts, so a wrong or missing
  // hint only hides a lock and never makes one.
  const named = s.indexer?.creatorLocks ? await s.indexer.creatorLocks(me).catch(() => null) : null;
  for (const n of named ?? []) await loadDeclaration(s.kob, s.indexer ?? null, network, n.covenant_id).catch(() => null);
  const mine = knownDeclarations(network).filter((d) => d.lock && d.lock.key === me && d.genesisTxid);
  const out: LockedHolding[] = [];
  for (const d of mine) {
    const l = d.lock!;
    const utxos = await s.tracker.lockedTokenUtxos({
      covenantId: d.covenantId, program: l.program, authority: l.authority, extensionCommitment: l.extensionCommitment, genesis: { txid: d.genesisTxid!, outputs: l.outputs },
    });
    const amount = utxos.reduce((a, u) => a + BigInt(u.state.amount), 0n);
    const coin = l.kind === 'auto' && l.coin ? { transactionId: d.genesisTxid!, index: l.coin.index, amount: l.coin.amount } : null;
    if (amount > 0n) out.push({ token: d.covenantId, program: l.program, until: BigInt(l.until), key: l.key, authority: l.authority, utxos, amount, kind: l.kind === 'auto' ? 'auto' : 'manual', coin });
  }
  return out;
}

/**
 * Spendable KAS (node) and free token UTXOs (tracker, verified on the node) of a key. The registry
 * `tokens` with a known program are read. With `anyToken` every other token the key holds is read
 * as well. These come from the indexer's holdings of the key and the local candidates, verified the
 * same way, and include tokens from other trading apps, launchpads and bridges. KRON UTXOs KOB
 * cannot spend (`id_type` 0) are read too, and only a balance shows them.
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
  const lockedP = o.anyToken ? loadLockedHoldings(s, pubkey).catch((e) => (fail('tokens', e), [] as LockedHolding[])) : Promise.resolve([] as LockedHolding[]);
  const [kasFree, freeTokens, held, locked] = await Promise.all([kasP, tokensP, heldP, lockedP]);
  const programs = new Map<Hex, TokenProgram>();
  for (const u of held) if (!read.has(u.covenantId)) programs.set(u.covenantId, u.program);
  // The program the node verified each held token under. The display of a token without a registry
  // entry takes its program's default decimals.
  const network = s.config?.network;
  if (network) for (const [id, p] of programs) noteTokenProgram(network, id, p);
  const programOf = new Map<Hex, TokenProgram>(programs);
  for (const t of tokens) if (t.program) programOf.set(t.covenantId, t.program);
  for (const l of locked) if (!programOf.has(l.token)) programOf.set(l.token, l.program);
  return { kasFree, freeTokens: [...freeTokens, ...held.map(({ program: _p, ...u }) => u)], programs, programOf, ...(locked.length ? { locked } : {}), errors };
}

/**
 * The combined balances of one key from its free balances (`loadFreeBalances`), live orders and
 * strays, with the program of every token read. A KRON program's holder owner decides which
 * key-held states are spendable (KForge: id_type 0). Without the programs a KForge holding reads
 * "not spendable by KOB".
 */
export function combineFreeBalances(free: FreeBalances, o: { maker: Hex; liveOrders: (OrderView | OrderSnapshot)[]; strays: StrayView[] }): CombinedBalances {
  return combineBalances({
    maker: o.maker,
    freeKas: free.kasFree ?? 0n,
    freeTokens: free.freeTokens,
    liveOrders: o.liveOrders,
    strays: o.strays,
    ...(free.programOf ? { programs: free.programOf } : {}),
    ...(free.locked?.length ? { locked: free.locked } : {}),
  });
}
