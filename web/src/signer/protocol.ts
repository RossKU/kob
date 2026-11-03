// Message protocol between the app (parent page) and the private-key signer (signer.html, loaded in an `<iframe sandbox="allow-scripts">`
// WITHOUT allow-same-origin: an opaque origin, so the app's scripts can read neither its DOM nor its memory).
//
//   signer -> app   ready | connected {pubkey} | signed {id, txid, signatures, prepSignatures?} | rejected {id, reason} | forgotten | height {px}
//   app -> signer   sign {id, built, tokens, notes?, prep?} | update {id, built, tokens} | clock {daa} | cancel {id} | forget
//
// `notes`: the app's non-blocking hints about the order (an order below the minimum value, a price far through the market) as STRUCTURED codes
// with numbers only (`SignerNote`). The frame renders its own fixed sentences from them, under the app's label, never blocking and never in
// place of a row it computed itself; the app cannot put free text into the frame. The parser drops every entry it does not know or whose
// numbers are out of bounds, and the whole list when it is longer than MAX_NOTES. Sent with `sign` only: the notes are worked out once, when the
// confirmation opens, and an `update` of the request keeps them.
//
// `prep`: a token creation (KCC-23): the preparation `built` (the genesis) spends output 0 of, shown and signed with it under ONE approval
// (`prepSignatures` are its signatures); `genesis`: the states its token outputs are claimed to hold, which the frame checks against each
// output's script (a P2SH output does not show its state) before it names the token's supply and holders.
//
// Nothing secret ever crosses: the app -> signer direction must use target '*' (the frame's origin is opaque, "null"), so it carries only the
// unsigned transaction; the signer returns public keys and signatures only. Both sides drop every message that does not parse here, and
// check the sender: the signer accepts only `event.source === window.parent` from the app's own origin, the app only its signer frame
// (origin "null").
import type { BuiltTx, Hex, InputSignature } from '../kob/types';
import type { RegistryRow } from './table';

export const SIGNER_PROTOCOL = 'kob-signer/1';

/** Display hint for a token of the transaction (from the app's registry: a label only, the amounts come from the transaction itself). */
export interface TokenHint {
  covenantId: Hex;
  ticker: string;
  decimals: number;
  /** the app shows this token's KAS market inverted (KAS/TOKEN): the frame words a plain order of it the same way (a label only, like the ticker) */
  inverted?: boolean;
  /**
   * the registry does not list the token: its decimals and ticker come from its KCC-23 declaration on chain (`declared`), from this browser's
   * issuance of it (`issued`), the decimals are its token program's default and the ticker is empty (`program`: a launchpad's convention,
   * `convention` names it), or the decimals are the assumed 8 and the ticker is empty (`assumed`); the frame marks the token so
   * (app/token-display.ts)
   */
  source?: 'declared' | 'issued' | 'program' | 'assumed';
  /** with `source: 'program'`: the source of the decimals as the app names it (`KaspaRocket convention`; a label only) */
  convention?: string;
}

/** A creation's claim of its genesis token outputs: program, extension commitment and each output's state (checked by the frame, never trusted). */
export interface GenesisClaim {
  program: string;
  extensionCommitment: Hex;
  outputs: { index: number; amount: string; owner: Hex; ownerScheme: number; borrowScheme: number; borrowGuard: Hex }[];
}

/**
 * A hint of the app about the order, as a code and numbers (never text): the frame words it itself (notes.ts) and says it is the app's, not
 * verified by the signer. Sompi amounts are decimal strings (they can pass 2^53); `bps` are basis points.
 */
export type SignerNote =
  /** the order is worth `value` sompi, under the executor's minimum `min` (`source`: its own, or the assumed default): it will not be listed */
  | { kind: 'belowMin'; value: string; min: string; source: 'indexer' | 'default' }
  /** the price is `bps` basis points through the market (probably a typing error) */
  | { kind: 'farThrough'; bps: number };

/** the most notes of a request (more is not a request of the app) */
export const MAX_NOTES = 4;
/** the largest amount a note may carry, in sompi (above the whole KAS supply) */
const MAX_NOTE_SOMPI = 3_000_000_000_000_000_000n;
/** the largest percentage a note may carry, in basis points (1 000 000 %) */
const MAX_NOTE_BPS = 100_000_000;

export type RejectReason = 'user' | 'invalid' | 'no-key' | 'busy' | 'cancelled';

export type SignerToApp =
  | { p: typeof SIGNER_PROTOCOL; type: 'ready' }
  | { p: typeof SIGNER_PROTOCOL; type: 'connected'; pubkey: Hex }
  /** `txid`: the transaction signed (the request's, or the update of it the frame showed last); `prepSignatures`: of the request's `prep` */
  | { p: typeof SIGNER_PROTOCOL; type: 'signed'; id: string; txid?: Hex; signatures: InputSignature[]; prepSignatures?: InputSignature[] }
  | { p: typeof SIGNER_PROTOCOL; type: 'rejected'; id: string; reason: RejectReason }
  | { p: typeof SIGNER_PROTOCOL; type: 'forgotten' }
  /** the height of the frame's content (CSS px): the app sizes the frame to it, up to the viewport */
  | { p: typeof SIGNER_PROTOCOL; type: 'height'; px: number };

export type AppToSigner =
  /** `retimeOf`: the request the user just confirmed, planned again with a later clock: signed without asking again when nothing they approved got worse (retime.ts) */
  | { p: typeof SIGNER_PROTOCOL; type: 'sign'; id: string; built: BuiltTx; tokens: TokenHint[]; registry?: RegistryRow[]; notes?: SignerNote[]; retimeOf?: string; prep?: BuiltTx; genesis?: GenesisClaim }
  /** the same request planned again while the user reads it (a later clock, the market now): shown in its place (core.ts `receive`) */
  | { p: typeof SIGNER_PROTOCOL; type: 'update'; id: string; built: BuiltTx; tokens: TokenHint[]; registry?: RegistryRow[] }
  /** the node's DAA score now (display only: the times of the table are said from it; nothing is signed from it) */
  | { p: typeof SIGNER_PROTOCOL; type: 'clock'; daa: string }
  | { p: typeof SIGNER_PROTOCOL; type: 'cancel'; id: string }
  | { p: typeof SIGNER_PROTOCOL; type: 'forget' };

// ------------------------------------------------------------------------------------------------ validation

const HEX = /^(?:[0-9a-f]{2})*$/;
const HEX32 = /^[0-9a-f]{64}$/;
const ID = /^[A-Za-z0-9_-]{1,64}$/;
const TICKER = /^[\x20-\x7e]{1,24}$/;
const REASONS: readonly RejectReason[] = ['user', 'invalid', 'no-key', 'busy', 'cancelled'];
/** generous bounds: a KOB transaction has a handful of inputs and outputs; anything far larger is not a request of the app */
const MAX_IO = 512;
const MAX_JSON = 2_000_000;
/** the most tokens of a registry table */
const MAX_TABLE = 4096;

const isObj = (v: unknown): v is Record<string, unknown> => typeof v === 'object' && v !== null && !Array.isArray(v);
const isIndex = (v: unknown, below: number): v is number => typeof v === 'number' && Number.isInteger(v) && v >= 0 && v < below;
const isHex = (v: unknown, max = MAX_JSON): v is string => typeof v === 'string' && v.length <= max && HEX.test(v);
const isUint = (v: unknown): boolean => (typeof v === 'string' && /^(0|[1-9][0-9]{0,19})$/.test(v)) || (typeof v === 'number' && Number.isSafeInteger(v) && v >= 0);
const hasKeys = (o: Record<string, unknown>, keys: string[]): boolean => Object.keys(o).every((k) => keys.includes(k));

/** A signature as kob-wasm `finalize` takes it: 64 bytes, or 65 with the SIGHASH_ALL byte. */
const isSignature = (v: unknown): v is InputSignature =>
  isObj(v) && hasKeys(v, ['inputIndex', 'signature']) && isIndex(v.inputIndex, MAX_IO) && typeof v.signature === 'string' && /^[0-9a-f]{128}(01)?$/.test(v.signature);

/** Structural check of a built transaction: the shapes the signer reads (it decodes and re-derives everything else itself). */
export function isBuiltTx(v: unknown): v is BuiltTx {
  if (!isObj(v) || !isObj(v.tx) || !Array.isArray(v.sign) || !Array.isArray(v.plans) || !Array.isArray(v.roles) || !isObj(v.fee) || !Array.isArray(v.covenants)) return false;
  const tx = v.tx;
  if (!HEX32.test(String(tx.id)) || !Array.isArray(tx.inputs) || !Array.isArray(tx.outputs) || !isHex(tx.payload)) return false;
  if (tx.inputs.length === 0 || tx.inputs.length > MAX_IO || tx.outputs.length > MAX_IO || v.plans.length !== tx.inputs.length) return false;
  for (const i of tx.inputs as unknown[]) {
    if (!isObj(i) || !HEX32.test(String(i.transactionId)) || !isIndex(i.index, 2 ** 32) || !isHex(i.signatureScript) || !isObj(i.utxo)) return false;
  }
  for (const o of tx.outputs as unknown[]) {
    if (!isObj(o) || !isUint(o.value) || !isHex(o.scriptPublicKey, 20_000)) return false;
  }
  const seen = new Set<number>();
  for (const s of v.sign as unknown[]) {
    if (!isObj(s) || !hasKeys(s, ['inputIndex', 'pubkey', 'sighashType', 'sighash', 'redeemScript'])) return false;
    if (!isIndex(s.inputIndex, tx.inputs.length) || seen.has(s.inputIndex)) return false;
    seen.add(s.inputIndex);
    if (!HEX32.test(String(s.pubkey)) || s.sighashType !== 1 || !HEX32.test(String(s.sighash))) return false;
    if (!(s.redeemScript === null || isHex(s.redeemScript, 200_000))) return false;
  }
  return seen.size > 0;
}

const isTokenHint = (v: unknown): v is TokenHint =>
  isObj(v) && hasKeys(v, ['covenantId', 'ticker', 'decimals', 'inverted', 'source', 'convention']) && HEX32.test(String(v.covenantId)) && typeof v.ticker === 'string' && isIndex(v.decimals, 19) &&
  (v.inverted === undefined || typeof v.inverted === 'boolean') &&
  (v.source === undefined || v.source === 'declared' || v.source === 'issued' || v.source === 'program' || v.source === 'assumed') &&
  // a program default's source: a short printable label, with that source only
  (v.source === 'program' ? typeof v.convention === 'string' && /^[\x20-\x7e]{1,40}$/.test(v.convention) : v.convention === undefined) &&
  // a registry or issued ticker; an assumed or program-default token has none (the frame names it by its covenant id)
  (v.source === 'assumed' || v.source === 'program' ? v.ticker === '' : TICKER.test(v.ticker));

const isClaim = (v: unknown): v is GenesisClaim =>
  isObj(v) && hasKeys(v, ['program', 'extensionCommitment', 'outputs']) && typeof v.program === 'string' && /^[A-Za-z0-9_]{1,64}$/.test(v.program) &&
  HEX32.test(String(v.extensionCommitment)) && Array.isArray(v.outputs) && v.outputs.length > 0 && v.outputs.length <= 64 &&
  v.outputs.every(
    (o) =>
      isObj(o) && hasKeys(o, ['index', 'amount', 'owner', 'ownerScheme', 'borrowScheme', 'borrowGuard']) && isIndex(o.index, MAX_IO) &&
      typeof o.amount === 'string' && /^(0|[1-9][0-9]{0,18})$/.test(o.amount) && HEX32.test(String(o.owner)) && isIndex(o.ownerScheme, 256) &&
      isIndex(o.borrowScheme, 256) && HEX32.test(String(o.borrowGuard)),
  );

/** The app's registry table (table.ts): rows of a covenant id, a printable ticker and decimals, no covenant id twice. */
const isTable = (v: unknown): v is RegistryRow[] =>
  Array.isArray(v) && v.length <= MAX_TABLE &&
  v.every((r) => isObj(r) && hasKeys(r, ['covenantId', 'ticker', 'decimals']) && HEX32.test(String(r.covenantId)) && typeof r.ticker === 'string' && TICKER.test(r.ticker) && isIndex(r.decimals, 19)) &&
  new Set(v.map((r: RegistryRow) => r.covenantId)).size === v.length;

const sompiNote = (v: unknown): string | null => {
  const s = typeof v === 'number' && Number.isSafeInteger(v) && v >= 0 ? String(v) : v;
  return typeof s === 'string' && /^(0|[1-9][0-9]{0,18})$/.test(s) && BigInt(s) <= MAX_NOTE_SOMPI ? s : null;
};

/**
 * The notes of a request as the frame may word them: known kinds with exactly their fields and numbers within bounds, nothing else. An entry
 * that is not one is dropped; a list that is not an array, or longer than MAX_NOTES, is dropped whole. Never throws.
 */
export function sanitizeNotes(v: unknown): SignerNote[] {
  if (!Array.isArray(v) || v.length > MAX_NOTES) return [];
  const out: SignerNote[] = [];
  for (const n of v as unknown[]) {
    if (!isObj(n)) continue;
    if (n.kind === 'belowMin' && hasKeys(n, ['kind', 'value', 'min', 'source']) && (n.source === 'indexer' || n.source === 'default')) {
      const value = sompiNote(n.value);
      const min = sompiNote(n.min);
      if (value !== null && min !== null) out.push({ kind: 'belowMin', value, min, source: n.source });
    } else if (n.kind === 'farThrough' && hasKeys(n, ['kind', 'bps']) && typeof n.bps === 'number' && Number.isSafeInteger(n.bps) && n.bps >= 0 && n.bps <= MAX_NOTE_BPS) {
      out.push({ kind: 'farThrough', bps: n.bps });
    }
  }
  return out;
}

const sized = (v: unknown): boolean => {
  try {
    return JSON.stringify(v).length <= MAX_JSON;
  } catch {
    return false;
  }
};

/** What the SIGNER accepts from the app; null for anything else (dropped silently). */
export function parseAppMessage(data: unknown): AppToSigner | null {
  if (!isObj(data) || data.p !== SIGNER_PROTOCOL || typeof data.type !== 'string') return null;
  switch (data.type) {
    case 'sign':
      if (!hasKeys(data, ['p', 'type', 'id', 'built', 'tokens', 'registry', 'notes', 'retimeOf', 'prep', 'genesis']) || typeof data.id !== 'string' || !ID.test(data.id)) return null;
      if (data.retimeOf !== undefined && (typeof data.retimeOf !== 'string' || !ID.test(data.retimeOf))) return null;
      if (!Array.isArray(data.tokens) || data.tokens.length > 32 || !data.tokens.every(isTokenHint)) return null;
      if (!sized(data.built) || !isBuiltTx(data.built)) return null;
      if (data.registry !== undefined && !isTable(data.registry)) return null;
      // a creation is never a re-timed request
      if (data.prep !== undefined && (data.retimeOf !== undefined || !sized(data.prep) || !isBuiltTx(data.prep))) return null;
      // a genesis claim only with its creation
      if (data.genesis !== undefined && (data.prep === undefined || !isClaim(data.genesis))) return null;
      const notes = sanitizeNotes(data.notes);
      return {
        p: SIGNER_PROTOCOL, type: 'sign', id: data.id, built: data.built, tokens: data.tokens,
        ...(data.registry !== undefined ? { registry: data.registry as RegistryRow[] } : {}),
        ...(notes.length > 0 ? { notes } : {}),
        ...(data.retimeOf !== undefined ? { retimeOf: data.retimeOf as string } : {}), ...(data.prep !== undefined ? { prep: data.prep as BuiltTx } : {}),
        ...(data.genesis !== undefined ? { genesis: data.genesis as GenesisClaim } : {}),
      };
    case 'update':
      if (!hasKeys(data, ['p', 'type', 'id', 'built', 'tokens', 'registry']) || typeof data.id !== 'string' || !ID.test(data.id)) return null;
      if (!Array.isArray(data.tokens) || data.tokens.length > 32 || !data.tokens.every(isTokenHint)) return null;
      if (!sized(data.built) || !isBuiltTx(data.built)) return null;
      if (data.registry !== undefined && !isTable(data.registry)) return null;
      return { p: SIGNER_PROTOCOL, type: 'update', id: data.id, built: data.built, tokens: data.tokens, ...(data.registry !== undefined ? { registry: data.registry as RegistryRow[] } : {}) };
    case 'clock':
      return hasKeys(data, ['p', 'type', 'daa']) && typeof data.daa === 'string' && /^[0-9]{1,20}$/.test(data.daa) ? { p: SIGNER_PROTOCOL, type: 'clock', daa: data.daa } : null;
    case 'cancel':
      return hasKeys(data, ['p', 'type', 'id']) && typeof data.id === 'string' && ID.test(data.id) ? { p: SIGNER_PROTOCOL, type: 'cancel', id: data.id } : null;
    case 'forget':
      return hasKeys(data, ['p', 'type']) ? { p: SIGNER_PROTOCOL, type: 'forget' } : null;
    default:
      return null;
  }
}

/** What the APP accepts from the signer; null for anything else. */
export function parseSignerMessage(data: unknown): SignerToApp | null {
  if (!isObj(data) || data.p !== SIGNER_PROTOCOL || typeof data.type !== 'string') return null;
  switch (data.type) {
    case 'ready':
    case 'forgotten':
      return hasKeys(data, ['p', 'type']) ? { p: SIGNER_PROTOCOL, type: data.type } : null;
    case 'connected':
      return hasKeys(data, ['p', 'type', 'pubkey']) && HEX32.test(String(data.pubkey)) ? { p: SIGNER_PROTOCOL, type: 'connected', pubkey: String(data.pubkey) } : null;
    case 'signed': {
      if (!hasKeys(data, ['p', 'type', 'id', 'txid', 'signatures', 'prepSignatures']) || typeof data.id !== 'string' || !ID.test(data.id)) return null;
      if (data.txid !== undefined && !HEX32.test(String(data.txid))) return null;
      const sigsOf = (v: unknown): InputSignature[] | null =>
        Array.isArray(v) && v.length > 0 && v.length <= MAX_IO && v.every(isSignature) ? v.map((s) => ({ inputIndex: s.inputIndex, signature: s.signature })) : null;
      const signatures = sigsOf(data.signatures);
      if (!signatures) return null;
      const prepSignatures = data.prepSignatures === undefined ? undefined : sigsOf(data.prepSignatures);
      if (prepSignatures === null) return null;
      return {
        p: SIGNER_PROTOCOL, type: 'signed', id: data.id, ...(data.txid !== undefined ? { txid: String(data.txid) } : {}), signatures,
        ...(prepSignatures ? { prepSignatures } : {}),
      };
    }
    case 'height':
      return hasKeys(data, ['p', 'type', 'px']) && isIndex(data.px, 100_000) ? { p: SIGNER_PROTOCOL, type: 'height', px: data.px } : null;
    case 'rejected':
      if (!hasKeys(data, ['p', 'type', 'id', 'reason']) || typeof data.id !== 'string' || !ID.test(data.id) || !REASONS.includes(data.reason as RejectReason)) return null;
      return { p: SIGNER_PROTOCOL, type: 'rejected', id: data.id, reason: data.reason as RejectReason };
    default:
      return null;
  }
}

/** The signer's sender check: only its parent, from the app's own origin (the origin of the signer page's URL). */
export function fromParent(e: { source: unknown; origin: string }, parent: unknown, appOrigin: string): boolean {
  return e.source === parent && e.source !== null && e.origin === appOrigin;
}

/** The app's sender check: only its signer frame, whose origin is opaque ("null": the frame is sandboxed without allow-same-origin). */
export function fromSigner(e: { source: unknown; origin: string }, frame: unknown): boolean {
  return e.source === frame && e.source !== null && e.origin === 'null';
}

type DistOmit<T, K extends PropertyKey> = T extends unknown ? Omit<T, K> : never;

/** A signer -> app message. */
export const toApp = (m: DistOmit<SignerToApp, 'p'>): SignerToApp => ({ p: SIGNER_PROTOCOL, ...m }) as SignerToApp;
/** An app -> signer message. */
export const toSigner = (m: DistOmit<AppToSigner, 'p'>): AppToSigner => ({ p: SIGNER_PROTOCOL, ...m }) as AppToSigner;
