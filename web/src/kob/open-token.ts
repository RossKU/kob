// Open-list tokens: every token whose program is on KOB's strict template list is listed by the executor, whether or not the registry has an
// entry for it (`GET /v1/tokens`: `standing: unverified`, `ticker: ""`, no scale, no decimals). To trade one from the web app a TokenInfo is
// synthesised from what the chain and the indexer show, with the conservative conventions below. Nothing here vouches for the token: the
// caller shows the long covenant id and a note on base units next to every order, and the pre-sign screen re-derives the rest.
//
//   * program / family: the token's template hash must be one this build embeds (kob.templates()); else `program-unknown`;
//   * extension commitment (KCC-20): the indexer row's, else the one of the token's listed orders; none found: `no-extension` (KRON has none).
//     The pick and its source are kept on the TokenInfo (`extension`) so the pre-sign screen shows them, cross-checked against the commitments
//     known without the indexer (`extensionReferences`: a genesis-verified registry entry, the wallet's node-verified holdings; audit A1-2);
//   * scale: a token created under KCC-23 declares its decimals on chain (`opts.declared`, verified against its covenant id): its scale is
//     10^min(decimals, 9), and a market whose orders use another scale is a `scale-conflict` (`openScale: 'declared'`, with or without orders). A token
//     without a declaration (created before KCC-23): the scale its first order set, open or ended, as the indexer reports it (`market_scale`), else
//     (an older indexer) the one shared by most open orders of the token, so the wallet's orders quote the same whole token as the book; decimals =
//     its exponent (amounts are shown in that whole token, prices per it). No order ever: the decimals the user entered for the token's first
//     order (0..=9, scale 10^decimals; `openScale: 'entered'`), else `no-scale`: such a token's decimals are never assumed; once an order exists
//     its scale decides again, also after every order of the token has ended (a first order with other decimals then would put two scales on one
//     market: TN10 2026-10-15 O-3), and entered decimals that give another scale are refused (`scale-conflict`);
//   * program review: on mainnet only a `reviewed` program (the KCC-20 3/3 reference, its public-mint build, the KRON programs) is traded
//     here, whatever the indexer lists; prototypes (8x8, ...) are `program-unreviewed`;
//   * a KCC-20 token the indexer does not list at all (no order of it was ever placed) is known only from the wallet's own token UTXOs of it
//     that the node confirmed (`heldTokenView`): their addresses commit to the program and the state, so they show the program and the
//     extension commitment. Without such holdings there is nothing to check the covenant id against, and the token is not offered;
//   * tick: 1 sompi.
import type { IndexerTokenView } from '../data/indexer-types';
import type { Hex, TokenProgram, TokenState, TokenUtxo } from './types';
import { TOKEN_PROGRAMS } from './programs.generated';
import type { KobWasm } from './wasm';
import { extensionOfState, familyOfProgram, isKeyOwned } from './token-state';
import { TEMPLATE_ID_PROGRAM, toTokenMarket } from './token-market';
import { CAPABILITIES, longId, type Capability, type ExtensionCheck, type ExtensionReference, type TokenInfo, type TokenRegistry } from './registry';

export type OpenTokenIssue = 'program-unknown' | 'program-unreviewed' | 'no-scale' | 'no-extension' | 'scale-invalid' | 'scale-conflict' | 'book-unavailable';
export type OpenTokenResult = { ok: true; info: TokenInfo } | { ok: false; reason: OpenTokenIssue };

/** Most decimals a first order can name: the scale is `10^decimals`, at most 10^9 (`MAX_SCALE`, kob-protocol `state.rs`). */
export const MAX_OPEN_DECIMALS = 9;

/** Decimals entered for a first order: an integer 0..=9 (one digit), else null. */
export function parseOpenDecimals(text: string): number | null {
  const s = text.trim();
  return /^[0-9]$/.test(s) ? Number(s) : null;
}

/**
 * Whether this build trades a token of `program` without a registry entry on `network`: on mainnet only a program the profile table marks
 * `reviewed` (`programs.generated.ts`); on a test network every embedded program (the indexer's strict list decides there).
 */
export function programAllowed(program: string, network: string | null | undefined): boolean {
  const p = TOKEN_PROGRAMS.find((x) => x.name === program);
  if (!p) return false;
  return network !== 'mainnet' || p.review === 'reviewed';
}

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

/**
 * The commitments of a KCC-20 covenant id known without the indexer: the registry's entry of the id when its genesis was verified (one
 * commitment), and the distinct commitments of the wallet's own token UTXOs of the id that the node confirmed (`TokenTracker.tokenUtxosFor`).
 */
export function extensionReferences(covenantId: Hex, reg: TokenRegistry | null, holdings: readonly TokenUtxo[] = []): ExtensionReference[] {
  const out: ExtensionReference[] = [];
  const entry = reg?.tokens.find((t) => t.covenantId === covenantId);
  if (entry && entry.family === 'kcc20' && entry.json.genesis_verified === true && entry.extensionCommitment) out.push({ commitment: entry.extensionCommitment, source: 'registry-genesis' });
  const held = new Set<Hex>();
  for (const u of holdings) {
    const ext = u.covenantId === covenantId ? extensionOfState(u.state) : null;
    if (ext) held.add(ext.toLowerCase());
  }
  for (const c of [...held].sort()) out.push({ commitment: c, source: 'holdings' });
  return out;
}

/** The extension check of an open-list KCC-20 token's commitment: every reference that names another commitment is a mismatch. */
export function checkExtension(commitment: Hex, source: ExtensionCheck['source'], references: readonly ExtensionReference[]): ExtensionCheck {
  const c = commitment.toLowerCase();
  return { commitment: c, source, references: [...references], mismatch: references.filter((r) => r.commitment.toLowerCase() !== c) };
}

const PROGRAM_TEMPLATE_ID: Readonly<Record<string, string>> = Object.fromEntries(Object.entries(TEMPLATE_ID_PROGRAM).map(([id, p]) => [p, id]));

export interface OpenTokenOptions {
  /** decimals the user entered for the token's first order (0..=9): used only when no order tells the scale */
  decimals?: number | null;
  /** the app's network: on `mainnet` only reviewed programs are traded (`programAllowed`) */
  network?: string | null;
  /** the row was built from the wallet's node-verified holdings (`heldTokenView`): the extension commitment's source is `holdings` */
  fromHoldings?: boolean;
  /**
   * the ticker and name of this browser's issuance of the token (data/issued-decimals.ts): its label instead of the covenant id. A label only
   * (unverified, shown as such); the decimals still come from the market's scale
   */
  issued?: { ticker: string; name: string } | null;
  /**
   * the token's KCC-23 declaration, verified against its covenant id (data/token-declarations.ts): its decimals are the chain's, so the market's
   * scale is `10^min(decimals, 9)` without any entered decimals, and orders of another scale are a `scale-conflict` (never silently re-read);
   * its ticker and name label the token (declared by its creator, unreviewed)
   */
  declared?: { decimals: number | null; symbol: string | null; name: string | null } | null;
}

/**
 * The scale of an open-list token's market: the indexer's `market_scale` (its first order's, open or ended), else the scale most of its open orders
 * use (an indexer without `market_scale`, or a token whose first order it has not seen yet); null when neither tells one. A `market_scale` that is
 * not a power of ten up to 10^9 is `invalid`.
 */
export function marketScaleOf(view: Pick<IndexerTokenView, 'market_scale'>, orderScales: readonly ScaleSample[]): number | null | 'invalid' {
  const m = view.market_scale;
  if (m !== undefined && m !== null) return typeof m === 'number' && exponentOf(m) !== null ? m : 'invalid';
  return standardScaleOf(orderScales);
}

/**
 * TokenInfo of an open-list token, or the reason it cannot be traded here. `orderScales` are the scales of the token's open orders (book rows);
 * `extFromOrders` the extension commitment read from one of its listed orders; `references` the commitments known without the indexer
 * (`extensionReferences`), compared with the pick (`info.extension`); `opts.decimals` the decimals entered for a first order.
 */
export function synthesizeOpenToken(
  kob: KobWasm,
  view: IndexerTokenView,
  orderScales: readonly ScaleSample[],
  extFromOrders: Hex | null = null,
  reg: TokenRegistry | null = null,
  references: readonly ExtensionReference[] = [],
  opts: OpenTokenOptions = {},
): OpenTokenResult {
  const tpl = view.template_hash ? kob.templates().find((t) => t.hash === view.template_hash && t.tokenSlots) : undefined;
  if (!tpl || !tpl.tokenSlots) return { ok: false, reason: 'program-unknown' };
  const program = tpl.name as TokenProgram;
  const family = familyOfProgram(program);
  const templateId = PROGRAM_TEMPLATE_ID[program];
  if (!templateId) return { ok: false, reason: 'program-unknown' };
  if (!programAllowed(program, opts.network)) return { ok: false, reason: 'program-unreviewed' };
  const ext = family === 'kron' ? null : (view.extension_commitment ?? extFromOrders);
  if (family === 'kcc20' && !ext) return { ok: false, reason: 'no-extension' };
  const fromOrders = marketScaleOf(view, orderScales);
  if (fromOrders === 'invalid') return { ok: false, reason: 'scale-invalid' };
  const declared = opts.declared && opts.declared.decimals !== null && Number.isInteger(opts.declared.decimals) && opts.declared.decimals >= 0 && opts.declared.decimals <= 18 ? opts.declared.decimals : null;
  const d = opts.decimals;
  const typed = typeof d === 'number' && Number.isInteger(d) && d >= 0 && d <= MAX_OPEN_DECIMALS ? d : null;
  // decimals entered for a first order while the market has a scale: the same scale is fine, another is refused (never silently replaced: the
  // amount and the price typed for them would then mean other quantities)
  if (fromOrders !== null && typed !== null && 10 ** typed !== fromOrders) return { ok: false, reason: 'scale-conflict' };
  // the decimals the token declared on chain decide its scale; a market whose orders use another one disagrees with the chain
  const declaredScale = declared !== null ? 10 ** Math.min(declared, MAX_OPEN_DECIMALS) : null;
  if (declaredScale !== null && fromOrders !== null && fromOrders !== declaredScale) return { ok: false, reason: 'scale-conflict' };
  const entered = fromOrders === null && declaredScale === null ? typed : null;
  const scale = fromOrders ?? declaredScale ?? (entered !== null ? 10 ** entered : null);
  if (scale === null) return { ok: false, reason: 'no-scale' };
  const decimals = declared ?? exponentOf(scale);
  if (decimals === null) return { ok: false, reason: 'scale-invalid' };

  const id = view.covenant_id;
  // its only name: 8 + 8 hex of the covenant id (a 4 + 4 fragment is 32 bits, which a token made to match it can copy); on the issuing browser
  // the ticker it was issued with (a label the issuer chose, unverified)
  const label = longId(id);
  const shownTicker = opts.declared?.symbol || opts.issued?.ticker || label;
  const shownName = opts.declared?.name || opts.issued?.name || shownTicker;
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
  const templatePowers = reg?.templates.find((t) => t.id === templateId)?.capabilities ?? [];
  const capabilities = [...new Set([...templatePowers, ...(view.powers ?? [])])].filter((p): p is Capability => (CAPABILITIES as readonly string[]).includes(p));
  return {
    ok: true,
    info: {
      ticker: shownTicker, name: shownName, family, covenantId: id, templateId, templateHash: tpl.hash, extensionCommitment: ext, extensionClass: 'other', decimals,
      tick: 1n, status: 'listed', capabilities, display: null, openList: true, openScale: entered !== null ? 'entered' : declaredScale !== null ? 'declared' : 'orders', program,
      templateMatchesPinned: true, prefixLen: tpl.prefixLen, suffixLen: tpl.suffixLen, slots: { inputs: tpl.tokenSlots[0], outputs: tpl.tokenSlots[1] },
      tradable: true, untradableReason: null, json,
      ...(family === 'kcc20' && ext ? { extension: checkExtension(ext, opts.fromHoldings ? 'holdings' : view.extension_commitment ? 'indexer' : 'orders', references) } : {}),
    },
  };
}

export type HeldTokenIssue = 'not-held' | 'program-ambiguous' | 'program-unknown' | 'program-unreviewed' | 'not-kcc20' | 'extension-ambiguous';
export type HeldTokenResult = { ok: true; view: IndexerTokenView } | { ok: false; reason: HeldTokenIssue };

/**
 * The token row of a KCC-20 covenant id the indexer does not list, from the wallet's own token UTXOs of it that the node confirmed
 * (`TokenTracker.tokenUtxosFor`, called with `programs[0]`; `programs` are the programs of the wallet's stored candidates of the covenant
 * id). Each confirmed UTXO's address is the P2SH of the program and its state, at an outpoint the node lists under `covenantId`: the
 * covenant id holds tokens of that program. The candidates must name ONE program, one this build embeds and allows on `network`
 * (`programAllowed`), of the KCC-20 family, and the UTXOs must carry ONE extension commitment. No decimals, no scale, no orders: the
 * first order names its decimals (`synthesizeOpenToken`, `opts.decimals`).
 */
/** What [`findHeldToken`] reads of the wallet's token tracker (`data/token-tracker.ts` `TokenTracker`). */
export interface HeldTokenSource {
  /** the candidates this browser stores (an issuance made here, transfers built here, an import) */
  list(pubkey: Hex): readonly { tokenCovId: Hex; program: TokenProgram }[];
  /** every token UTXO of the key the node confirms, of any token (the indexer's holdings and the stored candidates), with its program */
  heldTokens(pubkey: Hex): Promise<readonly { covenantId: Hex; program: TokenProgram; state: TokenState }[]>;
  /** the key-owned UTXOs of one token, confirmed on the node */
  tokenUtxosFor(pubkey: Hex, token: { covenantId: Hex; program: TokenProgram }): Promise<TokenUtxo[]>;
}

/**
 * [`heldTokenView`] of `covenantId` for the wallet `pubkey`. The program candidates are the stored ones and the programs of the wallet's
 * node-confirmed holdings of the id (a token bought or received elsewhere, which this browser never stored: its market opened only after a
 * manual import). The UTXOs are then read and confirmed on the node under that one program (`tokenUtxosFor`), as before.
 */
export async function findHeldToken(
  tracker: HeldTokenSource, kob: KobWasm, pubkey: Hex, covenantId: Hex, network: string | null | undefined,
): Promise<HeldTokenResult> {
  const id = covenantId.toLowerCase();
  const stored = tracker.list(pubkey).filter((c) => c.tokenCovId.toLowerCase() === id).map((c) => c.program);
  // a failed holdings read leaves the stored candidates (the node check below still decides)
  const holdings = await tracker.heldTokens(pubkey).catch(() => []);
  // key-held under the holding's own program (KRON: its holder owner, id_type 0 for KForge)
  const held = holdings.filter((u) => u.covenantId.toLowerCase() === id && isKeyOwned(u.state, u.program)).map((u) => u.program);
  // one program per covenant id: candidates naming two programs are refused by heldTokenView, without asking the node
  const programs = [...new Set([...stored, ...held])];
  const utxos = programs.length === 1 ? await tracker.tokenUtxosFor(pubkey, { covenantId: id, program: programs[0]! }) : [];
  return heldTokenView(kob, id, programs, utxos, network);
}

export function heldTokenView(
  kob: KobWasm, covenantId: Hex, programs: readonly TokenProgram[], held: readonly TokenUtxo[], network: string | null | undefined,
): HeldTokenResult {
  const id = covenantId.toLowerCase();
  const mine = held.filter((u) => (u.covenantId ?? '').toLowerCase() === id);
  const distinct = [...new Set(programs)];
  if (distinct.length > 1) return { ok: false, reason: 'program-ambiguous' };
  if (mine.length === 0 || distinct.length === 0) return { ok: false, reason: 'not-held' };
  const program = distinct[0]!;
  const tpl = kob.templates().find((t) => t.name === program && t.tokenSlots);
  const templateId = PROGRAM_TEMPLATE_ID[program];
  if (!tpl || !templateId) return { ok: false, reason: 'program-unknown' };
  if (familyOfProgram(program) !== 'kcc20') return { ok: false, reason: 'not-kcc20' };
  if (!programAllowed(program, network)) return { ok: false, reason: 'program-unreviewed' };
  const exts = new Set(mine.map((u) => extensionOfState(u.state)?.toLowerCase() ?? ''));
  if (exts.size !== 1 || exts.has('')) return { ok: false, reason: 'extension-ambiguous' };
  return {
    ok: true,
    view: {
      ticker: '', covenant_id: id, template_hash: tpl.hash, extension_commitment: [...exts][0]!, scale: null, decimals: null, open_asks: 0, open_bids: 0,
      standing: 'unverified', powers: [], template_id: templateId, family: 'kcc20',
    },
  };
}
