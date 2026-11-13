// Turns a registry token entry into a TokenMarket, the parameters every planner needs.
// The module depends only on the shape of `registry/tokens.json` (snake_case), so it works with
// whatever the registry loader returns.
import type { Family, Hex, TokenProgram } from './types';
import { PROGRAM_OF_REGISTRY_ID } from './programs.generated';
import { familyOfProgram } from './token-state';
import type { TokenMarket } from './plan-types';
import type { KobWasm } from './wasm';

/** The fields of a `registry/tokens.json` token entry that market parameters derive from. */
export interface RegistryTokenLike {
  ticker: string;
  covenant_id: Hex;
  template_id: string;
  extension_commitment: Hex | null;
  decimals: number;
  /** Minimum price increment in sompi per whole token. Null means 1 sompi. */
  tick?: number | string | bigint | null;
  family?: string;
  /** True when every genesis output of the token was checked. Absent or false means unverified. */
  genesis_verified?: boolean;
}

/** The fields of a `registry/tokens.json` template entry used to pin the token program. */
export interface RegistryTemplateLike {
  id: string;
  template_hash: Hex;
  prefix_len?: number;
  suffix_len?: number;
  max_token_inputs?: number;
  max_token_outputs?: number;
  family?: string;
}

/** Maps the registry template ids of the token programs kob-wasm embeds to their program names. */
export const TEMPLATE_ID_PROGRAM: Readonly<Record<string, TokenProgram>> = PROGRAM_OF_REGISTRY_ID;

/** Raised for a registry token that KOB cannot trade or that contradicts the embedded program. */
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
 * Builds the TokenMarket of a registry token for the embedded kob-wasm.
 *
 * Program, template hash and lengths come from `kob.templates()`, the keeper tips from
 * `kob.keeperTips()` and the order scale from `kob.defaultScale(decimals)`. Throws TokenMarketError
 * for an unknown program or a family the registry entry contradicts.
 *
 * `templates` is the registry's template array. When given, the entry of `token.template_id` must
 * match the embedded program, so a registry and build mismatch fails instead of building orders
 * against the wrong program.
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
    genesisVerified: token.genesis_verified === true,
  };
}
