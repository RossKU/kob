// Shared helpers of the wallet adapters: signature extraction from wallet-signed transactions,
// timeouts, error classification, and key and network normalisation. They are pure and use no DOM.
// They let the adapters talk to a provider object without knowing where it came from.
import type { BuiltTx, Hex, InputSignature } from '../kob/types';
import { WalletError } from './types';

/** How long a wallet popup may stay unanswered while signing, in ms. */
export const SIGN_TIMEOUT_MS = 180_000;
/** How long a wallet popup may stay unanswered while connecting, in ms. */
export const CONNECT_TIMEOUT_MS = 60_000;
/** Re-reading an already connected wallet is silent. It either answers at once or it is gone. */
export const REFRESH_TIMEOUT_MS = 15_000;

// Bytes.

const HEX = /^[0-9a-f]*$/i;

/** Decodes a hex string. Throws when it is not hex or has an odd length. */
export function hexToBytes(hex: string): Uint8Array {
  if (hex.length % 2 || !HEX.test(hex)) throw new Error('not a hex string');
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; i++) out[i] = parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  return out;
}

/** Encodes bytes as lowercase hex. */
export const bytesToHex = (b: Uint8Array): string => Array.from(b, (x) => x.toString(16).padStart(2, '0')).join('');

/**
 * The data pushes of a signature script, in order. Non-push opcodes are skipped, because a
 * wallet-assembled signature script may contain small-integer opcodes. A truncated push ends the
 * parse. It never throws, because a hostile or odd wallet response must not crash the app.
 */
export function parsePushes(script: Uint8Array): Uint8Array[] {
  const out: Uint8Array[] = [];
  let i = 0;
  while (i < script.length) {
    const op = script[i++]!;
    let len = -1;
    if (op === 0x00) {
      out.push(new Uint8Array(0));
      continue;
    }
    if (op >= 0x01 && op <= 0x4b) len = op;
    else if (op === 0x4c) {
      if (i + 1 > script.length) break;
      len = script[i]!;
      i += 1;
    } else if (op === 0x4d) {
      if (i + 2 > script.length) break;
      len = script[i]! | (script[i + 1]! << 8);
      i += 2;
    } else if (op === 0x4e) {
      if (i + 4 > script.length) break;
      len = (script[i]! | (script[i + 1]! << 8) | (script[i + 2]! << 16) | (script[i + 3]! << 24)) >>> 0;
      i += 4;
    } else continue; // OP_1NEGATE, OP_1..OP_16, anything else: not a data push
    if (len < 0 || i + len > script.length) break;
    out.push(script.subarray(i, i + len));
    i += len;
  }
  return out;
}

// Signature extraction.

function asTxObject(v: unknown, depth = 0): { inputs?: { signatureScript?: unknown }[] } | null {
  let x = v;
  if (typeof x === 'string') {
    try {
      x = JSON.parse(x);
    } catch {
      return null;
    }
  }
  if (typeof x !== 'object' || x === null) return null;
  const o = x as Record<string, unknown>;
  if (Array.isArray(o.inputs)) return o as { inputs: { signatureScript?: unknown }[] };
  // Wallets wrap the transaction differently.
  if (depth < 2) {
    for (const k of ['psktTransactionJson', 'transaction', 'tx', 'signedTx', 'signedTransaction', 'result']) {
      if (k in o) {
        const inner = asTxObject(o[k], depth + 1);
        if (inner) return inner;
      }
    }
  }
  return null;
}

/**
 * The Schnorr signature of input `inputIndex` in a wallet-signed transaction. The transaction is
 * safe JSON, as a string or an object, possibly wrapped in a response object. The wallet shapes
 * seen on TN10 (tools/wallet-gate/RESULTS.md) are:
 * - `push(sig65)`, a 66-byte script: KasWare and Kastle for P2SH inputs, and P2PK inputs of every
 *   wallet. It is the first 65-byte push.
 * - `<sig65><redeem>` or `<args..><sig65>...`: Kaspire's modes, and Kastle with the redeem script.
 *   It is again the first 65-byte push.
 * - The KCC-20 owner witness `0x00 || sig65`, a 66-byte push.
 * - A bare 64-byte signature.
 * Returns hex of 65 bytes (signature plus sighash byte) or 64 bytes. kob-wasm `finalize` verifies
 * it against the digest, so this only locates it. Throws `WalletError('no-signature')` when the
 * input carries none. Kastle without `scripts` returns an empty signature script.
 */
export function extractSignature(signedTx: unknown, inputIndex: number): Hex {
  const tx = asTxObject(signedTx);
  if (!tx?.inputs) throw new WalletError('no-signature', 'The wallet did not return a transaction.');
  const input = tx.inputs[inputIndex];
  const ss = input?.signatureScript;
  if (typeof ss !== 'string' || ss === '') throw new WalletError('no-signature', `The wallet returned no signature for input ${inputIndex}.`);
  let script: Uint8Array;
  try {
    script = hexToBytes(ss);
  } catch {
    throw new WalletError('no-signature', `The wallet returned an unreadable signature script for input ${inputIndex}.`);
  }
  const pushes = parsePushes(script);
  const p65 = pushes.find((p) => p.length === 65);
  if (p65) return bytesToHex(p65);
  const p66 = pushes.find((p) => p.length === 66 && p[0] === 0x00);
  if (p66) return bytesToHex(p66.subarray(1));
  const p64 = pushes.find((p) => p.length === 64);
  if (p64) return bytesToHex(p64);
  throw new WalletError('no-signature', `No signature found in the wallet's signature script for input ${inputIndex} (pushes: ${pushes.map((p) => p.length).join(',') || 'none'}).`);
}

/** One signature per `built.sign` request, in order. `label` names the wallet in messages. */
export function collectSignatures(built: BuiltTx, signedTx: unknown, label: string): InputSignature[] {
  return built.sign.map((req) => {
    let signature: Hex;
    try {
      signature = extractSignature(signedTx, req.inputIndex);
    } catch (e) {
      if (e instanceof WalletError && e.code === 'no-signature') throw new WalletError('no-signature', `${label}: ${e.message}`);
      throw e;
    }
    // SIGHASH_ALL only. Another type would sign different data than the confirmation screen showed.
    if (signature.length === 130 && signature.slice(128) !== '01') {
      throw new WalletError('other', `${label} signed input ${req.inputIndex} with an unexpected signature hash type (${signature.slice(128)}).`);
    }
    return { inputIndex: req.inputIndex, signature };
  });
}

// Errors and timeouts.

/** Readable text of whatever a wallet threw: `{code, message}` objects, strings or Errors. */
export function errText(e: unknown): string {
  if (e && typeof e === 'object') {
    const o = e as { code?: unknown; message?: unknown };
    const parts = [o.code !== undefined && o.code !== null ? `code ${String(o.code)}` : '', typeof o.message === 'string' ? o.message : String(e)];
    return parts.filter(Boolean).join(': ');
  }
  return String(e);
}

/**
 * Whether the user closed or declined the wallet popup (code 4001, as in EIP-1193, or the usual
 * wording).
 */
export function isUserRejection(e: unknown): boolean {
  if (e instanceof WalletError) return e.code === 'rejected';
  const code = e && typeof e === 'object' ? (e as { code?: unknown }).code : undefined;
  if (code === 4001 || code === 'ACTION_REJECTED' || code === 'USER_REJECTED') return true;
  return /\b(reject(ed|ion)?|den(y|ied)|declin(e|ed)|cancel(l?ed)?|refus(e|ed)|dismiss(ed)?|closed by (the )?user|user closed|closed the (window|popup)|user abort)\b/i.test(errText(e));
}

/**
 * The wallet refused to sign because its own node does not know an input, or sees it spent (KasWare 0.11.5 `verifyPsktInputsOnActiveNetwork`:
 * "Input #0 (...) is not an unspent output on this network.", "... could not be verified ...", "... already spent by a pending transaction ...",
 * "Cannot verify inputs: the active network node is unreachable.", "Input verification failed: the active network node did not respond.").
 */
export function isInputsUnknown(e: unknown): boolean {
  if (e instanceof WalletError) return e.code === 'inputs-unknown';
  return /not an unspent output|does not match an unspent output|could not be verified|already spent by a pending|cannot verify inputs|input verification failed/i.test(errText(e));
}

/** Maps anything a provider call threw to a `WalletError`. */
export function toWalletError(e: unknown, label: string): WalletError {
  if (e instanceof WalletError) return e;
  if (isUserRejection(e)) return new WalletError('rejected', `You rejected the request in ${label}.`);
  if (isInputsUnknown(e)) return new WalletError('inputs-unknown', `${label} cannot see a coin of this transaction on its own node yet: ${errText(e)}`);
  return new WalletError('other', `${label}: ${errText(e)}`);
}

/** Rejects with `WalletError('timeout')` when the wallet popup is not answered in time. */
export function withTimeout<T>(p: Promise<T>, ms: number, what: string): Promise<T> {
  let t: ReturnType<typeof setTimeout>;
  const limit = new Promise<never>((_, rej) => {
    t = setTimeout(() => rej(new WalletError('timeout', `${what}: no response after ${Math.round(ms / 1000)}s (popup not answered?)`)), ms);
  });
  return Promise.race([p, limit]).finally(() => clearTimeout(t));
}

// Keys.

/**
 * The x-only public key (64 hex chars) from what wallets return: 32-byte x-only, or 33-byte
 * compressed (Kastle), with an optional 0x prefix.
 */
export function normalizePubkey(p: unknown): Hex {
  if (typeof p !== 'string') throw new WalletError('other', 'The wallet returned no public key.');
  const h = p.trim().replace(/^0x/i, '').toLowerCase();
  if (!HEX.test(h)) throw new WalletError('other', 'The wallet returned an unreadable public key.');
  if (h.length === 64) return h;
  if (h.length === 66) return h.slice(2); // compressed (parity byte + x): the x-only key is the last 32 bytes
  throw new WalletError('other', `Unexpected public key format from the wallet (${h.length / 2} bytes).`);
}
