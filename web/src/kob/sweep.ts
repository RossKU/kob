// Sweep records (KOB1 optional record 0x83, `payload::verify_sweep` in kob-protocol).
//
// A sweep moves an order's strays in place. The record names an output and an input. The maker's
// `cancel` of the order spent at the input continues the covenant id at the output under the same
// script, so the state, price, amount and custody stay the same.
//
// kob-wasm builds sweeps (`sweepOrder`) but has no recovery for them. This module re-checks a
// record against the built transaction as `verify_sweep` does. The previous state comes from the
// order input's signing plan, whose script `decodeSigning` checks against the spent UTXO.
//
// - The input is spent by its maker's `cancel`, carries a covenant id and is the P2SH of that
//   state. No other input carries the id.
// - The output is the P2SH of the same state, bound to the same covenant id with the order input as
//   its authorising input. It is the only output bound to that id.
//
// Pure, no I/O. The pre-sign decoder and the record store use it.
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

/** A sweep record that does not hold, with the reason shown to the user. */
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

/** Verifies one sweep record against the transaction and the signing plans (`verify_sweep`). */
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

/** True when `verifySweep` rejected the record. */
export const isSweepFailure = (r: RecoveredSweep | SweepFailure): r is SweepFailure => 'reason' in r;

/** Verifies every sweep record of a built transaction and splits them into held and failed. */
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
