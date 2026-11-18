// The private-key signer's logic, without DOM. signer/main.ts is the page around it, and unit
// tests drive it directly.
//
// The key is parsed into the official SDK's `PrivateKey`, whose bytes live in wasm memory. It is
// held in this closure only, never in storage, a message or an error text. `forget()` frees it.
// The pending request is signed only through `approve(id)`, which the page calls from a trusted
// click on its own button. Two kinds of request are signed in `receive` without a new click: a
// re-timed request on which nothing the user approved got worse, and the next link of a merge
// session the user allowed. Both rest on an approval the user gave before and on this frame's
// own checks. So a request from the app alone never produces a signature.
import type { KaspaSdk, SdkPrivateKey } from '../data/kaspa-sdk';
import type { BuiltTx, Hex, InputSignature } from '../kob/types';
import type { KobWasm } from '../kob/wasm';
import { toApp, type AppToSigner, type RejectReason, type SessionCloseReason, type SignerNote, type SignerToApp, type TokenHint } from './protocol';
import { creationSummary, sessionSummary, summarize, type SignerSummary } from './summary';
import { admitStep, mergeFacts, openState, recordStep, sessionTerms, stepsLeft, type MergeFacts, type SessionState, type SessionTerms } from './session';
import { RETIME_MAX_COUNT, RETIME_WINDOW_MS, sameOrBetter } from './retime';
import type { RegistryRow } from './table';
import { trustedHints } from './trusted-hints';

/** The error for key text that is not a private key. Its message never contains the input. */
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

/** The request on screen, waiting for the user's Approve or Reject. */
export interface PendingRequest {
  id: string;
  /** What Approve signs, which is the request or the update of it shown last. */
  built: BuiltTx;
  summary: SignerSummary;
  /**
   * How the last change came.
   * - `new` is the request.
   * - `silent` is an update that this frame's decoding finds no worse for the user than what they
   *   were shown. The figures follow in place.
   * - `shown` is an update that is worse somewhere. It is shown as updated, and Approve is held
   *   for a moment.
   */
  change: 'new' | 'silent' | 'shown';
  /** What an update is compared with: the request as first shown, or the last update shown. */
  baseline: SignerSummary;
  /**
   * The app's hints (protocol.ts `SignerNote`), validated. The frame words them, and they never
   * block. They are fixed with the request, and an update keeps them.
   */
  notes: SignerNote[];
  /**
   * For a token creation, the KCC-23 preparation whose output 0 `built` spends. It is signed with
   * `built` on the one approval.
   */
  prep?: BuiltTx;
  /**
   * For a merge plan to allow once (session.ts), the terms this frame derived from it. Approve then
   * signs nothing and opens the session.
   */
  session?: SessionTerms;
}

/** What the signer core needs from its page. */
export interface SignerCoreDeps {
  sdk: KaspaSdk;
  kob: KobWasm;
  post(m: SignerToApp): void;
  /**
   * A clock in ms for re-time windows and session expiry. The page passes its monotonic clock, and
   * tests pass their own.
   */
  now?: () => number;
  /** A wall clock in ms, for tests. A session also expires on it. */
  wallNow?: () => number;
  /** Random bytes for a session id, for tests. */
  random?: (n: number) => Uint8Array;
  /**
   * The hash of the registry table pinned into this bundle (table.ts). Without it every registry
   * hint is shown as unverified.
   */
  registryHash?: string;
}

/** The signer's logic as the page drives it. */
export interface SignerCore {
  /**
   * Parses and holds the key, and posts `connected`. It throws `SignerKeyError`, whose text never
   * contains the input.
   */
  importKey(text: string): Hex;
  hasKey(): boolean;
  pubkey(): Hex | null;
  /** Frees the key, drops a pending request as cancelled, and posts `forgotten`. */
  forget(): void;
  /**
   * Handles a validated app message.
   *
   * A `sign` request becomes the pending request to show, which is returned. Anything else is
   * handled here. A request that re-times the one the user confirmed last (`retimeOf`, within
   * RETIME_WINDOW_MS) is signed at once when this frame's own decoding finds that nothing the user
   * approved got worse (retime.ts `sameOrBetter`). Otherwise it is shown like any request. An
   * `update` of the pending request replaces what it shows and what Approve signs, and is returned
   * with its `change`. `clock` is the page's, so it gives null here.
   */
  receive(m: AppToSigner): PendingRequest | null;
  pending(): PendingRequest | null;
  /**
   * Signs the pending request `id`, called only from the user's click in the signer page. It posts
   * `signed`, or `rejected` when it cannot sign.
   */
  approve(id: string): boolean;
  reject(id: string, reason?: RejectReason): void;
  /**
   * The live session, for tests and the page. It gives its id, the transactions it still covers and
   * when it expires.
   */
  session(): { sid: string; left: number; expiresAt: number } | null;
}

const KEY_TEXT = /^(?:0x)?([0-9a-fA-F]{64})$/;

/** Creates the signer core. It holds no key until `importKey`. */
export function createSignerCore(deps: SignerCoreDeps): SignerCore {
  const { sdk, kob, post } = deps;
  // The hints of a request as the frame may show them. A registry label shows only when the app's
  // table is the pinned one (table.ts).
  const hintsOf = (m: { tokens: TokenHint[]; registry?: RegistryRow[] }): TokenHint[] => trustedHints(m.tokens, m.registry, deps.registryHash);
  let key: FullPrivateKey | null = null;
  let pub: Hex | null = null;
  let pending: PendingRequest | null = null;
  // The request the user confirmed last, with its decoding and when. It is the only one a re-timed
  // request may stand in for.
  // - `summary` is what the user approved, never a re-timed one. Each re-time is compared with it,
  //   so small differences cannot add up over many re-times.
  // - `retimes` counts the transactions signed without asking on this approval.
  let approved: { id: string; summary: SignerSummary; at: number; retimes: number } | null = null;
  // The one-approval merge session the user allowed (session.ts). It lives in this closure only and
  // is never stored or sent.
  let session: SessionState | null = null;
  const now = deps.now ?? Date.now;
  const wallNow = deps.wallNow ?? Date.now;
  const random = deps.random ?? ((n: number) => crypto.getRandomValues(new Uint8Array(n)));

  const endSession = (reason: SessionCloseReason) => {
    if (session) post(toApp({ type: 'session-closed', sid: session.sid, reason }));
    session = null;
  };

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

  /**
   * Signs `built` with SIGHASH_ALL. It returns the signatures and the id kob-wasm's finalize gives
   * the signed transaction, which is what the node will name it.
   */
  const signWithId = (built: BuiltTx): { sigs: InputSignature[]; txid: Hex } => {
    // The SDK computes each digest from the transaction itself. kob-wasm `finalize` then checks
    // them against the request's digests, and the script engine runs the result. So a request
    // whose digests or scripts do not belong to this transaction is refused, not signed blindly.
    const tx = sdk.Transaction.deserializeFromSafeJSON(JSON.stringify(built.tx));
    const sigs = built.sign.map((req) => {
      const push = sdk.createInputSignature(tx, req.inputIndex, key!, sdk.SighashType.All);
      if (!/^41[0-9a-f]{128}01$/.test(push)) throw new Error('unexpected signature encoding');
      return { inputIndex: req.inputIndex, signature: push.slice(2) };
    });
    const signed = kob.finalize(built, sigs, { tightenBudgets: true });
    kob.validate(signed);
    return { sigs, txid: signed.tx.id };
  };
  const sign = (built: BuiltTx): InputSignature[] => signWithId(built).sigs;

  const forget = () => {
    endSession('forgotten');
    drop('cancelled');
    approved = null;
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
      endSession('forgotten');
      drop('cancelled');
      approved = null;
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
      if (m.type === 'clock') return null;
      if (m.type === 'session-close') {
        if (session?.sid === m.sid) endSession('closed');
        return null;
      }
      if (m.type === 'update') {
        // Only the request on screen is updated, until it is approved or rejected. An update this
        // frame cannot read, or one for another key, is dropped. A creation or a session screen is
        // never updated.
        if (!pending || pending.id !== m.id || !pub || pending.prep || pending.session || m.built.sign.some((s) => s.pubkey !== pub)) return null;
        let next: SignerSummary;
        try {
          next = summarize(kob, m.built, pub, hintsOf(m));
        } catch {
          return null;
        }
        const silent = next.problems === 0 && sameOrBetter(pending.baseline.decoded, next.decoded);
        pending = { ...pending, built: m.built, summary: next, change: silent ? 'silent' : 'shown', baseline: silent ? pending.baseline : next };
        return pending;
      }
      if (!key || !pub) {
        post(toApp({ type: 'rejected', id: m.id, reason: 'no-key' }));
        return null;
      }
      if (pending) {
        post(toApp({ type: 'rejected', id: m.id, reason: 'busy' }));
        return null;
      }
      if (m.type === 'session-open') {
        // One session at a time. A new plan ends the old session, and it is allowed or not on its
        // own screen.
        endSession('closed');
        const hints = hintsOf(m);
        let terms: SessionTerms;
        let first: SignerSummary;
        try {
          if (m.plan.some((b) => b.sign.some((s) => s.pubkey !== pub))) throw new Error('foreign key');
          const links = m.plan.map((built) => {
            const s = summarize(kob, built, pub!, hints);
            const facts = mergeFacts(kob, s.decoded, built, pub!);
            if ('refused' in facts) throw new Error(facts.refused);
            return { built, facts, s };
          });
          const derived = sessionTerms(links);
          if ('refused' in derived) throw new Error(derived.refused);
          terms = derived;
          first = links[0]!.s;
        } catch {
          post(toApp({ type: 'rejected', id: m.id, reason: 'invalid' }));
          return null;
        }
        const summary = sessionSummary(terms, hints, first.decoded);
        pending = { id: m.id, built: m.plan[0]!, summary, change: 'new', baseline: summary, notes: [], session: terms };
        return pending;
      }
      // A request this frame refuses outright while a session is live ends the session too.
      const endLive = () => {
        if (session) endSession(m.session !== undefined && m.session === session.sid ? 'refused' : 'closed');
      };
      // Every requested signature must be this key's. The signer never signs for another key's
      // input.
      if ([...m.built.sign, ...(m.prep?.sign ?? [])].some((s) => s.pubkey !== pub)) {
        endLive();
        post(toApp({ type: 'rejected', id: m.id, reason: 'invalid' }));
        return null;
      }
      if (m.prep) {
        // A token creation. The preparation and the genesis are checked and shown together
        // (summary.ts `creationSummary`) under one approval. It is never a session step.
        endLive();
        let summary: SignerSummary;
        try {
          summary = creationSummary(kob, m.prep, m.built, pub, hintsOf(m), m.genesis);
        } catch {
          endLive();
          post(toApp({ type: 'rejected', id: m.id, reason: 'invalid' }));
          return null;
        }
        pending = { id: m.id, built: m.built, summary, change: 'new', baseline: summary, notes: m.notes ?? [], prep: m.prep };
        return pending;
      }
      let summary: SignerSummary;
      try {
        // The app's lock hints only let a coin to this key's own open creator lock be recognised
        // (summary.ts `summarize`).
        summary = summarize(kob, m.built, pub, hintsOf(m), m.locks, wallNow());
      } catch {
        endLive();
        post(toApp({ type: 'rejected', id: m.id, reason: 'invalid' }));
        return null;
      }
      if (session) {
        // A step of the session is signed without a prompt only when it is exactly the next link
        // of the plan the user allowed. Anything else ends the session and is shown for
        // confirmation as usual. That covers another session id, no session id and any deviation.
        const live = session;
        let facts: MergeFacts | null = null;
        let why: ReturnType<typeof admitStep> = 'deviates';
        try {
          if (m.session === live.sid) {
            const f = mergeFacts(kob, summary.decoded, m.built, pub);
            facts = 'refused' in f ? null : f;
          }
          why = facts ? admitStep(live, facts, now(), wallNow()) : 'deviates';
        } catch {
          // A check that throws refuses. The session ends and the request is shown as usual.
          facts = null;
          why = 'deviates';
        }
        if (facts && why === null) {
          let signed: { sigs: InputSignature[]; txid: Hex };
          try {
            signed = signWithId(m.built);
          } catch {
            endSession('refused');
            post(toApp({ type: 'rejected', id: m.id, reason: 'invalid' }));
            return null;
          }
          recordStep(live, facts, signed.txid);
          post(toApp({ type: 'signed', id: m.id, txid: m.built.tx.id, signatures: signed.sigs }));
          if (stepsLeft(live) === 0) endSession('done');
          return null;
        }
        endSession(why === 'expired' ? 'expired' : m.session === undefined ? 'closed' : 'refused');
      }
      const base = m.retimeOf !== undefined && approved && approved.id === m.retimeOf && now() - approved.at <= RETIME_WINDOW_MS && approved.retimes < RETIME_MAX_COUNT ? approved : null;
      if (base && summary.problems === 0 && sameOrBetter(base.summary.decoded, summary.decoded)) {
        // The transaction the user confirmed, planned again with a later clock. Its terms are the
        // same or better, so their confirmation covers it.
        let sigs: InputSignature[];
        try {
          sigs = sign(m.built);
        } catch {
          post(toApp({ type: 'rejected', id: m.id, reason: 'invalid' }));
          return null;
        }
        approved = { id: m.id, summary: base.summary, at: base.at, retimes: base.retimes + 1 };
        post(toApp({ type: 'signed', id: m.id, txid: m.built.tx.id, signatures: sigs }));
        return null;
      }
      pending = { id: m.id, built: m.built, summary, change: 'new', baseline: summary, notes: m.notes ?? [] };
      return pending;
    },
    pending: () => pending,
    approve(id) {
      if (!pending || pending.id !== id || !key || !pub) return false;
      // A request with a finding the decoder blocks signing on is never signed here, whatever
      // asks. The page's Approve is off for it too.
      if (pending.summary.problems > 0) return false;
      const req = pending;
      pending = null;
      if (req.session) {
        // The user allowed the plan, and nothing is signed now. The session id is minted here and
        // binds the steps to this plan, this key and this frame.
        const sid = [...random(16)].map((b) => b.toString(16).padStart(2, '0')).join('');
        session = openState(sid, pub, req.session, now(), wallNow());
        post(toApp({ type: 'session-opened', id, sid, txs: req.session.links.length, feeMax: req.session.feeMax.toString(), expiresInMs: req.session.ttlMs }));
        return true;
      }
      let sigs: InputSignature[];
      let prepSigs: InputSignature[] | undefined;
      try {
        // For a creation, the preparation is signed first, then the genesis spending it. Each is
        // checked by finalize and the script engine.
        prepSigs = req.prep ? sign(req.prep) : undefined;
        sigs = sign(req.built);
      } catch {
        post(toApp({ type: 'rejected', id, reason: 'invalid' }));
        return false;
      }
      // A creation is never what a re-timed request stands in for.
      approved = req.prep ? null : { id, summary: req.summary, at: now(), retimes: 0 };
      post(toApp({ type: 'signed', id, txid: req.built.tx.id, signatures: sigs, ...(prepSigs ? { prepSignatures: prepSigs } : {}) }));
      return true;
    },
    reject(id, reason = 'user') {
      if (pending?.id !== id) return;
      // Rejecting any request ends the session, as the session screen says.
      endSession('rejected');
      drop(reason);
    },
    session: () => (session ? { sid: session.sid, left: stepsLeft(session), expiresAt: session.expiresAt } : null),
  };
}
