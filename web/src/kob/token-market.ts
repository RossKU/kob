// Registry token entry -> TokenMarket (the parameters every planner needs). The registry loader is another work item: this module only
// depends on the STRUCTURAL shape of `registry/tokens.json` (snake_case), so it works with whatever the loader returns.
import type { Family, Hex, TokenProgram } from './types';
import { familyOfProgram } from './token-state';
import type { TokenMarket } from './plan-types';
import type { KobWasm } from './wasm';

/** The fields of a `registry/tokens.json` token entry that market parameters derive from (extra fields are ignored). */
export interface RegistryTokenLike {
  ticker: string;
  covenant_id: Hex;
  template_id: string;
  extension_commitment: Hex | null;
  decimals: number;
  /** minimum price increment, sompi per whole token; null = none (1 sompi). The registry's `lot_size` (protocol v2.6) is ignored. */
  tick?: number | string | bigint | null;
  family?: string;
}

/** The fields of a `registry/tokens.json` template entry used to pin the token program (optional: built-in ids cover the shipped ones). */
export interface RegistryTemplateLike {
  id: string;
  template_hash: Hex;
  prefix_len?: number;
  suffix_len?: number;
  max_token_inputs?: number;
  max_token_outputs?: number;
  family?: string;
}

/** Registry template ids of the token programs kob-wasm embeds. */
export const TEMPLATE_ID_PROGRAM: Readonly<Record<string, TokenProgram>> = {
  'kcc20-ref-3x3': 'KCC20Ref',
  'kcc20-ref-4x5': 'KCC20Ref_4x5',
  'kcc20-ref-8x8': 'KCC20Ref_8x8',
  'kcc20-ref-16x16': 'KCC20Ref_16x16',
  'kcc20-p2': 'KCC20P2',
  'kcc20-kaspacom-0-2-5': 'KCC20KaspaCom_0_2_5',
  'kron-2433': 'KronToken2433',
  'kron-2732': 'KronToken2732',
};

export class TokenMarketError extends Error {
  constructor(message: string) {
    super(message);
    this.name = 'TokenMarketError';
  }
}

const big = (v: number | string | bigint, what: string): bigint => {
  try {
    return BigInt(v);
  } catch {
    throw new TokenMarketError(`${what} is not an integer: ${String(v)}`);
  }
};
const strip0x = (h: string): string => (h.startsWith('0x') ? h.slice(2) : h).toLowerCase();

/**
 * Builds the TokenMarket of a registry token for the embedded kob-wasm: program, template hash and lengths from `kob.templates()`,
 * default refund tip from `kob.keeperTips()`, the order scale from `kob.defaultScale(decimals)`. Throws TokenMarketError for tokens KOB cannot trade (
 * unknown program or a family the registry entry contradicts).
 *
 * `templates` (the registry's `templates` array) is optional: when given, the template hash of `token.template_id` must equal the
 * embedded program's pinned hash, so a registry / build mismatch fails loudly instead of building orders against the wrong program.
 */
export function toTokenMarket(kob: KobWasm, token: RegistryTokenLike, templates?: readonly RegistryTemplateLike[]): TokenMarket {
  const tk = token.ticker;
  const tick = token.tick == null ? 1n : big(token.tick, `${tk} tick`);
  if (tick <= 0n) throw new TokenMarketError(`${tk}: the tick must be positive`);
  if (!Number.isInteger(token.decimals) || token.decimals < 0 || token.decimals > 18) throw new TokenMarketError(`${tk}: decimals out of range`);

  const program = TEMPLATE_ID_PROGRAM[token.template_id];
  if (!program) throw new TokenMarketError(`${tk}: unknown token program "${token.template_id}"`);
  const family: Family = familyOfProgram(program);
  if (token.family !== undefined && token.family !== family) throw new TokenMarketError(`${tk}: family ${token.family} does not match the program ${program}`);
  if (family === 'kcc20' && token.extension_commitment == null) throw new TokenMarketError(`${tk}: a KCC-20 token needs its extension commitment`);
  const info = kob.templates().find((t) => t.name === program);
  if (!info || !info.tokenSlots) throw new TokenMarketError(`${tk}: kob-wasm has no token program ${program}`);

  const regTpl = templates?.find((t) => t.id === token.template_id);
  if (regTpl) {
    if (strip0x(regTpl.template_hash) !== info.hash) throw new TokenMarketError(`${tk}: registry template hash differs from the embedded ${program}`);
    if (regTpl.suffix_len !== undefined && regTpl.suffix_len !== info.suffixLen) throw new TokenMarketError(`${tk}: registry suffix length differs from the embedded ${program}`);
    if (regTpl.max_token_inputs !== undefined && regTpl.max_token_inputs !== info.tokenSlots[0]) throw new TokenMarketError(`${tk}: registry input slots differ from the embedded ${program}`);
    if (regTpl.max_token_outputs !== undefined && regTpl.max_token_outputs !== info.tokenSlots[1]) throw new TokenMarketError(`${tk}: registry output slots differ from the embedded ${program}`);
  }

  const tips = kob.keeperTips()[program];
  if (!tips) throw new TokenMarketError(`${tk}: no keeper tips for ${program}`);
  return {
    covenantId: strip0x(token.covenant_id),
    ticker: tk,
    decimals: token.decimals,
    program,
    family,
    templateHash: info.hash,
    prefixLen: info.prefixLen,
    suffixLen: info.suffixLen,
    extensionCommitment: family === 'kron' ? '00'.repeat(32) : strip0x(token.extension_commitment!),
    slots: { inputs: info.tokenSlots[0], outputs: info.tokenSlots[1] },
    scale: kob.defaultScale(token.decimals),
    tick,
    refundTip: BigInt(tips.refundTip),
    keeperTip: BigInt(tips.keeperTip),
  };
}
