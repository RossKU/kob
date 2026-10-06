// Test helper: real built transactions to hand to wallet adapters (kob-wasm builds them; the script engine validates them once signed).
import type { BuiltTx, InputSignature } from '../kob/types';
import { loadKobNode } from '../kob/wasm.node';
import { goldenRequest } from './golden';
import { MAKER_PK, sendTokensRequest } from './token-fixtures';

export const kob = loadKobNode();

/** KCC-20 send: inputs 0 (token leader) and 1 (delegator) are covenant inputs with the maker's owner witness, input 2 is P2PK funding. */
export const buildSend = (): BuiltTx => kob.build(sendTokensRequest());

/** Cancel of a sell order: input 0 is the KobAsk covenant input (`cancel(sig)`), input 1 the custody token (covenant-id witness, unsigned). */
export const buildCancel = (): BuiltTx => kob.build(goldenRequest('cancel.ask', MAKER_PK));

/** finalize (verifies each signature against the digest) + script-engine validation: throws unless the result is consensus-valid. */
export function assertConsensusValid(built: BuiltTx, sigs: InputSignature[]) {
  const signed = kob.finalize(built, sigs, { tightenBudgets: true });
  kob.validate(signed);
  return signed;
}
