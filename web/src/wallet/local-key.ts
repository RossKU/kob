// The "Private key" wallet: the app's side of the sandboxed signer frame (signer.html,
// src/signer). The key never enters this page. It is typed into the frame, held in the frame's
// memory and used there after the user approves in the frame. This side only sees the public key
// and the signatures, through the validated messages of signer/protocol.ts. Reloading the page or
// removing the frame forgets the key.
import type { BuiltTx, Hex, InputSignature } from '../kob/types';
import { normalizeNetwork, pubkeyToAddress, type KaspaSdk } from '../data/kaspa-sdk';
import type { RegistryRow } from '../signer/table';
import { toSigner, parseSignerMessage, type AppToSigner, type GenesisClaim, type RejectReason, type SignerNote, type TokenHint } from '../signer/protocol';
import { SIGN_TIMEOUT_MS } from './sigs';
import { WalletError, type SignOptions, type WalletAdapter, type WalletInfo } from './types';

/** The wallet label shown for the private-key signer. */
export const LOCAL_KEY_LABEL = 'Private key';

/** What the app does when the signer frame reports something. */
export interface SignerHostEvents {
  /** The frame loaded a key, given as its x-only public key. */
  onConnected?(pubkey: Hex): void;
  /** The frame forgot its key. */
  onForgotten?(): void;
  /**
   * A signing request is waiting for the user in the frame (true) or is settled (false). The frame
   * must be shown meanwhile.
   */
  onRequest?(active: boolean): void;
  /** The frame's content height in CSS px. The frame is sized to it. */
  onHeight?(px: number): void;
}

interface Waiting {
  id: string;
  /** `prep` holds the signatures of the request's preparation, for a creation (`signChain`). */
  resolve(s: InputSignature[], prep?: InputSignature[]): void;
  reject(e: WalletError): void;
  timer: ReturnType<typeof setTimeout>;
  /**
   * What the frame was given for this request, by transaction id: the request and every update of
   * it.
   */
  shown: Map<string, BuiltTx>;
  /**
   * A re-timed request shows the frame only after RETIME_SHOW_MS. The frame signs it at once when
   * nothing got worse, and then there is no prompt to show.
   */
  show?: ReturnType<typeof setTimeout>;
  /**
   * For a creation: the preparation shown and signed with the request. Its signatures must come
   * back too.
   */
  prep?: BuiltTx;
}

/**
 * How long a re-timed request waits before the frame is shown. When the frame accepts it, it is
 * signed in milliseconds without a prompt.
 */
const RETIME_SHOW_MS = 2000;

const REJECT_TEXT: Record<RejectReason, [WalletError['code'], string]> = {
  user: ['rejected', 'You rejected the request in the private key signer.'],
  cancelled: ['rejected', 'The signing request was cancelled.'],
  busy: ['other', 'The private key signer is busy with another request.'],
  'no-key': ['other', 'The private key signer holds no key: connect again.'],
  invalid: ['other', 'The private key signer refused this transaction (it is not signable with this key as built).'],
};

/**
 * The app's end of the signer frame. `post` sends to the frame
 * (`frame.contentWindow.postMessage(m, '*')`), whose origin is opaque. `receive` takes what the
 * page's `message` listener got from the frame after the sender check (`fromSigner`), and drops
 * anything that is not protocol.
 */
export class SignerHost {
  private pub: Hex | null = null;
  private waiting: Waiting | null = null;
  private seq = 0;
  /** The id of the request the frame signed last, which is what a re-timed request names. */
  private lastSigned: string | null = null;
  /**
   * The transaction the frame signed last: the request, or the update of it that it showed last.
   */
  private lastSignedTx: BuiltTx | null = null;
  /**
   * The registry table the app sends with each request (signer/table.ts). The frame trusts a listed
   * token's ticker and decimals only if they are the pinned ones.
   */
  registryTable: RegistryRow[] | undefined;

  constructor(private readonly post: (m: AppToSigner) => void, private readonly events: SignerHostEvents = {}) {}

  /** The public key of the loaded key, or null. */
  pubkey(): Hex | null {
    return this.pub;
  }

  /** Handles one message from the frame. Anything that is not protocol is dropped. */
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
          // The frame names the transaction it signed. It must be one given for this request,
          // otherwise the answer is not for what was asked.
          const tx = m.txid === undefined ? null : (this.waiting.shown.get(m.txid) ?? null);
          if (m.txid !== undefined && !tx) {
            this.settle(null, new WalletError('no-signature', 'The private key signer signed a transaction it was not given.'));
            return;
          }
          // A creation gets the preparation's signatures with the genesis's, and a plain request
          // gets none.
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
   * Asks the frame to show a token creation on one confirmation, the genesis `built` and the
   * preparation `prep` it spends, and to sign both once the user approves there. Resolves with the
   * signatures of each.
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
      this.post(toSigner({ type: 'sign', id, built, tokens, ...this.table(), prep, ...(genesis ? { genesis } : {}) }));
    });
  }

  /**
   * Asks the frame to show `built` and sign it once the user approves there. `retime` marks a
   * request that re-times the one signed last (retime.ts).
   */
  sign(built: BuiltTx, tokens: TokenHint[], timeoutMs = SIGN_TIMEOUT_MS, retime = false, notes: SignerNote[] = []): Promise<InputSignature[]> {
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
      // A re-timed request is signed without a prompt when nothing got worse, so the frame is shown
      // only if it is still waiting by then.
      if (retimeOf) this.waiting.show = setTimeout(() => this.waiting?.id === id && this.events.onRequest?.(true), RETIME_SHOW_MS);
      else this.events.onRequest?.(true);
      this.post(toSigner({ type: 'sign', id, built, tokens, ...this.table(), ...(notes.length > 0 ? { notes } : {}), ...(retimeOf ? { retimeOf } : {}) }));
    });
  }

  private table(): { registry?: RegistryRow[] } {
    return this.registryTable ? { registry: this.registryTable } : {};
  }

  /**
   * The transaction the frame signed last. Null when none, or when it was signed before the frame
   * named it.
   */
  signedTx(): BuiltTx | null {
    return this.lastSignedTx;
  }

  /**
   * The waiting request planned again. The frame shows it in its place when its own check lets it
   * (signer/core.ts). Nothing happens while none waits.
   */
  update(built: BuiltTx, tokens: TokenHint[]): void {
    const w = this.waiting;
    if (!w) return;
    w.shown.set(built.tx.id, built);
    this.post(toSigner({ type: 'update', id: w.id, built, tokens, ...this.table() }));
  }

  /** The node's DAA score now, for the times the frame says. Display only. */
  clock(daa: bigint): void {
    this.post(toSigner({ type: 'clock', daa: daa.toString() }));
  }

  /**
   * The user closed the frame while a request was open, so it is cancelled. That is a neutral
   * rejection.
   */
  cancel(): void {
    const w = this.waiting;
    if (!w) return;
    this.post(toSigner({ type: 'cancel', id: w.id }));
    this.settle(null, new WalletError('rejected', 'The signing request was cancelled.'));
  }

  /**
   * Tells the frame to drop its key. The caller removes the frame too, which forgets it in any
   * case.
   */
  forget(): void {
    this.post(toSigner({ type: 'forget' }));
    this.pub = null;
    this.settle(null, new WalletError('other', 'The private key signer was disconnected.'));
  }
}

/** What `createLocalKeyAdapter` needs. */
export interface LocalKeyDeps {
  host: () => SignerHost | null;
  sdk: KaspaSdk;
  /**
   * Registry labels of the transaction's tokens, shown by the frame next to the amounts it decodes
   * itself.
   */
  tokenHints?: (built: BuiltTx) => TokenHint[];
  /**
   * The registry table the frame checks against the hash pinned in its bundle (signer/table.ts).
   * Without it every registry label is shown as unverified.
   */
  registryTable?: () => RegistryRow[];
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
    // The frame signs any input of its key by digest, covenant inputs included. It is the same
    // `push(sig65)` KasWare returns.
    covenantSigning: 'proven',
    // the frame signs what it is shown: no node of its own to look the inputs up on
    inputPolicy: 'offline',
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
      host.registryTable = deps.registryTable?.();
      const sigs = await host.signChain(prep, built, deps.tokenHints?.(built) ?? [], opts.timeoutMs, opts.genesisClaim);
      // One signature per request, in each request's order. Anything else is not an answer to this
      // request.
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
      // The clock goes first, so the frame's first view of the request already says its times from
      // it.
      if (opts.clockDaa !== undefined) host.clock(opts.clockDaa);
      host.registryTable = deps.registryTable?.();
      const pending = host.sign(built, deps.tokenHints?.(built) ?? [], opts.timeoutMs, opts.retime === true, opts.signerNotes);
      opts.live?.attach({ replace: (b) => host.update(b, deps.tokenHints?.(b) ?? []), clock: (daa) => host.clock(daa) });
      const sigs = await pending;
      // The transaction signed: the request, or the update of it that the frame showed last.
      const signed = host.signedTx() ?? built;
      if (opts.live) opts.live.signed = signed;
      else if (signed !== built) throw new WalletError('no-signature', 'The private key signer signed another transaction than requested.');
      // One signature per request, in the request's order. Anything else is not an answer to this
      // request.
      if (sigs.length !== signed.sign.length || sigs.some((s, i) => s.inputIndex !== signed.sign[i]!.inputIndex)) {
        throw new WalletError('no-signature', 'The private key signer answered with other signatures than requested.');
      }
      return sigs;
    },
  };
}
