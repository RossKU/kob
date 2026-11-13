// Kastle adapter (`window.kastle`), a reference wallet behind `features.kastle`. It was exercised
// on TN10 with Kastle 2.60.1.
// - `signTx(network, txJson, scripts)`. A plain call leaves P2SH (covenant) inputs unsigned and
//   reports no error, so the call must pass `scripts` for them:
//   `[{ inputIndex, scriptHex: '', signType: 'All' }]`. The empty-script variant returns just
//   `push(sig65)`.
// - `getAccount().publicKey` is 33 bytes, compressed. The x-only key is the last 32.
// - Its popup shows misleading balance changes for covenant transactions, such as the P2SH input
//   as "Change to your balance". The app's own confirmation screen is the safety layer.
import type { BuiltTx, InputSignature } from '../kob/types';
import { normalizeNetwork } from '../data/kaspa-sdk';
import { WalletError, type SignOptions, type WalletAdapter, type WalletInfo } from './types';
import { CONNECT_TIMEOUT_MS, REFRESH_TIMEOUT_MS, SIGN_TIMEOUT_MS, extractSignature, collectSignatures, normalizePubkey, toWalletError, withTimeout } from './sigs';
import { DEFAULT_POLL_MS, hasEmitter, listenProvider, pollProvider, type EmitterLike } from './events';

/** One entry of Kastle's `scripts` argument of `signTx`, for a covenant input. */
export interface KastleScript {
  inputIndex: number;
  scriptHex: string;
  signType: 'All';
}

/** The Kastle provider object, as the extension injects it. */
export interface KastleProvider extends EmitterLike {
  connect(): Promise<boolean>;
  getAccount(): Promise<{ address: string; publicKey: string }>;
  request(method: string, params?: unknown): Promise<unknown>;
  signTx(network: string, txJson: string, scripts?: KastleScript[]): Promise<unknown>;
}

/** What the Kastle adapter needs. */
export interface KastleDeps {
  getProvider?: () => KastleProvider | null | undefined;
}

const LABEL = 'Kastle';

/** The adapter for Kastle. It reads the provider at call time. */
export function createKastleAdapter(deps: KastleDeps = {}): WalletAdapter {
  const provider = () => (deps.getProvider ? deps.getProvider() : (globalThis as { kastle?: KastleProvider }).kastle) ?? null;
  const need = (): KastleProvider => {
    const p = provider();
    if (!p) throw new WalletError('unsupported', 'Kastle is not installed in this browser.');
    return p;
  };
  return {
    id: 'kastle',
    label: LABEL,
    // Some builds return covenant inputs unsigned, so this wallet is probed once per account before
    // the first order.
    covenantSigning: 'unknown',
    detect: () => !!provider(),

    async connect(wantNetwork: string): Promise<WalletInfo> {
      const p = need();
      const want = normalizeNetwork(wantNetwork);
      try {
        const ok = await withTimeout(p.connect(), CONNECT_TIMEOUT_MS, `${LABEL}.connect`);
        if (!ok) throw new WalletError('rejected', 'Kastle did not grant the connection.');
        let network = normalizeNetwork(await p.request('kas:get_network'));
        if (network !== want) {
          try {
            await withTimeout(p.request('kas:switch_network', want), CONNECT_TIMEOUT_MS, `${LABEL}.switch_network`);
          } catch {
            /* reported through the result's `network` */
          }
          network = normalizeNetwork(await p.request('kas:get_network'));
        }
        const acc = await p.getAccount();
        let version = '?';
        try {
          version = String((await p.request('kas:get_version')) ?? '?');
        } catch {
          /* optional */
        }
        return { id: 'kastle', label: LABEL, address: acc.address, pubkey: normalizePubkey(acc.publicKey), network, version };
      } catch (e) {
        throw toWalletError(e, LABEL);
      }
    },

    async refresh(): Promise<WalletInfo> {
      const p = need();
      try {
        const acc = await withTimeout(p.getAccount(), REFRESH_TIMEOUT_MS, `${LABEL}.getAccount`);
        if (!acc || typeof acc.address !== 'string' || !acc.address) throw new WalletError('other', `${LABEL} no longer exposes an account.`);
        const network = normalizeNetwork(await withTimeout(p.request('kas:get_network') as Promise<string>, REFRESH_TIMEOUT_MS, `${LABEL}.get_network`));
        let version = '?';
        try {
          version = String((await p.request('kas:get_version')) ?? '?');
        } catch {
          /* optional */
        }
        return { id: 'kastle', label: LABEL, address: acc.address, pubkey: normalizePubkey(acc.publicKey), network, version };
      } catch (e) {
        throw toWalletError(e, LABEL);
      }
    },

    subscribe(onEvent, o = {}) {
      const p = provider();
      if (!p) return () => undefined;
      // Kastle's page API emits accountsChanged([address], or [] when the site lost its
      // connection) and networkChanged(network id). It emits the same as kas:account_changed and
      // kas:network_changed, so the first pair is enough.
      if (hasEmitter(p)) return listenProvider(p, { accounts: ['accountsChanged'], network: ['networkChanged'] }, onEvent);
      return pollProvider(
        async () => {
          const acc = await p.getAccount();
          return { account: acc?.address ?? '', network: normalizeNetwork(await p.request('kas:get_network')) };
        },
        onEvent,
        o.pollMs ?? DEFAULT_POLL_MS,
      );
    },

    async signTx(built: BuiltTx, opts: SignOptions = {}): Promise<InputSignature[]> {
      const p = need();
      const network = normalizeNetwork(opts.network ?? '');
      if (!network) throw new WalletError('unsupported', 'Kastle needs the network name to sign.');
      // Covenant inputs only. P2PK inputs are signed by a plain call.
      const scripts: KastleScript[] = built.sign.filter((s) => s.redeemScript !== null).map((s) => ({ inputIndex: s.inputIndex, scriptHex: '', signType: 'All' }));
      const txJson = JSON.stringify(built.tx);
      let response: unknown;
      try {
        const call = scripts.length ? p.signTx(network, txJson, scripts) : p.signTx(network, txJson);
        response = await withTimeout(call, opts.timeoutMs ?? SIGN_TIMEOUT_MS, `${LABEL}.signTx`);
      } catch (e) {
        throw toWalletError(e, LABEL);
      }
      // A covenant input Kastle did not sign comes back with an empty signature script and no
      // error. Say what happened.
      for (const s of built.sign) {
        try {
          extractSignature(response, s.inputIndex);
        } catch (e) {
          if (e instanceof WalletError && e.code === 'no-signature' && s.redeemScript !== null) {
            throw new WalletError('no-signature', `Kastle returned input ${s.inputIndex} (a covenant input) unsigned. This wallet build cannot sign this transaction.`);
          }
          throw e;
        }
      }
      return collectSignatures(built, response, LABEL);
    },
  };
}
