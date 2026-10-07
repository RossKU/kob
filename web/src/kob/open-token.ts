// Open-list tokens: every token whose program is on KOB's strict template list is listed by the executor, whether or not the registry has an
// entry for it (`GET /v1/tokens`: `standing: unverified`, `ticker: ""`, no scale, no decimals). To trade one from the web app a TokenInfo is
// synthesised from what the chain and the indexer show, with the conservative conventions below. Nothing here vouches for the token: the
// caller shows the `unverified` badge, the short covenant id and a caution line next to every order, and the pre-sign screen re-derives the rest.
//
//   * program / family: the token's template hash must be one this build embeds (kob.templates()); else `program-unknown`;
//   * extension commitment (KCC-20): the indexer row's, else the one of the token's listed orders; none found: `no-extension` (KRON has none);
//   * scale: the order scale (a power of ten, the price denominator) shared by most open orders of the token, so the wallet's orders quote the
//     same whole token as the book; no orders: `no-scale`; decimals = its exponent (the chain carries no decimals: amounts are shown in that
//     whole token, prices per it);
//   * tick: 1 sompi.
import type { IndexerTokenView } from '../data/indexer-types';
import type { Hex, TokenProgram } from './types';
import type { KobWasm } from './wasm';
import { familyOfProgram } from './token-state';
import { TEMPLATE_ID_PROGRAM, toTokenMarket } from './token-market';
import { CAPABILITIES, longId, type Capability, type TokenInfo, type TokenRegistry } from './registry';

export type OpenTokenIssue = 'program-unknown' | 'no-scale' | 'no-extension' | 'scale-invalid';
export type OpenTokenResult = { ok: true; info: TokenInfo } | { ok: false; reason: OpenTokenIssue };

/** What an order tells about its scale: base units per whole token (its price denominator). */
export interface ScaleSample { scale?: number | null }

/** Exponent of a power of ten in 1..=10^9, or null. */
const exponentOf = (scale: number): number | null => {
  for (let d = 0; d <= 9; d++) if (scale === 10 ** d) return d;
  return null;
};

/** The scale shared by most of the orders (ties: the smaller scale); null when no order tells one. Only powers of ten up to 10^9 count. */
export function standardScaleOf(rows: readonly ScaleSample[]): number | null {
  const count = new Map<number, number>();
  for (const r of rows) {
    if (!r.scale || exponentOf(r.scale) === null) continue;
    count.set(r.scale, (count.get(r.scale) ?? 0) + 1);
  }
  let best: number | null = null;
  let bestN = 0;
  for (const [s, n] of count) if (n > bestN || (n === bestN && best !== null && s < best)) [best, bestN] = [s, n];
  return best;
}

const PROGRAM_TEMPLATE_ID: Readonly<Record<string, string>> = Object.fromEntries(Object.entries(TEMPLATE_ID_PROGRAM).map(([id, p]) => [p, id]));

/**
 * TokenInfo of an open-list token, or the reason it cannot be traded here. `orderScales` are the scales of the token's open orders (book rows);
 * `extFromOrders` the extension commitment read from one of its listed orders.
 */
export function synthesizeOpenToken(kob: KobWasm, view: IndexerTokenView, orderScales: readonly ScaleSample[], extFromOrders: Hex | null = null, reg: TokenRegistry | null = null): OpenTokenResult {
  const tpl = view.template_hash ? kob.templates().find((t) => t.hash === view.template_hash && t.tokenSlots) : undefined;
  if (!tpl || !tpl.tokenSlots) return { ok: false, reason: 'program-unknown' };
  const program = tpl.name as TokenProgram;
  const family = familyOfProgram(program);
  const templateId = PROGRAM_TEMPLATE_ID[program];
  if (!templateId) return { ok: false, reason: 'program-unknown' };
  const ext = family === 'kron' ? null : (view.extension_commitment ?? extFromOrders);
  if (family === 'kcc20' && !ext) return { ok: false, reason: 'no-extension' };
  const scale = standardScaleOf(orderScales);
  if (scale === null) return { ok: false, reason: 'no-scale' };
  const decimals = exponentOf(scale);
  if (decimals === null) return { ok: false, reason: 'scale-invalid' };

  const id = view.covenant_id;
  // its only name: 8 + 8 hex of the covenant id (a 4 + 4 fragment is 32 bits, which a token made to match it can copy)
  const label = longId(id);
  const json = {
    ticker: label, name: label, family, covenant_id: id, template_id: templateId, extension_commitment: ext, extension_class: 'other' as const,
    decimals, tick: 1, status: 'listed' as const, verified: false,
  };
  try {
    toTokenMarket(kob, json); // the same checks a registry token passes (keeper tips, embedded program)
  } catch {
    return { ok: false, reason: 'scale-invalid' };
  }
  // the shipped registry's capabilities of the program template always apply; the indexer's powers can only add to them
  const declared = reg?.templates.find((t) => t.id === templateId)?.capabilities ?? [];
  const capabilities = [...new Set([...declared, ...(view.powers ?? [])])].filter((p): p is Capability => (CAPABILITIES as readonly string[]).includes(p));
  return {
    ok: true,
    info: {
      ticker: label, name: label, family, covenantId: id, templateId, templateHash: tpl.hash, extensionCommitment: ext, extensionClass: 'other', decimals,
      tick: 1n, status: 'listed', verified: false, official: false, capabilities, display: null, openList: true, program,
      templateMatchesPinned: true, prefixLen: tpl.prefixLen, suffixLen: tpl.suffixLen, slots: { inputs: tpl.tokenSlots[0], outputs: tpl.tokenSlots[1] },
      tradable: true, untradableReason: null, json,
    },
  };
}
