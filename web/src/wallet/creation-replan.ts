// A saved creation (data/creations.ts) planned again before "Finish the creation" asks the wallet. The record is untrusted input (anything that
// can write this origin's localStorage can plant one), so its stored genesis is never signed: the spec is rebuilt from the record's facts (the
// preparation's funding inputs and declaration, the token's holders, carrier and program, the fee rate), kob-wasm plans both transactions from it
// again, and the result must reproduce the record's preparation and genesis ids and token exactly. The caller then checks the plan as a fresh
// creation (ui/confirm/issuance-model.ts) and the preparation's output 0 on the node.
import type { CreationRecord } from '../data/creations';
import { issueRaw, type IssueResult, type IssueSpec } from '../kob/issue';
import { decodeDeclaration } from '../kob/kcc23';
import { lockOfScript } from '../kob/creator-lock';
import type { Hex } from '../kob/types';
import type { KobWasm } from '../kob/wasm';

export type Replan = { ok: true; plan: IssueResult } | { ok: false; reason: string };

const P2PK = /^000020([0-9a-f]{64})ac$/;
const PROGRAM = { KCC20Ref: '3x3', KCC20PublicMint: 'public-mint' } as const;

/** The issuance spec the record's creation was planned from, or why it cannot be read. */
export function specOfRecord(kob: KobWasm, rec: CreationRecord): IssueSpec | string {
  const t = rec.token;
  const program = PROGRAM[t.program as keyof typeof PROGRAM];
  if (!program) return 'unknown token program';
  let d;
  try {
    d = decodeDeclaration(kob, rec.prep.payload).token;
  } catch {
    return 'the first step is not a token declaration';
  }
  if (d.name === null || d.symbol === null || d.decimals === null) return 'the declaration has no name, ticker or decimals';
  const funding = [];
  for (const i of rec.prep.inputs) {
    const pk = P2PK.exec(i.utxo?.scriptPublicKey ?? '')?.[1];
    if (!pk) return 'a coin of the first step is not a key-owned coin';
    funding.push({ transactionId: i.transactionId, index: i.index, amount: i.utxo.amount, blockDaaScore: i.utxo.blockDaaScore, pubkey: pk });
  }
  // the change goes to the key of a change output, else to the first coin's owner (kob-wasm's default)
  const changeTo = P2PK.exec(rec.prep.outputs[1]?.scriptPublicKey ?? '')?.[1] ?? funding[0]?.pubkey;
  const feeRate = rec.genesisBuilt?.fee?.feeRate;
  const c = rec.curve;
  const opt = <K extends string>(k: K, v: string | null) => (v ? ({ [k]: v } as Record<K, string>) : {});
  // a creator lock: the holder its hash owns was planned as the funding key's (kob-wasm turns it into the lock's)
  let lock: { holder: number; until: string; key: Hex } | null = null;
  if (d.lock) {
    const l = lockOfScript(kob, d.lock);
    const holder = l ? t.outputs.findIndex((o) => o.ownerScheme === 3 && o.owner === l.authority) : -1;
    if (!l || holder < 0) return 'the declared creator lock holds no output of the token';
    lock = { holder, until: l.until, key: l.key };
  }
  return {
    name: d.name, ticker: d.symbol, decimals: d.decimals, supply: t.supply,
    holders: t.outputs.map((o, i) => ({ owner: c && i === c.custody ? '00'.repeat(32) : lock && i === lock.holder ? lock.key : o.owner, ownerScheme: lock && i === lock.holder ? 0 : o.ownerScheme, amount: o.amount, ...(o.borrowScheme !== 0 ? { borrowScheme: o.borrowScheme, borrowGuard: o.borrowGuard } : {}) })),
    extensionCommitment: t.extensionCommitment, carrier: t.carrier, ...(feeRate ? { feeRate } : {}), funding, ...(changeTo ? { changeTo } : {}),
    ...(t.outputs.some((o) => o.borrowScheme !== 0) ? { allowBorrow: true } : {}),
    ...opt('description', d.description), ...opt('icon', d.image), ...(d.image ? opt('imageHash', d.imageHash) : {}), ...opt('website', d.website),
    ...opt('x', d.x), ...opt('telegram', d.telegram), network: rec.network, program,
    // a curve placed in the genesis: its terms from its recorded state (kob-wasm writes its custody's owner)
    ...(c ? { curve: { custody: c.custody, levelSize: c.state.state.levelSize, table: c.state.state.table, minFill: c.state.state.minFill, tip: c.state.state.tip, activeFrom: c.state.state.activeFrom, carrier: c.carrier } } : {}),
    ...(lock ? { lock: { holder: lock.holder, until: lock.until } } : {}),
  };
}

const sameJson = (a: unknown, b: unknown): boolean => JSON.stringify(a) === JSON.stringify(b);
const tokenFacts = (t: IssueResult['token']) => ({
  covenantId: t.covenantId, program: t.program, templateHash: t.templateHash, extensionCommitment: t.extensionCommitment, name: t.name, ticker: t.ticker,
  decimals: t.decimals, supply: String(t.supply), carrier: String(t.carrier),
  outputs: t.outputs.map((o) => ({ index: o.index, amount: String(o.amount), owner: o.owner, ownerScheme: o.ownerScheme, borrowScheme: o.borrowScheme, borrowGuard: o.borrowGuard })),
});

/**
 * Plans the record's creation again with kob-wasm. `ok` only when the new plan has the record's preparation id, genesis id, covenant id and
 * token, every funding coin and holder is checked by kob-wasm, and the wallet `maker` owns the record. Never throws.
 */
export function replanCreation(kob: KobWasm, rec: CreationRecord, maker: Hex): Replan {
  try {
    if (rec.maker !== maker.toLowerCase()) return { ok: false, reason: 'the record is of another wallet' };
    const spec = specOfRecord(kob, rec);
    if (typeof spec === 'string') return { ok: false, reason: spec };
    let plan = issueRaw(kob, spec);
    // a v1 record keeps no plan, so no fee rate: the signed genesis's fee says it (the fee is the rate times a mass the rate does not change)
    if (plan.built.tx.id !== rec.genesisTxid && !spec.feeRate && rec.genesis) {
      const paid = BigInt(rec.prep.outputs[0]?.value ?? 0) - rec.genesis.outputs.reduce((a, o) => a + BigInt(o.value), 0n);
      const rate = (paid * BigInt(plan.built.fee.feeRate)) / BigInt(plan.built.fee.fee);
      if (rate > 0n) plan = issueRaw(kob, { ...spec, feeRate: rate.toString() });
    }
    if (plan.prep.tx.id !== rec.prepTxid || rec.prep.id !== rec.prepTxid) return { ok: false, reason: 'the first step is not the one the record names' };
    if (plan.built.tx.id !== rec.genesisTxid) return { ok: false, reason: 'the saved token does not match its first step' };
    if (plan.token.covenantId !== rec.covenantId || !sameJson(tokenFacts(plan.token), tokenFacts(rec.token))) return { ok: false, reason: 'the saved token does not match its first step' };
    // a stored plan or signed genesis must be this genesis too (shown and tracked from the record)
    if (rec.genesisBuilt && !sameJson(rec.genesisBuilt.tx.outputs, plan.built.tx.outputs)) return { ok: false, reason: 'the saved genesis differs from the plan' };
    if (rec.genesis && !(sameJson(rec.genesis.outputs, plan.built.tx.outputs) && rec.genesis.payload === plan.built.tx.payload && rec.genesis.inputs.length === 1 && rec.genesis.inputs[0]!.transactionId === rec.prepTxid && rec.genesis.inputs[0]!.index === 0)) {
      return { ok: false, reason: 'the saved genesis differs from the plan' };
    }
    return { ok: true, plan };
  } catch (e) {
    return { ok: false, reason: e instanceof Error ? e.message : String(e) };
  }
}
