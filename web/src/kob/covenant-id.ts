// Whether an output group is a genesis, from bound data alone (R9FB2-1): the authorising input's
// outpoint and the outputs bound to the group. SIGHASH_ALL does not cover an input's UTXO covenant
// id, so the app's word on it decides nothing. The id is the consensus one (rusty-kaspa
// `covenant_id`), through kob-wasm `genesisCovenantId`; tests pin it to real placements.
import type { Hex } from './types';
import type { KobWasm } from './wasm';

export interface GroupOutput { index: number; value: bigint | string | number; scriptPublicKey: string }

/** The genesis covenant id of `outputs` authorised by `outpoint`. */
export function genesisCovenantId(kob: Pick<KobWasm, 'genesisCovenantId'>, outpoint: { transactionId: string; index: number }, outputs: readonly GroupOutput[]): Hex {
  return kob.genesisCovenantId(outpoint.transactionId, outpoint.index, outputs.map((o) => ({ index: o.index, value: String(o.value), scriptPublicKey: o.scriptPublicKey }))).toLowerCase();
}

/**
 * True when the outputs bound to `covenantId` and authorised by input `i` of `tx` form a valid
 * GENESIS of that id: on chain they start a new covenant, whatever the app says input `i` carries.
 */
export function isGenesisBinding(kob: Pick<KobWasm, 'genesisCovenantId'>, tx: { inputs: readonly { transactionId: string; index: number }[]; outputs: readonly { value: unknown; scriptPublicKey: string; covenant?: { authorizingInput: number; covenantId: string } | null }[] }, i: number, covenantId: string): boolean {
  const inp = tx.inputs[i];
  if (!inp) return false;
  const group = tx.outputs
    .map((o, index) => ({ o, index }))
    .filter(({ o }) => o.covenant?.authorizingInput === i && o.covenant.covenantId.toLowerCase() === covenantId.toLowerCase())
    .map(({ o, index }) => ({ index, value: String(o.value), scriptPublicKey: o.scriptPublicKey }));
  if (!group.length) return false;
  try {
    return genesisCovenantId(kob, inp, group) === covenantId.toLowerCase();
  } catch {
    return false;
  }
}
