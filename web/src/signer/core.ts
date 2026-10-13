// The private-key signer's logic, without DOM (signer/main.ts is the page around it; unit tests drive it directly).
//
// The key is parsed into the official SDK's `PrivateKey` (its bytes live in wasm memory) and held in this closure only: never in storage, never in
// a message, never in an error text. `forget()` frees it. Signing happens only through `approve(id)`, which the page calls from a trusted click on
// its own button: a request from the app alone never produces a signature.
import type { KaspaSdk, SdkPrivateKey } from '../data/kaspa-sdk';
import type { BuiltTx, Hex, InputSignature } from '../kob/types';
import type { KobWasm } from '../kob/wasm';
import { toApp, type AppToSigner, type RejectReason, type SignerToApp, type TokenHint } from './protocol';
import { summarize, type SignerSummary } from './summary';

export class SignerKeyError extends Error {
  constructor() {
    super('This is not a valid private key: 64 hex characters (32 bytes) are expected.');
    this.name = 'SignerKeyError';
  }
}

/** The SDK key API the signer needs beyond the app's typed subset. */
interface FullPrivateKey extends SdkPrivateKey {
  toPublicKey(): { toXOnlyPublicKey(): { toString(): string; free?(): void }; free?(): void };
  free?(): void;
}

export interface PendingRequest {
  id: string;
  built: BuiltTx;
  summary: SignerSummary;
}

export interface SignerCoreDeps {
  sdk: KaspaSdk;
  kob: KobWasm;
  post(m: SignerToApp): void;
}

export interface SignerCore {
  /** parses and holds the key; posts `connected`. Throws `SignerKeyError` (whose text never contains the input). */
  importKey(text: string): Hex;
  hasKey(): boolean;
  pubkey(): Hex | null;
  /** frees the key and drops a pending request (rejected as cancelled); posts `forgotten` */
  forget(): void;
  /** a validated app message: a `sign` request becomes the pending request to show (returned), anything else is handled here */
  receive(m: AppToSigner): PendingRequest | null;
  pending(): PendingRequest | null;
  /** signs the pending request `id` (ONLY from the user's click in the signer page); posts `signed`, or `rejected` when it cannot */
  approve(id: string): boolean;
  reject(id: string, reason?: RejectReason): void;
}

const KEY_TEXT = /^(?:0x)?([0-9a-fA-F]{64})$/;

export function createSignerCore(deps: SignerCoreDeps): SignerCore {
  const { sdk, kob, post } = deps;
  let key: FullPrivateKey | null = null;
  let pub: Hex | null = null;
  let pending: PendingRequest | null = null;

  const drop = (reason: RejectReason) => {
    if (pending) post(toApp({ type: 'rejected', id: pending.id, reason }));
    pending = null;
  };

  const free = () => {
    try {
      key?.free?.();
    } catch {
      /* already freed */
    }
    key = null;
    pub = null;
  };

  const sign = (built: BuiltTx): InputSignature[] => {
    // the SDK computes each digest from the transaction itself; kob-wasm `finalize` then checks them against the request's digests and the
    // script engine runs the result: a request whose digests or scripts do not belong to this transaction is refused, not signed blindly
    const tx = sdk.Transaction.deserializeFromSafeJSON(JSON.stringify(built.tx));
    const sigs = built.sign.map((req) => {
      const push = sdk.createInputSignature(tx, req.inputIndex, key!, sdk.SighashType.All);
      if (!/^41[0-9a-f]{128}01$/.test(push)) throw new Error('unexpected signature encoding');
      return { inputIndex: req.inputIndex, signature: push.slice(2) };
    });
    kob.validate(kob.finalize(built, sigs, { tightenBudgets: true }));
    return sigs;
  };

  const forget = () => {
    drop('cancelled');
    free();
    post(toApp({ type: 'forgotten' }));
  };

  return {
    importKey(text) {
      const m = KEY_TEXT.exec(text.trim());
      if (!m) throw new SignerKeyError();
      let next: FullPrivateKey;
      let x: string;
      try {
        next = new sdk.PrivateKey(m[1]!.toLowerCase()) as FullPrivateKey;
        const pk = next.toPublicKey();
        const xo = pk.toXOnlyPublicKey();
        x = xo.toString().toLowerCase();
        xo.free?.();
        pk.free?.();
      } catch {
        throw new SignerKeyError(); // the SDK's own message is not passed on
      }
      if (!/^[0-9a-f]{64}$/.test(x)) {
        next.free?.();
        throw new SignerKeyError();
      }
      drop('cancelled');
      free();
      key = next;
      pub = x;
      post(toApp({ type: 'connected', pubkey: x }));
      return x;
    },
    hasKey: () => key !== null,
    pubkey: () => pub,
    forget,
    receive(m) {
      if (m.type === 'forget') {
        forget();
        return null;
      }
      if (m.type === 'cancel') {
        if (pending?.id === m.id) drop('cancelled');
        return null;
      }
      if (!key || !pub) {
        post(toApp({ type: 'rejected', id: m.id, reason: 'no-key' }));
        return null;
      }
      if (pending) {
        post(toApp({ type: 'rejected', id: m.id, reason: 'busy' }));
        return null;
      }
      // every requested signature must be this key's: the signer never signs for another key's input
      if (m.built.sign.some((s) => s.pubkey !== pub)) {
        post(toApp({ type: 'rejected', id: m.id, reason: 'invalid' }));
        return null;
      }
      let summary: SignerSummary;
      try {
        summary = summarize(kob, m.built, pub, m.tokens as TokenHint[]);
      } catch {
        post(toApp({ type: 'rejected', id: m.id, reason: 'invalid' }));
        return null;
      }
      pending = { id: m.id, built: m.built, summary };
      return pending;
    },
    pending: () => pending,
    approve(id) {
      if (!pending || pending.id !== id || !key) return false;
      const req = pending;
      pending = null;
      let sigs: InputSignature[];
      try {
        sigs = sign(req.built);
      } catch {
        post(toApp({ type: 'rejected', id, reason: 'invalid' }));
        return false;
      }
      post(toApp({ type: 'signed', id, signatures: sigs }));
      return true;
    },
    reject(id, reason = 'user') {
      if (pending?.id === id) drop(reason);
    },
  };
}
