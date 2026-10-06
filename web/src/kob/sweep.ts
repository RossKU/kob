// SWEEP records (KOB1 optional record 0x83, kob-protocol `payload::verify_sweep`): the maker's sweep of an order's strays IN PLACE.
//
// A sweep record names (output, input): the maker's `cancel` of the order spent at `input` continues its covenant id at `output` under the SAME
// script (the same state, so the same price, amount and custody). kob-wasm builds sweeps (`sweepOrder`) but exposes no recovery for them, so this
// module re-checks a record against the built transaction exactly as `verify_sweep` does, with the previous state taken from the order input's
// signing plan (whose script `decodeSigning` checks against the spent UTXO):
//   * the input is spent by its maker's `cancel`, carries a covenant id and is the P2SH of that state; no other input carries the id;
//   * the output is the P2SH of the SAME state, bound to the same covenant id with the order input as its authorising input, and the only
//     output bound to that id.
// Pure (no I/O); used by the pre-sign decoder and by the record store after a sweep was accepted.
import { isOrderKind } from './order-facts';
import type { Hex, OrderState, Payload, SigPlan, TxJson } from './types';
import type { KobWasm } from './wasm';

/** A verified sweep: the same order at a new output. */
export interface RecoveredSweep {
  output: number;
  input: number;
  covenantId: Hex;
  /** KAS on the continuation */
  value: bigint;
  /** the order's state (unchanged) */
  order: OrderState;
}

export interface SweepFailure { output: number; input: number; reason: string }

/** The sweep records of a payload (none when it does not decode). */
export function sweepRecordsOf(kob: KobWasm, payloadHex: string): { output: number; input: number }[] {
  if (!payloadHex) return [];
  let p: Payload | null;
  try {
    p = kob.decodePayload(payloadHex);
  } catch {
    return [];
  }
  return (p?.records ?? []).flatMap((r) => (r.type === 'sweep' ? [{ output: r.output, input: r.input }] : []));
}

/** Verifies one sweep record against the transaction and the signing plans of its inputs (`verify_sweep`). */
export function verifySweep(kob: KobWasm, tx: TxJson, plans: readonly SigPlan[], rec: { output: number; input: number }): RecoveredSweep | SweepFailure {
  const { output, input } = rec;
  const bad = (reason: string): SweepFailure => ({ output, input, reason: `sweep record for output ${output}: ${reason}` });
  const plan = plans[input];
  if (!plan || plan.kind !== 'entry' || plan.entry !== 'cancel') return bad("the order input is not spent by its maker's cancel");
  let prev: OrderState;
  let spk: string;
  try {
    const st = kob.decodeState(plan.template, plan.state);
    if (!isOrderKind(st.kind)) return bad('the input is not an order');
    prev = st as OrderState;
    spk = kob.scriptPublicKey(prev);
  } catch {
    return bad("the order input's state does not decode");
  }
  const inp = tx.inputs[input];
  if (!inp) return bad('no such input');
  const id = inp.utxo.covenantId ?? null;
  if (!id) return bad('the input carries no covenant id');
  if (inp.utxo.scriptPublicKey !== spk) return bad('the input is not the order the record continues');
  if (tx.inputs.filter((x) => (x.utxo.covenantId ?? null) === id).length !== 1) return bad("another input carries the order's covenant id");
  const out = tx.outputs[output];
  if (!out) return bad('no such output');
  if (out.scriptPublicKey !== spk) return bad('the output is not the same order (another script)');
  if (!out.covenant || out.covenant.authorizingInput !== input || out.covenant.covenantId !== id) {
    return bad("the output does not continue the order's covenant id from the order input");
  }
  if (tx.outputs.filter((x) => x.covenant?.covenantId === id).length !== 1) return bad("other outputs carry the order's covenant id (a swept order is one output)");
  return { output, input, covenantId: id, value: BigInt(out.value), order: prev };
}

export const isSweepFailure = (r: RecoveredSweep | SweepFailure): r is SweepFailure => 'reason' in r;

/** Every sweep record of a built transaction, verified: the sweeps that hold and the records that do not. */
export function recoverSweeps(kob: KobWasm, tx: TxJson, plans: readonly SigPlan[]): { sweeps: RecoveredSweep[]; failed: SweepFailure[] } {
  const sweeps: RecoveredSweep[] = [];
  const failed: SweepFailure[] = [];
  for (const rec of sweepRecordsOf(kob, tx.payload)) {
    const r = verifySweep(kob, tx, plans, rec);
    if (isSweepFailure(r)) failed.push(r);
    else sweeps.push(r);
  }
  return { sweeps, failed };
}
