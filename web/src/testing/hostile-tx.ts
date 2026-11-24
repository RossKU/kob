// Hostile transactions for the decoder's tests, made from an honest build at the transaction level.
// The shared builder refuses to build them (a sweep never takes the custody, a cancel always spends
// it), so the tests must not depend on it: they take what the builder makes for an honest action and
// change the inputs, plans and outputs the decoder reads, keeping the values balanced and the fee
// as built.
import type { OrderSnapshot } from '../kob/cancel';
import type { BuiltTx, Kcc20State, TokenProgram, TokenUtxo } from '../kob/types';
import type { KobWasm } from '../kob/wasm';

const clone = <T>(v: T): T => JSON.parse(JSON.stringify(v)) as T;

type TokenPlan = { kind: 'tokenLeader' | 'tokenDelegator'; template: TokenProgram; state: Kcc20State; nextStates?: Kcc20State[] };
const tokenPlan = (b: BuiltTx, i: number): TokenPlan | null => {
  const p = b.plans[i] as unknown as TokenPlan;
  return p && (p.kind === 'tokenLeader' || p.kind === 'tokenDelegator') ? p : null;
};

/**
 * An honest sweep of an order's strays turned into one that also moves the order's custody: one
 * stray input of the order's own token is replaced by the custody (outpoint, carrier, script, plan
 * state), and the token output to the maker takes the custody's amount in its place (the leader's
 * next state and its script). The KAS the carriers differ by goes to the change.
 */
export function sweepTakingCustody(kob: KobWasm, honest: BuiltTx, snap: OrderSnapshot, stray: TokenUtxo): BuiltTx {
  const b = clone(honest);
  const custody = snap.custody;
  if (!custody) throw new Error('the order has no custody');
  const k = b.tx.inputs.findIndex((x) => x.transactionId === stray.transactionId && x.index === stray.index);
  const plan = k >= 0 ? tokenPlan(b, k) : null;
  if (!plan) throw new Error('the stray is not a token input of the build');
  const delta = BigInt(custody.state.amount) - BigInt(plan.state.amount);
  const carrier = BigInt(custody.amount) - BigInt(b.tx.inputs[k]!.utxo.amount);
  const input = b.tx.inputs[k]!;
  input.transactionId = custody.transactionId;
  input.index = custody.index;
  input.utxo = { ...input.utxo, amount: custody.amount, scriptPublicKey: kob.tokenScriptPublicKey(plan.template, custody.state as Kcc20State), covenantId: custody.covenantId ?? input.utxo.covenantId };
  plan.state = clone(custody.state as Kcc20State);
  // the token output to the maker carries the custody's amount in place of the stray's
  const leader = b.plans.map((_, i) => tokenPlan(b, i)).find((p) => p?.kind === 'tokenLeader' && p.nextStates?.length);
  const toMaker = leader?.nextStates?.find((s) => s.owner === snap.order.state.state.maker);
  if (!leader || !toMaker) throw new Error('no token output to the maker');
  const oldSpk = kob.tokenScriptPublicKey(leader.template, toMaker);
  toMaker.amount = (BigInt(toMaker.amount) + delta).toString();
  const out = b.tx.outputs.find((o) => o.scriptPublicKey === oldSpk);
  if (!out) throw new Error('the token output to the maker is not in the build');
  out.scriptPublicKey = kob.tokenScriptPublicKey(leader.template, toMaker);
  // the KAS the carriers differ by goes to the change, else (a sweep whose order carrier pays the
  // fee has none) to the token output to the maker, which returns the carriers
  const change = b.tx.outputs.find((o) => !o.covenant) ?? out;
  change.value = (BigInt(change.value) + carrier).toString();
  return b;
}

/**
 * The build without the token inputs `pick` selects (with their plans, roles and sign requests),
 * the token output to the wallet `maker` lowered by their amount (the leader's next state and its
 * script) and the KAS change (or that token output) by their carriers, so values balance and the fee
 * is as built. With `relabel`, the roles of the order inputs it names become that label.
 */
export function dropTokenInputs(kob: KobWasm, honest: BuiltTx, maker: string, pick: (state: Kcc20State, i: number) => boolean): BuiltTx {
  const b = clone(honest);
  const drop = b.tx.inputs.map((_, i) => i).filter((i) => {
    const p = tokenPlan(b, i);
    return !!p && pick(p.state, i);
  });
  if (!drop.length) throw new Error('nothing to drop');
  const tokens = drop.reduce((a, i) => a + BigInt(tokenPlan(b, i)!.state.amount), 0n);
  const carriers = drop.reduce((a, i) => a + BigInt(b.tx.inputs[i]!.utxo.amount), 0n);
  // the token output to the wallet, from the leader's next states (the leader may be dropped too)
  const leader = b.plans.map((_, i) => tokenPlan(b, i)).find((p) => p?.kind === 'tokenLeader' && p.nextStates?.some((x) => x.owner === maker)) ?? null;
  const toMaker = leader?.nextStates?.find((x) => x.owner === maker) ?? null;
  let out = null as BuiltTx['tx']['outputs'][number] | null;
  let freed = 0n;
  if (leader && toMaker) {
    out = b.tx.outputs.find((o) => o.scriptPublicKey === kob.tokenScriptPublicKey(leader.template, toMaker)) ?? null;
    const left = BigInt(toMaker.amount) - tokens;
    if (left > 0n) {
      toMaker.amount = left.toString();
      if (out) out.scriptPublicKey = kob.tokenScriptPublicKey(leader.template, toMaker);
    } else if (out) {
      leader.nextStates = leader.nextStates!.filter((x) => x !== toMaker);
      const gone = out;
      freed = BigInt(gone.value);
      b.tx.outputs = b.tx.outputs.filter((o) => o !== gone);
      out = null;
    }
  }
  const keep = <T,>(xs: T[]): T[] => xs.filter((_, i) => !drop.includes(i));
  const map = new Map<number, number>();
  let k = 0;
  b.tx.inputs.forEach((_, i) => { if (!drop.includes(i)) map.set(i, k++); });
  b.tx.inputs = keep(b.tx.inputs);
  b.plans = keep(b.plans);
  b.roles = keep(b.roles);
  b.sign = b.sign.filter((r) => map.has(r.inputIndex)).map((r) => ({ ...r, inputIndex: map.get(r.inputIndex)! }));
  for (const o of b.tx.outputs) if (o.covenant && map.has(o.covenant.authorizingInput)) o.covenant = { ...o.covenant, authorizingInput: map.get(o.covenant.authorizingInput)! };
  const change = b.tx.outputs.find((o) => !o.covenant) ?? out;
  if (change) change.value = (BigInt(change.value) - carriers + freed).toString();
  return b;
}

/** The token output to the wallet `maker` re-stated under another class (its script follows). */
export function restateMakerOutput(kob: KobWasm, honest: BuiltTx, maker: string, ext: string): BuiltTx {
  const b = clone(honest);
  const leader = b.plans.map((_, i) => tokenPlan(b, i)).find((p) => p?.kind === 'tokenLeader' && p.nextStates?.some((x) => x.owner === maker));
  const toMaker = leader?.nextStates?.find((x) => x.owner === maker);
  if (!leader || !toMaker) throw new Error('no token output to the maker');
  const out = b.tx.outputs.find((o) => o.scriptPublicKey === kob.tokenScriptPublicKey(leader.template, toMaker));
  if (!out) throw new Error('no such output');
  toMaker.extension_commitment = ext;
  out.scriptPublicKey = kob.tokenScriptPublicKey(leader.template, toMaker);
  return b;
}
