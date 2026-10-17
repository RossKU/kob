// Message protocol between the app (parent page) and the private-key signer (signer.html, loaded in an `<iframe sandbox="allow-scripts">`
// WITHOUT allow-same-origin: an opaque origin, so the app's scripts can read neither its DOM nor its memory).
//
//   signer -> app   ready | connected {pubkey} | signed {id, txid, signatures} | rejected {id, reason} | forgotten | height {px}
//   app -> signer   sign {id, built, tokens} | update {id, built, tokens} | clock {daa} | cancel {id} | forget
//
// Nothing secret ever crosses: the app -> signer direction must use target '*' (the frame's origin is opaque, "null"), so it carries only the
// unsigned transaction; the signer returns public keys and signatures only. Both sides drop every message that does not parse here, and
// check the sender: the signer accepts only `event.source === window.parent` from the app's own origin, the app only its signer frame
// (origin "null").
import type { BuiltTx, Hex, InputSignature } from '../kob/types';

export const SIGNER_PROTOCOL = 'kob-signer/1';

/** Display hint for a token of the transaction (from the app's registry: a label only, the amounts come from the transaction itself). */
export interface TokenHint {
  covenantId: Hex;
  ticker: string;
  decimals: number;
  /** the app shows this token's KAS market inverted (KAS/TOKEN): the frame words a plain order of it the same way (a label only, like the ticker) */
  inverted?: boolean;
}

export type RejectReason = 'user' | 'invalid' | 'no-key' | 'busy' | 'cancelled';

export type SignerToApp =
  | { p: typeof SIGNER_PROTOCOL; type: 'ready' }
  | { p: typeof SIGNER_PROTOCOL; type: 'connected'; pubkey: Hex }
  /** `txid`: the transaction signed (the request's, or the update of it the frame showed last) */
  | { p: typeof SIGNER_PROTOCOL; type: 'signed'; id: string; txid?: Hex; signatures: InputSignature[] }
  | { p: typeof SIGNER_PROTOCOL; type: 'rejected'; id: string; reason: RejectReason }
  | { p: typeof SIGNER_PROTOCOL; type: 'forgotten' }
  /** the height of the frame's content (CSS px): the app sizes the frame to it, up to the viewport */
  | { p: typeof SIGNER_PROTOCOL; type: 'height'; px: number };

export type AppToSigner =
  /** `retimeOf`: the request the user just confirmed, planned again with a later clock: signed without asking again when nothing they approved got worse (retime.ts) */
  | { p: typeof SIGNER_PROTOCOL; type: 'sign'; id: string; built: BuiltTx; tokens: TokenHint[]; retimeOf?: string }
  /** the same request planned again while the user reads it (a later clock, the market now): shown in its place (core.ts `receive`) */
  | { p: typeof SIGNER_PROTOCOL; type: 'update'; id: string; built: BuiltTx; tokens: TokenHint[] }
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
  isObj(v) && hasKeys(v, ['covenantId', 'ticker', 'decimals', 'inverted']) && HEX32.test(String(v.covenantId)) && typeof v.ticker === 'string' && TICKER.test(v.ticker) && isIndex(v.decimals, 19) &&
  (v.inverted === undefined || typeof v.inverted === 'boolean');

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
      if (!hasKeys(data, ['p', 'type', 'id', 'built', 'tokens', 'retimeOf']) || typeof data.id !== 'string' || !ID.test(data.id)) return null;
      if (data.retimeOf !== undefined && (typeof data.retimeOf !== 'string' || !ID.test(data.retimeOf))) return null;
      if (!Array.isArray(data.tokens) || data.tokens.length > 32 || !data.tokens.every(isTokenHint)) return null;
      if (!sized(data.built) || !isBuiltTx(data.built)) return null;
      return { p: SIGNER_PROTOCOL, type: 'sign', id: data.id, built: data.built, tokens: data.tokens, ...(data.retimeOf !== undefined ? { retimeOf: data.retimeOf as string } : {}) };
    case 'update':
      if (!hasKeys(data, ['p', 'type', 'id', 'built', 'tokens']) || typeof data.id !== 'string' || !ID.test(data.id)) return null;
      if (!Array.isArray(data.tokens) || data.tokens.length > 32 || !data.tokens.every(isTokenHint)) return null;
      if (!sized(data.built) || !isBuiltTx(data.built)) return null;
      return { p: SIGNER_PROTOCOL, type: 'update', id: data.id, built: data.built, tokens: data.tokens };
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
      if (!hasKeys(data, ['p', 'type', 'id', 'txid', 'signatures']) || typeof data.id !== 'string' || !ID.test(data.id)) return null;
      if (data.txid !== undefined && !HEX32.test(String(data.txid))) return null;
      if (!Array.isArray(data.signatures) || data.signatures.length === 0 || data.signatures.length > MAX_IO || !data.signatures.every(isSignature)) return null;
      const signatures = data.signatures.map((s) => ({ inputIndex: s.inputIndex, signature: s.signature }));
      return { p: SIGNER_PROTOCOL, type: 'signed', id: data.id, ...(data.txid !== undefined ? { txid: String(data.txid) } : {}), signatures };
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
