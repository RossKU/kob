// Test helper: real kob-wasm requests taken from the golden vectors (crates/kob-protocol/vectors/golden.json), re-keyed to a test key so
// the resulting transaction can be signed locally / by the fake wallets and is then consensus-valid (the script engine runs it).
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import type { ActionRequest, Hex } from '../kob/types';

interface Golden { transactions: { name: string; request: ActionRequest }[] }

let cache: Golden | null = null;
const golden = (): Golden => (cache ??= JSON.parse(readFileSync(fileURLToPath(new URL('../../../crates/kob-protocol/vectors/golden.json', import.meta.url)), 'utf8')) as Golden);

/** The golden vector's maker key (x-only) of the order / funding used by the cancel.* and create.* requests. */
export const GOLDEN_MAKER: Hex = '1b84c5567b126440995d3ed5aaba0565d71e1834604819ff9c17f5e9d5dd078f';

/** The golden request `name` with every occurrence of the golden maker key replaced by `makerPubkey`. */
export function goldenRequest(name: string, makerPubkey: Hex): ActionRequest {
  const v = golden().transactions.find((t) => t.name === name);
  if (!v) throw new Error(`no golden vector ${name}`);
  return JSON.parse(JSON.stringify(v.request).split(GOLDEN_MAKER).join(makerPubkey)) as ActionRequest;
}
