// Test kit of the pair orders (KobPair, KobCondPair, KobIfdPair): the golden pair vectors of every family mix, re-keyed to the test maker, built,
// signed and validated by kob-wasm (the script engine runs every covenant), and the live snapshots of what a transaction leaves behind (a
// continuation, a new exit, their custodies of A and / or B), found by deriving each candidate state's script and matching the outputs.
import type { OrderSnapshot } from '../kob/cancel';
import { custodiesOf } from '../kob/order-facts';
import type { BuiltTx, Hex, OrderState, SignedTx, TokenProgram, TokenState, TokenUtxo, TxJson } from '../kob/types';
import type { KobWasm } from '../kob/wasm';
import { isCovenantOwned } from '../kob/token-state';
import { MAKER, goldenRequest, signAndValidate } from './chain-fixtures';

/** The family mixes of the golden pair vectors (A family - B family): `pair.<mix>create.ask` etc. */
export const PAIR_MIXES = ['', 'kcc20-kron.', 'kron-kcc20.', 'kron-kron.'] as const;
/** The create vectors per kind: [vector suffix, kind, side]. */
export const PAIR_CREATES = [
  ['create.ask', 'KobPair', 'sell'], ['create.bid', 'KobPair', 'buy'], ['create.condAsk', 'KobCondPair', 'sell'], ['create.condBid', 'KobCondPair', 'buy'],
  ['create.ifdBid', 'KobIfdPair', 'buy'], ['create.ifdAsk', 'KobIfdPair', 'sell'],
] as const;

/** Token output states of a built tx by output index (the leader plan of each KCC-20 token lists them in output order; KRON: the first input of each token). */
export function tokenOutputStates(built: BuiltTx): Map<number, { covenantId: Hex; program: TokenProgram; state: TokenState }> {
  const out = new Map<number, { covenantId: Hex; program: TokenProgram; state: TokenState }>();
  const seen = new Set<Hex>();
  built.plans.forEach((p, i) => {
    if (p.kind !== 'tokenLeader' && p.kind !== 'kronToken') return;
    const cov = built.tx.inputs[i]!.utxo.covenantId;
    if (!cov || seen.has(cov)) return;
    seen.add(cov);
    const outs = built.tx.outputs.map((o, j) => [o, j] as const).filter(([o]) => o.covenant?.covenantId === cov);
    outs.forEach(([, j], k) => {
      const st = p.nextStates[k];
      if (st) out.set(j, { covenantId: cov, program: p.template, state: st });
    });
  });
  return out;
}

/** Builds a golden request re-keyed to the test maker (every maker, funding key and owned token of the vector), signs it with the maker key and validates it. */
export function buildGolden(kob: KobWasm, name: string): { built: BuiltTx; signed: SignedTx } {
  const built = kob.build(goldenRequest(name, MAKER.pk));
  return { built, signed: signAndValidate(kob, built, [MAKER.sk]) };
}

/**
 * The live snapshot of the order of covenant id `cov` after `tx`: its output whose script is one of the `candidates` states (the first that
 * matches), and its custodies (the token outputs owned by `cov`, matched to kob-protocol `custodies` of that state by token and amount). Null when
 * no output matches.
 */
export function snapshotAfter(kob: KobWasm, built: BuiltTx, tx: TxJson, cov: Hex, candidates: OrderState[], daa = 1000n): OrderSnapshot | null {
  const outs = tx.outputs.map((o, j) => ({ o, j })).filter((x) => x.o.covenant?.covenantId === cov);
  for (const st of candidates) {
    let spk: string;
    try {
      spk = kob.scriptPublicKey(st);
    } catch {
      continue;
    }
    const hit = outs.find((x) => x.o.scriptPublicKey === spk);
    if (!hit) continue;
    const tokens = tokenOutputStates(built);
    const owned = [...tokens].filter(([, t]) => isCovenantOwned(t.state) && t.state.owner === cov);
    const taken = new Set<number>();
    const utxoOf = (token: Hex, amount: bigint): TokenUtxo | null => {
      const m = owned.find(([j, t]) => !taken.has(j) && t.covenantId === token && BigInt(t.state.amount) === amount);
      if (!m) return null;
      taken.add(m[0]);
      return { transactionId: tx.id, index: m[0], amount: tx.outputs[m[0]]!.value, blockDaaScore: daa.toString(), covenantId: token, state: m[1].state };
    };
    const cs = custodiesOf(st, kob);
    const custody = cs[0] && cs[0].amount > 0n ? utxoOf(cs[0].token, cs[0].amount) : null;
    const prefund = cs[1] && cs[1].amount > 0n ? utxoOf(cs[1].token, cs[1].amount) : null;
    return {
      covenantId: cov,
      order: { transactionId: tx.id, index: hit.j, amount: hit.o.value, blockDaaScore: daa.toString(), covenantId: cov, state: st },
      custody,
      ...(prefund ? { prefund } : {}),
      strays: [],
      refundDueDaa: null,
      deadline: null,
      source: 'chain',
    };
  }
  return null;
}

/** The order state of a request leg (`legs[i].order.state`), as kob-wasm tags it. */
export function legState(request: unknown, i: number, kind: string): OrderState {
  const legs = (request as { legs: { order: { state: object } }[] }).legs;
  return { kind, state: legs[i]!.order.state } as unknown as OrderState;
}

/** A state with some fields replaced (decimal strings). */
export function withFields(o: OrderState, fields: Record<string, string | bigint>): OrderState {
  return { kind: o.kind, state: { ...o.state, ...Object.fromEntries(Object.entries(fields).map(([k, v]) => [k, v.toString()])) } } as OrderState;
}
