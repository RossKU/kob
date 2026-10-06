// KasWare adapter (`window.kasware`). Proven on TN10 with KasWare 0.10.0 (tools/wallet-gate/RESULTS.md):
//   * `getNetwork()` reports `kaspa_testnet_10` (normalised here);
//   * `signPskt({ txJsonString, options: { signInputs: [{ index, sighashType: 1 }] } })` returns the signed tx JSON (string or object);
//     for a P2SH (covenant) input it puts only `push(sig65)` (66 bytes) in the signature script: the dApp assembles the rest. We do not
//     assemble anything here: only the signature is taken, kob-wasm `finalize` builds the script and checks the signature;
//   * the popup is a generic "Sign Transaction: Spend": it cannot show token semantics, the app's own confirmation screen does.
import type { BuiltTx, InputSignature } from '../kob/types';
import { normalizeNetwork } from '../data/kaspa-sdk';
import { WalletError, type SignOptions, type WalletAdapter, type WalletInfo } from './types';
import { CONNECT_TIMEOUT_MS, REFRESH_TIMEOUT_MS, SIGN_TIMEOUT_MS, collectSignatures, normalizePubkey, toWalletError, withTimeout } from './sigs';
import { DEFAULT_POLL_MS, hasEmitter, listenProvider, pollProvider, type EmitterLike } from './events';

export interface KaswareProvider extends EmitterLike {
  requestAccounts(): Promise<string[]>;
  /** silent: the accounts the site is already connected to (`[]` when not connected) */
  getAccounts?(): Promise<string[]>;
  getNetwork(): Promise<string>;
  switchNetwork(network: string): Promise<unknown>;
  getPublicKey(): Promise<string>;
  getVersion?(): Promise<string>;
  signPskt(request: { txJsonString: string; options: { signInputs: { index: number; sighashType: number }[] } }): Promise<unknown>;
}

export interface KaswareDeps {
  /** read at call time: extensions inject late (default `window.kasware`) */
  getProvider?: () => KaswareProvider | null | undefined;
}

const LABEL = 'KasWare';

/** KasWare's own names first (`kaspa_testnet_10`), then the plain SDK names: the wallet accepted `testnet-10` on TN10 too. */
const switchNames = (net: string): string[] => (net === 'mainnet' ? ['kaspa_mainnet', 'mainnet'] : net === 'testnet-10' ? ['kaspa_testnet_10', 'testnet-10'] : [net]);

async function version(p: KaswareProvider): Promise<string> {
  try {
    return String((await p.getVersion?.()) ?? '?');
  } catch {
    return '?';
  }
}

export function createKaswareAdapter(deps: KaswareDeps = {}): WalletAdapter {
  const provider = () => (deps.getProvider ? deps.getProvider() : (globalThis as { kasware?: KaswareProvider }).kasware) ?? null;
  const need = (): KaswareProvider => {
    const p = provider();
    if (!p) throw new WalletError('unsupported', 'KasWare is not installed in this browser.');
    return p;
  };
  return {
    id: 'kasware',
    label: LABEL,
    // signs covenant (P2SH) inputs: proven on TN10 (tools/wallet-gate), so orders placed with it can be cancelled
    covenantSigning: 'proven',
    detect: () => !!provider(),

    async connect(wantNetwork: string): Promise<WalletInfo> {
      const p = need();
      const want = normalizeNetwork(wantNetwork);
      try {
        const accounts = await withTimeout(p.requestAccounts(), CONNECT_TIMEOUT_MS, `${LABEL}.requestAccounts`);
        if (!Array.isArray(accounts) || typeof accounts[0] !== 'string') throw new WalletError('other', `${LABEL} returned no account.`);
        let network = normalizeNetwork(await p.getNetwork());
        if (network !== want) {
          for (const name of switchNames(want)) {
            try {
              await withTimeout(p.switchNetwork(name), CONNECT_TIMEOUT_MS, `${LABEL}.switchNetwork`);
            } catch {
              continue; // try the next spelling; the result's `network` tells the caller whether it worked
            }
            network = normalizeNetwork(await p.getNetwork());
            if (network === want) break;
          }
        }
        const pubkey = normalizePubkey(await p.getPublicKey());
        let version = '?';
        try {
          version = String(await p.getVersion?.() ?? '?');
        } catch {
          /* optional */
        }
        return { id: 'kasware', label: LABEL, address: accounts[0], pubkey, network, version };
      } catch (e) {
        throw toWalletError(e, LABEL);
      }
    },

    async refresh(): Promise<WalletInfo> {
      const p = need();
      try {
        // getAccounts is silent; requestAccounts on an already connected KasWare is too, and is only the fallback for a build without it
        const accounts = await withTimeout(p.getAccounts ? p.getAccounts() : p.requestAccounts(), REFRESH_TIMEOUT_MS, `${LABEL}.getAccounts`);
        if (!Array.isArray(accounts) || typeof accounts[0] !== 'string') throw new WalletError('other', `${LABEL} no longer exposes an account.`);
        const network = normalizeNetwork(await withTimeout(p.getNetwork(), REFRESH_TIMEOUT_MS, `${LABEL}.getNetwork`));
        const pubkey = normalizePubkey(await withTimeout(p.getPublicKey(), REFRESH_TIMEOUT_MS, `${LABEL}.getPublicKey`));
        return { id: 'kasware', label: LABEL, address: accounts[0], pubkey, network, version: await version(p) };
      } catch (e) {
        throw toWalletError(e, LABEL);
      }
    },

    subscribe(onEvent, o = {}) {
      const p = provider();
      if (!p) return () => undefined;
      // KasWare emits `accountsChanged` (string[]; [] when the site is disconnected) and `networkChanged` (network name)
      if (hasEmitter(p)) return listenProvider(p, { accounts: ['accountsChanged'], network: ['networkChanged'] }, onEvent);
      return pollProvider(
        async () => {
          const accounts = p.getAccounts ? await p.getAccounts() : [];
          return { account: accounts[0] ?? '', network: normalizeNetwork(await p.getNetwork()) };
        },
        onEvent,
        o.pollMs ?? DEFAULT_POLL_MS,
      );
    },

    async signTx(built: BuiltTx, opts: SignOptions = {}): Promise<InputSignature[]> {
      const p = need();
      const signInputs = built.sign.map((s) => ({ index: s.inputIndex, sighashType: s.sighashType }));
      let response: unknown;
      try {
        response = await withTimeout(
          p.signPskt({ txJsonString: JSON.stringify(built.tx), options: { signInputs } }),
          opts.timeoutMs ?? SIGN_TIMEOUT_MS,
          `${LABEL}.signPskt`,
        );
      } catch (e) {
        throw toWalletError(e, LABEL);
      }
      return collectSignatures(built, response, LABEL);
    },
  };
}
