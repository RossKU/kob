// Open-list tokens. The executor lists every token whose program this build supports for trading
// (the strict template list), whether or not the registry has an entry for it
// (`GET /v1/tokens`: `ticker: ""`, no scale, no decimals). To trade one from the web app, a
// TokenInfo is synthesised from what the chain and the indexer show, with the conservative
// conventions below. Nothing here vouches for the token. The caller shows the long covenant id and
// a note on base units next to every order, and the pre-sign screen re-derives the rest.
//
// - Program and family: the token's template hash must be one this build embeds
//   (kob.templates()), else `program-unknown`. A token whose covenant id several order placements
//   pin to different programs is listed without one (`program_ambiguous`) and gets
//   `program-ambiguous`. It is proven only from a token output.
// - Extension commitment (KCC-20): the indexer row's, else the one of the token's listed orders,
//   else `no-extension` (KRON has none). The pick and its source are kept on the TokenInfo
//   (`extension`), cross-checked against the commitments known without the indexer
//   (`extensionReferences`: a genesis-verified registry entry and the wallet's node-verified
//   holdings).
// - Scale and decimals, in this order (docs/spec/token-metadata.md 4).
//   1. A token created under KCC-23 declares its decimals on chain (`opts.declared`, verified
//      against its covenant id). Its scale is 10^min(decimals, 9). A market whose orders use
//      another scale is a `scale-conflict` (`openScale: 'declared'`).
//   2. A token without a declaration whose program has default decimals (profile
//      `default_decimals`, a launchpad's convention such as KaspaRocket 3) takes them the same way
//      (`openScale: 'program'`, `decimalsConvention` names the source).
//   3. Any other token takes the scale its first order set, open or ended, as the indexer reports
//      it (`market_scale`). An older indexer gives the scale most open orders share. The wallet's
//      orders then quote the same whole token as the book, and decimals is its exponent.
//   4. With no order ever, the decimals the user entered for the first order apply (0..=9, scale
//      10^decimals, `openScale: 'entered'`). Without them the result is `no-scale`, because such a
//      token's decimals are never assumed.
//   Once an order exists its scale decides again, also after every order of the token has ended.
//   Entered decimals that give another scale are refused (`scale-conflict`), because two scales on
//   one market would be wrong.
// - Program review: on mainnet only a `reviewed` program (the KCC-20 3/3 reference, its
//   public-mint build, the KRON programs) is traded here, whatever the indexer lists. Prototypes
//   (8x8, ...) are `program-unreviewed`.
// - A KCC-20 token the indexer does not list at all (no order of it was ever placed) is known only
//   from the wallet's own token UTXOs of it that the node confirmed (`heldTokenView`). Their
//   addresses commit to the program and the state, so they show the program and the extension
//   commitment. Without such holdings the same is read from any holder's token outputs of the
//   covenant id. The indexer names them (`GET /v1/token-utxos?token=`) and the node confirms each
//   one (`findPublicToken`, `TokenTracker.publicTokenUtxos`), so no wallet is needed. A token with
//   neither has nothing to check the covenant id against, and its market is not offered.
// - Tick: 1 sompi.
import type { IndexerTokenView } from '../data/indexer-types';
import type { Hex, TokenProgram, TokenState, TokenUtxo } from './types';
import { TOKEN_PROGRAMS, programDefaultDecimals } from './programs.generated';
import type { KobWasm } from './wasm';
import { extensionOfState, familyOfProgram, isKeyOwned } from './token-state';
import { TEMPLATE_ID_PROGRAM, toTokenMarket } from './token-market';
import { CAPABILITIES, longId, type Capability, type ExtensionCheck, type ExtensionReference, type TokenInfo, type TokenRegistry } from './registry';

/** Why an open-list token cannot be synthesised or traded here. */
export type OpenTokenIssue = 'program-unknown' | 'program-ambiguous' | 'program-unreviewed' | 'no-scale' | 'no-extension' | 'scale-invalid' | 'scale-conflict' | 'book-unavailable';
/** The synthesised TokenInfo of an open-list token, or the reason it cannot be traded. */
export type OpenTokenResult = { ok: true; info: TokenInfo } | { ok: false; reason: OpenTokenIssue };

/**
 * Most decimals a first order can name.
 * The scale is `10^decimals`, at most 10^9 (`MAX_SCALE`, kob-protocol `state.rs`).
 */
export const MAX_OPEN_DECIMALS = 9;

/** Decimals entered for a first order: an integer 0..=9 (one digit), else null. */
export function parseOpenDecimals(text: string): number | null {
  const s = text.trim();
  return /^[0-9]$/.test(s) ? Number(s) : null;
}

/**
 * Whether this build trades a token of `program` without a registry entry on `network`.
 * The program must be one whose tokens exist on the network (the profile's `networks`, empty
 * meaning any). On mainnet it must also be marked `reviewed` in the profile table
 * (`programs.generated.ts`). On a test network every other embedded program is allowed, and the
 * indexer's strict list decides there.
 */
export function programAllowed(program: string, network: string | null | undefined): boolean {
  const p = TOKEN_PROGRAMS.find((x) => x.name === program);
  if (!p) return false;
  if (network && p.networks.length > 0 && !(p.networks as readonly string[]).includes(network)) return false;
  return network !== 'mainnet' || p.review === 'reviewed';
}

/** What an order tells about its scale: base units per whole token (its price denominator). */
export interface ScaleSample { scale?: number | null }

/** Exponent of a power of ten in 1..=10^9, or null. */
export const exponentOf = (scale: number): number | null => {
  for (let d = 0; d <= 9; d++) if (scale === 10 ** d) return d;
  return null;
};

/**
 * The scale shared by most of the orders (ties: the smaller scale), or null if no order tells one.
 * Only powers of ten up to 10^9 count.
 */
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
 * The commitments of a KCC-20 covenant id known without the indexer.
 * They are the registry's entry of the id when its genesis was verified (one commitment), and the
 * distinct commitments of the wallet's own token UTXOs of the id that the node confirmed
 * (`TokenTracker.tokenUtxosFor`).
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

/**
 * The extension check of an open-list KCC-20 token's commitment.
 * Every reference that names another commitment is a mismatch.
 */
export function checkExtension(commitment: Hex, source: ExtensionCheck['source'], references: readonly ExtensionReference[]): ExtensionCheck {
  const c = commitment.toLowerCase();
  return { commitment: c, source, references: [...references], mismatch: references.filter((r) => r.commitment.toLowerCase() !== c) };
}

const PROGRAM_TEMPLATE_ID: Readonly<Record<string, string>> = Object.fromEntries(Object.entries(TEMPLATE_ID_PROGRAM).map(([id, p]) => [p, id]));

/** Inputs of `synthesizeOpenToken` beyond the indexer row. */
export interface OpenTokenOptions {
  /**
   * Decimals the user entered for the token's first order (0..=9).
   * Used only when no order tells the scale.
   */
  decimals?: number | null;
  /** The app's network. On `mainnet` only reviewed programs are traded (`programAllowed`). */
  network?: string | null;
  /**
   * The row was built from the wallet's node-verified holdings (`heldTokenView`), so the extension
   * commitment's source is `holdings`.
   */
  fromHoldings?: boolean;
  /**
   * The row was built from token outputs the node confirmed whoever holds them
   * (`findPublicToken`), so the extension commitment's source is `chain`.
   */
  fromChain?: boolean;
  /**
   * The ticker and name of this browser's issuance of the token (data/issued-decimals.ts). They
   * label the token instead of the covenant id. The label is unverified and shown as such. The
   * decimals still come from the market's scale.
   */
  issued?: { ticker: string; name: string } | null;
  /**
   * The token's KCC-23 declaration, verified against its covenant id (data/token-declarations.ts).
   * Its decimals are the chain's, so the market's scale is `10^min(decimals, 9)` without any
   * entered decimals. Orders of another scale are a `scale-conflict` and are never silently
   * re-read. Its ticker and name label the token. Its creator declared them and nobody reviewed
   * them.
   */
  declared?: { decimals: number | null; symbol: string | null; name: string | null } | null;
}

/**
 * The scale of an open-list token's market, or null when nothing tells one.
 * It is the indexer's `market_scale` (its first order's, open or ended). Otherwise it is the scale
 * most of its open orders use, for an indexer without `market_scale` or a token whose first order
 * it has not seen yet. A `market_scale` that is not a power of ten up to 10^9 is `invalid`.
 */
export function marketScaleOf(view: Pick<IndexerTokenView, 'market_scale'>, orderScales: readonly ScaleSample[]): number | null | 'invalid' {
  const m = view.market_scale;
  if (m !== undefined && m !== null) return typeof m === 'number' && exponentOf(m) !== null ? m : 'invalid';
  return standardScaleOf(orderScales);
}

/**
 * TokenInfo of an open-list token, or the reason it cannot be traded here.
 * - `orderScales` are the scales of the token's open orders (book rows).
 * - `extFromOrders` is the extension commitment read from one of its listed orders.
 * - `references` are the commitments known without the indexer (`extensionReferences`), compared
 *   with the pick (`info.extension`).
 * - `opts.decimals` are the decimals entered for a first order.
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
  // Several programs are claimed for the covenant id and the indexer named none (executor
  // `program_ambiguous`). A pick here would let anyone's order choose it.
  if (view.program_ambiguous) return { ok: false, reason: 'program-ambiguous' };
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
  // Decimals entered for a first order while the market has a scale: the same scale is fine and
  // another is refused. They are never silently replaced, because the amount and the price typed
  // for them would then mean other quantities.
  if (fromOrders !== null && typed !== null && 10 ** typed !== fromOrders) return { ok: false, reason: 'scale-conflict' };
  // The decimals the token declared on chain decide its scale, else its program's default (a
  // launchpad's convention for the tokens of its program that declare none). A market whose orders
  // use another one disagrees with them.
  const convention = declared === null ? programDefaultDecimals(program) : null;
  const fixed = declared ?? convention?.decimals ?? null;
  const declaredScale = fixed !== null ? 10 ** Math.min(fixed, MAX_OPEN_DECIMALS) : null;
  if (declaredScale !== null && fromOrders !== null && fromOrders !== declaredScale) return { ok: false, reason: 'scale-conflict' };
  const entered = fromOrders === null && declaredScale === null ? typed : null;
  const scale = fromOrders ?? declaredScale ?? (entered !== null ? 10 ** entered : null);
  if (scale === null) return { ok: false, reason: 'no-scale' };
  const decimals = fixed ?? exponentOf(scale);
  if (decimals === null) return { ok: false, reason: 'scale-invalid' };

  const id = view.covenant_id;
  // The only name is 8 + 8 hex of the covenant id. A 4 + 4 fragment is 32 bits, which a token made
  // to match it can copy. The issuing browser uses the ticker the token was issued with, a label
  // the issuer chose and nobody verified.
  const label = longId(id);
  const shownTicker = opts.declared?.symbol || opts.issued?.ticker || label;
  const shownName = opts.declared?.name || opts.issued?.name || shownTicker;
  const json = {
    ticker: label, name: label, family, covenant_id: id, template_id: templateId, extension_commitment: ext, extension_class: 'other' as const,
    decimals, tick: 1, verified: false,
  };
  try {
    toTokenMarket(kob, json); // the same checks a registry token passes (keeper tips, embedded program)
  } catch {
    return { ok: false, reason: 'scale-invalid' };
  }
  // The shipped registry's capabilities of the program template always apply. The indexer's powers
  // can only add to them.
  const templatePowers = reg?.templates.find((t) => t.id === templateId)?.capabilities ?? [];
  const capabilities = [...new Set([...templatePowers, ...(view.powers ?? [])])].filter((p): p is Capability => (CAPABILITIES as readonly string[]).includes(p));
  return {
    ok: true,
    info: {
      ticker: shownTicker, name: shownName, family, covenantId: id, templateId, templateHash: tpl.hash, extensionCommitment: ext, extensionClass: 'other', decimals,
      tick: 1n, capabilities, display: null, openList: true, openScale: entered !== null ? 'entered' : declared !== null ? 'declared' : convention ? 'program' : 'orders', program,
      ...(convention ? { decimalsConvention: convention.source } : {}),
      templateMatchesPinned: true, prefixLen: tpl.prefixLen, suffixLen: tpl.suffixLen, slots: { inputs: tpl.tokenSlots[0], outputs: tpl.tokenSlots[1] },
      tradable: true, untradableReason: null, json,
      ...(family === 'kcc20' && ext ? { extension: checkExtension(ext, opts.fromHoldings ? 'holdings' : opts.fromChain ? 'chain' : view.extension_commitment ? 'indexer' : 'orders', references) } : {}),
    },
  };
}

/** Why a held token cannot be turned into a token row. */
export type HeldTokenIssue = 'not-held' | 'program-ambiguous' | 'program-unknown' | 'program-unreviewed' | 'not-kcc20' | 'extension-ambiguous';
/** The token row built from holdings, or the reason it cannot be. */
export type HeldTokenResult = { ok: true; view: IndexerTokenView } | { ok: false; reason: HeldTokenIssue };

/**
 * What `findHeldToken` reads of the wallet's token tracker (`TokenTracker` in
 * data/token-tracker.ts).
 */
export interface HeldTokenSource {
  /** The candidates this browser stores: an issuance made here, transfers built here, an import. */
  list(pubkey: Hex): readonly { tokenCovId: Hex; program: TokenProgram }[];
  /**
   * Every token UTXO of the key that the node confirms, of any token, with its program. It covers
   * the indexer's holdings and the stored candidates.
   */
  heldTokens(pubkey: Hex): Promise<readonly { covenantId: Hex; program: TokenProgram; state: TokenState }[]>;
  /** The key-owned UTXOs of one token, confirmed on the node. */
  tokenUtxosFor(pubkey: Hex, token: { covenantId: Hex; program: TokenProgram }): Promise<TokenUtxo[]>;
}

/**
 * [`heldTokenView`] of `covenantId` for the wallet `pubkey`.
 * The program candidates are the stored ones and the programs of the wallet's node-confirmed
 * holdings of the id. That covers a token bought or received elsewhere, which this browser never
 * stored. The UTXOs are then read and confirmed on the node under that one program
 * (`tokenUtxosFor`).
 */
export async function findHeldToken(
  tracker: HeldTokenSource, kob: KobWasm, pubkey: Hex, covenantId: Hex, network: string | null | undefined,
): Promise<HeldTokenResult> {
  const id = covenantId.toLowerCase();
  const stored = tracker.list(pubkey).filter((c) => c.tokenCovId.toLowerCase() === id).map((c) => c.program);
  // A failed holdings read leaves the stored candidates. The node check below still decides.
  const holdings = await tracker.heldTokens(pubkey).catch(() => []);
  // Key-held under the holding's own program (KRON: its holder owner, id_type 0 for KForge).
  const held = holdings.filter((u) => u.covenantId.toLowerCase() === id && isKeyOwned(u.state, u.program)).map((u) => u.program);
  // One program per covenant id. Candidates naming two programs are refused by heldTokenView
  // without asking the node.
  const programs = [...new Set([...stored, ...held])];
  const utxos = programs.length === 1 ? await tracker.tokenUtxosFor(pubkey, { covenantId: id, program: programs[0]! }) : [];
  return heldTokenView(kob, id, programs, utxos, network);
}

/**
 * The token row of a KCC-20 covenant id the indexer does not list, from the wallet's own token
 * UTXOs of it that the node confirmed (`TokenTracker.tokenUtxosFor`, called with `programs[0]`).
 * `programs` are the programs of the wallet's stored candidates of the covenant id. Each confirmed
 * UTXO's address is the P2SH of the program and its state, at an outpoint the node lists under
 * `covenantId`, so the covenant id holds tokens of that program.
 * The candidates must name one program, one this build embeds and allows on `network`
 * (`programAllowed`), of the KCC-20 family. The UTXOs must carry one extension commitment.
 * There are no decimals, no scale and no orders. The first order names its decimals
 * (`synthesizeOpenToken`, `opts.decimals`).
 */
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
      powers: [], template_id: templateId, family: 'kcc20',
    },
  };
}

/** Why a token cannot be found from public token outputs. */
export type PublicTokenIssue = Exclude<HeldTokenIssue, 'not-held'> | 'indexer-unavailable' | 'not-indexed' | 'not-confirmed' | 'node-unavailable';
/** The token row built from public token outputs, or the reason it cannot be. */
export type PublicTokenResult = { ok: true; view: IndexerTokenView } | { ok: false; reason: PublicTokenIssue };

/**
 * What `findPublicToken` reads: the node-confirmed token outputs of one token, any holder
 * (`TokenTracker.publicTokenUtxos` in data/token-tracker.ts).
 */
export interface PublicTokenSource {
  publicTokenUtxos(covenantId: Hex): Promise<{ indexer: 'ok' | 'unavailable'; claimed: number; nodeFailed: boolean; utxos: readonly (TokenUtxo & { program: TokenProgram })[] }>;
}

/**
 * The token row of a KCC-20 covenant id that no registry or indexer token list names.
 * It is built from token outputs of the id that the indexer lists (`GET /v1/token-utxos?token=`)
 * and the node confirmed, whoever holds them. Each confirmed output's address commits to the token
 * program and the whole state, at an outpoint the node lists under the covenant id. The result is
 * [`heldTokenView`] of them, with its checks (one pinned program that this network allows, one
 * extension commitment). Nothing here reads a name, a ticker or decimals. Those are labels and
 * rules elsewhere.
 * The reasons say which fact is missing: the indexer does not answer (`indexer-unavailable`),
 * knows no output of the token (`not-indexed`), names some the node does not list
 * (`not-confirmed`), or the node could not be asked (`node-unavailable`).
 */
export async function findPublicToken(src: PublicTokenSource, kob: KobWasm, covenantId: Hex, network: string | null | undefined): Promise<PublicTokenResult> {
  const id = covenantId.toLowerCase();
  const r = await src.publicTokenUtxos(id);
  if (r.indexer === 'unavailable') return { ok: false, reason: 'indexer-unavailable' };
  if (r.utxos.length === 0) return { ok: false, reason: r.nodeFailed ? 'node-unavailable' : r.claimed === 0 ? 'not-indexed' : 'not-confirmed' };
  const held = heldTokenView(kob, id, [...new Set(r.utxos.map((u) => u.program))], r.utxos, network);
  return held.ok ? held : { ok: false, reason: held.reason === 'not-held' ? 'not-confirmed' : held.reason };
}
