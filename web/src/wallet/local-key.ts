// "Private key" wallet: the app's side of the sandboxed signer frame (signer.html, src/signer). The key never enters this page: it is typed into
// the frame, held in the frame's memory and used there after the user approves in the frame. This side only sees the public key and the
// signatures, through the validated messages of signer/protocol.ts. Reloading the page or removing the frame forgets the key.
import type { BuiltTx, Hex, InputSignature } from '../kob/types';
import { normalizeNetwork, pubkeyToAddress, type KaspaSdk } from '../data/kaspa-sdk';
import { toSigner, parseSignerMessage, type AppToSigner, type GenesisClaim, type RejectReason, type TokenHint } from '../signer/protocol';
import { SIGN_TIMEOUT_MS } from './sigs';
import { WalletError, type SignOptions, type WalletAdapter, type WalletInfo } from './types';

export const LOCAL_KEY_LABEL = 'Private key';

export interface SignerHostEvents {
  /** the frame loaded a key (its x-only public key) */
  onConnected?(pubkey: Hex): void;
  /** the frame forgot its key */
  onForgotten?(): void;
  /** a signing request is waiting for the user in the frame (true) or is settled (false): the frame must be shown meanwhile */
  onRequest?(active: boolean): void;
  /** the frame's content height (CSS px): the frame is sized to it */
  onHeight?(px: number): void;
}

interface Waiting {
  id: string;
  /** `prep`: the signatures of the request's preparation (a creation, `signChain`) */
  resolve(s: InputSignature[], prep?: InputSignature[]): void;
  reject(e: WalletError): void;
  timer: ReturnType<typeof setTimeout>;
  /** what the frame was given for this request, by transaction id: the request and every update of it */
  shown: Map<string, BuiltTx>;
  /** a re-timed request shows the frame only after RETIME_SHOW_MS: the frame signs it at once when nothing got worse (no prompt to show) */
  show?: ReturnType<typeof setTimeout>;
  /** a creation: the preparation shown and signed with the request; its signatures must come back too */
  prep?: BuiltTx;
}

/** How long a re-timed request waits before the frame is shown (it is signed in milliseconds, without a prompt, when the frame accepts it). */
const RETIME_SHOW_MS = 2000;

const REJECT_TEXT: Record<RejectReason, [WalletError['code'], string]> = {
  user: ['rejected', 'You rejected the request in the private key signer.'],
  cancelled: ['rejected', 'The signing request was cancelled.'],
  busy: ['other', 'The private key signer is busy with another request.'],
  'no-key': ['other', 'The private key signer holds no key: connect again.'],
  invalid: ['other', 'The private key signer refused this transaction (it is not signable with this key as built).'],
};

/**
 * The app's end of the signer frame. `post` sends to the frame (`frame.contentWindow.postMessage(m, '*')`: its origin is opaque); `receive` takes
 * what the page's `message` listener got from the frame AFTER the sender check (`fromSigner`), and drops anything that is not protocol.
 */
export class SignerHost {
  private pub: Hex | null = null;
  private waiting: Waiting | null = null;
  private seq = 0;
  /** the id of the request the frame signed last (what a re-timed request names) */
  private lastSigned: string | null = null;
  /** the transaction the frame signed last (the request, or the update of it it showed last) */
  private lastSignedTx: BuiltTx | null = null;

  constructor(private readonly post: (m: AppToSigner) => void, private readonly events: SignerHostEvents = {}) {}

  pubkey(): Hex | null {
    return this.pub;
  }

  receive(data: unknown): void {
    const m = parseSignerMessage(data);
    if (!m) return;
    switch (m.type) {
      case 'connected':
        this.pub = m.pubkey;
        this.events.onConnected?.(m.pubkey);
        return;
      case 'forgotten':
        this.pub = null;
        this.settle(null, new WalletError('other', 'The private key signer forgot its key.'));
        this.events.onForgotten?.();
        return;
      case 'height':
        this.events.onHeight?.(m.px);
        return;
      case 'signed':
        if (this.waiting?.id === m.id) {
          // the frame names the transaction it signed: one it was given for this request, else the answer is not for what was asked
          const tx = m.txid === undefined ? null : (this.waiting.shown.get(m.txid) ?? null);
          if (m.txid !== undefined && !tx) {
            this.settle(null, new WalletError('no-signature', 'The private key signer signed a transaction it was not given.'));
            return;
          }
          // a creation: the preparation's signatures with the genesis's; none for a plain request
          if (!!this.waiting.prep !== !!m.prepSignatures) {
            this.settle(null, new WalletError('no-signature', 'The private key signer answered with other signatures than requested.'));
            return;
          }
          this.lastSigned = m.id;
          this.lastSignedTx = tx;
          this.settle(m.signatures, null, m.prepSignatures);
        }
        return;
      case 'rejected': {
        if (this.waiting?.id !== m.id) return;
        const [code, text] = REJECT_TEXT[m.reason];
        this.settle(null, new WalletError(code, text));
        return;
      }
      default:
        return;
    }
  }

  private settle(sigs: InputSignature[] | null, err: WalletError | null, prep?: InputSignature[]): void {
    const w = this.waiting;
    if (!w) return;
    this.waiting = null;
    clearTimeout(w.timer);
    if (w.show !== undefined) clearTimeout(w.show);
    this.events.onRequest?.(false);
    if (sigs) w.resolve(sigs, prep);
    else w.reject(err!);
  }

  /**
   * Asks the frame to show a token creation (the genesis `built` and the preparation `prep` it spends) on ONE confirmation and sign both once
   * the user approves there. Resolves with the signatures of each.
   */
  signChain(prep: BuiltTx, built: BuiltTx, tokens: TokenHint[], timeoutMs = SIGN_TIMEOUT_MS, genesis?: GenesisClaim): Promise<{ prep: InputSignature[]; built: InputSignature[] }> {
    if (!this.pub) return Promise.reject(new WalletError('other', 'The private key signer holds no key: connect again.'));
    if (this.waiting) return Promise.reject(new WalletError('other', 'The private key signer is busy with another request.'));
    const id = `r${Date.now().toString(36)}${(++this.seq).toString(36)}`;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.post(toSigner({ type: 'cancel', id }));
        this.settle(null, new WalletError('timeout', `Private key signer: no answer after ${Math.round(timeoutMs / 1000)}s.`));
      }, timeoutMs);
      this.waiting = {
        id, timer, prep, shown: new Map([[built.tx.id, built]]), reject,
        resolve: (sigs, prepSigs) => (prepSigs ? resolve({ prep: prepSigs, built: sigs }) : reject(new WalletError('no-signature', 'The private key signer did not sign the preparation.'))),
      };
      this.events.onRequest?.(true);
      this.post(toSigner({ type: 'sign', id, built, tokens, prep, ...(genesis ? { genesis } : {}) }));
    });
  }

  /** Asks the frame to show `built` and sign it once the user approves there (`retime`: it re-times the request signed last, retime.ts). */
  sign(built: BuiltTx, tokens: TokenHint[], timeoutMs = SIGN_TIMEOUT_MS, retime = false): Promise<InputSignature[]> {
    if (!this.pub) return Promise.reject(new WalletError('other', 'The private key signer holds no key: connect again.'));
    if (this.waiting) return Promise.reject(new WalletError('other', 'The private key signer is busy with another request.'));
    const id = `r${Date.now().toString(36)}${(++this.seq).toString(36)}`;
    return new Promise<InputSignature[]>((resolve, reject) => {
      const timer = setTimeout(() => {
        this.post(toSigner({ type: 'cancel', id }));
        this.settle(null, new WalletError('timeout', `Private key signer: no answer after ${Math.round(timeoutMs / 1000)}s.`));
      }, timeoutMs);
      const retimeOf = retime && this.lastSigned ? this.lastSigned : undefined;
      this.waiting = { id, resolve, reject, timer, shown: new Map([[built.tx.id, built]]) };
      // a re-timed request is signed without a prompt when nothing got worse: the frame shows only if it is still waiting by then
      if (retimeOf) this.waiting.show = setTimeout(() => this.waiting?.id === id && this.events.onRequest?.(true), RETIME_SHOW_MS);
      else this.events.onRequest?.(true);
      this.post(toSigner({ type: 'sign', id, built, tokens, ...(retimeOf ? { retimeOf } : {}) }));
    });
  }

  /** The transaction the frame signed last (null: none, or one signed before the frame named it). */
  signedTx(): BuiltTx | null {
    return this.lastSignedTx;
  }

  /** The waiting request planned again: the frame shows it in its place when its own check lets it (signer/core.ts); nothing while none waits. */
  update(built: BuiltTx, tokens: TokenHint[]): void {
    const w = this.waiting;
    if (!w) return;
    w.shown.set(built.tx.id, built);
    this.post(toSigner({ type: 'update', id: w.id, built, tokens }));
  }

  /** The node's DAA score now, for the times the frame says (display only). */
  clock(daa: bigint): void {
    this.post(toSigner({ type: 'clock', daa: daa.toString() }));
  }

  /** The user closed the frame while a request was open: it is cancelled (a neutral rejection). */
  cancel(): void {
    const w = this.waiting;
    if (!w) return;
    this.post(toSigner({ type: 'cancel', id: w.id }));
    this.settle(null, new WalletError('rejected', 'The signing request was cancelled.'));
  }

  /** Tells the frame to drop its key (the caller removes the frame too, which forgets it in any case). */
  forget(): void {
    this.post(toSigner({ type: 'forget' }));
    this.pub = null;
    this.settle(null, new WalletError('other', 'The private key signer was disconnected.'));
  }
}

export interface LocalKeyDeps {
  host: () => SignerHost | null;
  sdk: KaspaSdk;
  /** registry labels of the transaction's tokens, shown by the frame next to the amounts it decodes itself */
  tokenHints?: (built: BuiltTx) => TokenHint[];
}

/** The `WalletAdapter` over the signer frame (id `local-key`). */
export function createLocalKeyAdapter(deps: LocalKeyDeps): WalletAdapter {
  const pub = (): Hex => {
    const p = deps.host()?.pubkey();
    if (!p) throw new WalletError('other', 'No private key is loaded.');
    return p;
  };
  let network = '';
  const info = (): WalletInfo => {
    const pubkey = pub();
    return { id: 'local-key', label: LOCAL_KEY_LABEL, address: pubkeyToAddress(deps.sdk, pubkey, network), pubkey, network, version: '' };
  };
  return {
    id: 'local-key',
    label: LOCAL_KEY_LABEL,
    // the frame signs any input of its key by digest (covenant inputs included): the same `push(sig65)` KasWare returns
    covenantSigning: 'proven',
    detect: () => true,
    async connect(wantNetwork: string): Promise<WalletInfo> {
      network = normalizeNetwork(wantNetwork);
      return info();
    },
    async refresh(): Promise<WalletInfo> {
      return info();
    },
    async signChain([prep, built]: [BuiltTx, BuiltTx], opts: SignOptions = {}): Promise<[InputSignature[], InputSignature[]]> {
      const host = deps.host();
      const pubkey = pub();
      if (!host) throw new WalletError('other', 'No private key is loaded.');
      if ([...prep.sign, ...built.sign].some((s) => s.pubkey !== pubkey)) throw new WalletError('other', 'This transaction asks for a signature of another key.');
      const sigs = await host.signChain(prep, built, deps.tokenHints?.(built) ?? [], opts.timeoutMs, opts.genesisClaim);
      // one signature per request, in each request's order: anything else is not an answer to this request
      for (const [b, got] of [[prep, sigs.prep], [built, sigs.built]] as const) {
        if (got.length !== b.sign.length || got.some((s, i) => s.inputIndex !== b.sign[i]!.inputIndex)) {
          throw new WalletError('no-signature', 'The private key signer answered with other signatures than requested.');
        }
      }
      return [sigs.prep, sigs.built];
    },
    async signTx(built: BuiltTx, opts: SignOptions = {}): Promise<InputSignature[]> {
      const host = deps.host();
      const pubkey = pub();
      if (!host) throw new WalletError('other', 'No private key is loaded.');
      if (built.sign.some((s) => s.pubkey !== pubkey)) throw new WalletError('other', 'This transaction asks for a signature of another key.');
      // the clock first: the frame's first view of the request already says its times from it
      if (opts.clockDaa !== undefined) host.clock(opts.clockDaa);
      const pending = host.sign(built, deps.tokenHints?.(built) ?? [], opts.timeoutMs, opts.retime === true);
      opts.live?.attach({ replace: (b) => host.update(b, deps.tokenHints?.(b) ?? []), clock: (daa) => host.clock(daa) });
      const sigs = await pending;
      // the transaction signed: the request, or the update of it the frame showed last
      const signed = host.signedTx() ?? built;
      if (opts.live) opts.live.signed = signed;
      else if (signed !== built) throw new WalletError('no-signature', 'The private key signer signed another transaction than requested.');
      // one signature per request, in the request's order: anything else is not an answer to this request
      if (sigs.length !== signed.sign.length || sigs.some((s, i) => s.inputIndex !== signed.sign[i]!.inputIndex)) {
        throw new WalletError('no-signature', 'The private key signer answered with other signatures than requested.');
      }
      return sigs;
    },
  };
}
