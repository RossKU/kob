// "Private key" wallet: the app's side of the sandboxed signer frame (signer.html, src/signer). The key never enters this page: it is typed into
// the frame, held in the frame's memory and used there after the user approves in the frame. This side only sees the public key and the
// signatures, through the validated messages of signer/protocol.ts. Reloading the page or removing the frame forgets the key.
import type { BuiltTx, Hex, InputSignature } from '../kob/types';
import { normalizeNetwork, pubkeyToAddress, type KaspaSdk } from '../data/kaspa-sdk';
import { toSigner, parseSignerMessage, type AppToSigner, type RejectReason, type TokenHint } from '../signer/protocol';
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
}

interface Waiting {
  id: string;
  resolve(s: InputSignature[]): void;
  reject(e: WalletError): void;
  timer: ReturnType<typeof setTimeout>;
  /** a re-timed request shows the frame only after RETIME_SHOW_MS: the frame signs it at once when nothing got worse (no prompt to show) */
  show?: ReturnType<typeof setTimeout>;
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
      case 'signed':
        if (this.waiting?.id === m.id) {
          this.lastSigned = m.id;
          this.settle(m.signatures, null);
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

  private settle(sigs: InputSignature[] | null, err: WalletError | null): void {
    const w = this.waiting;
    if (!w) return;
    this.waiting = null;
    clearTimeout(w.timer);
    if (w.show !== undefined) clearTimeout(w.show);
    this.events.onRequest?.(false);
    if (sigs) w.resolve(sigs);
    else w.reject(err!);
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
      this.waiting = { id, resolve, reject, timer };
      // a re-timed request is signed without a prompt when nothing got worse: the frame shows only if it is still waiting by then
      if (retimeOf) this.waiting.show = setTimeout(() => this.waiting?.id === id && this.events.onRequest?.(true), RETIME_SHOW_MS);
      else this.events.onRequest?.(true);
      this.post(toSigner({ type: 'sign', id, built, tokens, ...(retimeOf ? { retimeOf } : {}) }));
    });
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
    async signTx(built: BuiltTx, opts: SignOptions = {}): Promise<InputSignature[]> {
      const host = deps.host();
      const pubkey = pub();
      if (!host) throw new WalletError('other', 'No private key is loaded.');
      if (built.sign.some((s) => s.pubkey !== pubkey)) throw new WalletError('other', 'This transaction asks for a signature of another key.');
      const sigs = await host.sign(built, deps.tokenHints?.(built) ?? [], opts.timeoutMs, opts.retime === true);
      // one signature per request, in the request's order: anything else is not an answer to this request
      if (sigs.length !== built.sign.length || sigs.some((s, i) => s.inputIndex !== built.sign[i]!.inputIndex)) {
        throw new WalletError('no-signature', 'The private key signer answered with other signatures than requested.');
      }
      return sigs;
    },
  };
}
