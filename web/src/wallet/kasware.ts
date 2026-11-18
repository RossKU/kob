// The KasWare adapter (`window.kasware`). tools/wallet-gate/RESULTS.md holds the proof on TN10 with
// KasWare 0.10.0.
//
// - `getNetwork()` reports `kaspa_testnet_10`, which is normalised here.
// - `signPskt({ txJsonString, options: { signInputs: [{ index, sighashType: 1 }] } })` returns the
//   signed transaction JSON, as a string or an object. For a P2SH (covenant) input it puts only
//   `push(sig65)`, 66 bytes, in the signature script, and the dApp assembles the rest. Nothing is
//   assembled here. Only the signature is taken, and kob-wasm `finalize` builds the script and
//   checks the signature.
// - The popup is a generic "Sign Transaction: Spend". It cannot show token semantics, so the app's
//   own confirmation screen does.
// - In 0.11.5, read from its service worker, `signPskt` off mainnet refuses an input its own node
//   does not list as unspent in its UTXO set or mempool. `pushTx(txJson)` submits a signed
//   transaction to that node without a popup. `announce` uses it for a transaction KOB already
//   sent (wallet/visibility.ts).
// - When 0.11.5 refuses an input, it shows a "Critical Signing Warning" and only Reject. Reject
//   answers like any user rejection (`userRejectedRequest`, code 4001), so the app cannot tell the
//   two apart. A declined step that spends a fresh output says both (wallet/sign.ts).
// - In 0.11.5, `getUtxoEntries` answers for the wallet's own account from its UTXO tracker, which
//   lists a new output only once it is mature, 11 to 15 s on TN10. That is too slow to gate on.
import type { BuiltTx, InputSignature, TxJson } from '../kob/types';
import { normalizeNetwork } from '../data/kaspa-sdk';
import { WalletError, type SignOptions, type WalletAdapter, type WalletInfo } from './types';
import { CONNECT_TIMEOUT_MS, REFRESH_TIMEOUT_MS, SIGN_TIMEOUT_MS, collectSignatures, errText, normalizePubkey, toWalletError, withTimeout } from './sigs';
import { DEFAULT_POLL_MS, hasEmitter, listenProvider, pollProvider, type EmitterLike } from './events';

/** The part of `window.kasware` the adapter uses. */
export interface KaswareProvider extends EmitterLike {
  requestAccounts(): Promise<string[]>;
  /** Silently gives the accounts the site is already connected to, or `[]` when not connected. */
  getAccounts?(): Promise<string[]>;
  getNetwork(): Promise<string>;
  switchNetwork(network: string): Promise<unknown>;
  getPublicKey(): Promise<string>;
  getVersion?(): Promise<string>;
  signPskt(request: { txJsonString: string; options: { signInputs: { index: number; sighashType: number }[] } }): Promise<unknown>;
  /**
   * In 0.11.5, submits a signed transaction, as a safe JSON string, to the wallet's own node
   * without a popup. It resolves with its id.
   */
  pushTx?(rawtx: string): Promise<unknown>;
}

/** What the KasWare adapter takes from its caller. */
export interface KaswareDeps {
  /** Read at call time, because extensions inject late. The default is `window.kasware`. */
  getProvider?: () => KaswareProvider | null | undefined;
}

const LABEL = 'KasWare';
/**
 * `pushTx` answers at once with no popup. It either reaches the wallet's node quickly or not at
 * all.
 */
const ANNOUNCE_TIMEOUT_MS = 3000;
/** The answers of a node that already has the transaction. */
const ALREADY = /already (?:in|exists in) (?:the )?mempool|already accepted|already (?:have|known)|duplicate transaction/i;

/**
 * The network names to try, KasWare's own first (`kaspa_testnet_10`) and then the plain SDK names.
 * The wallet accepted `testnet-10` on TN10 too.
 */
const switchNames = (net: string): string[] => (net === 'mainnet' ? ['kaspa_mainnet', 'mainnet'] : net === 'testnet-10' ? ['kaspa_testnet_10', 'testnet-10'] : [net]);

async function version(p: KaswareProvider): Promise<string> {
  try {
    return String((await p.getVersion?.()) ?? '?');
  } catch {
    return '?';
  }
}

/** Creates the KasWare adapter. */
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
    // It signs covenant (P2SH) inputs, proven on TN10 (tools/wallet-gate), so orders placed with it
    // can be cancelled.
    covenantSigning: 'proven',
    detect: () => !!provider(),

    async connect(wantNetwork: string): Promise<WalletInfo> {
      const p = need();
      const want = normalizeNetwork(wantNetwork);
      try {
        let accounts = await withTimeout(p.requestAccounts(), CONNECT_TIMEOUT_MS, `${LABEL}.requestAccounts`);
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
          // The address prefix changes with the network, so the account read before the switch is
          // the other network's address.
          if (network === want) {
            const again = await withTimeout(p.getAccounts ? p.getAccounts() : p.requestAccounts(), CONNECT_TIMEOUT_MS, `${LABEL}.getAccounts`);
            if (Array.isArray(again) && typeof again[0] === 'string') accounts = again;
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
        // getAccounts is silent. requestAccounts on an already connected KasWare is silent too, and
        // is only the fallback for a build without getAccounts.
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
      // KasWare emits `accountsChanged` with a string[], which is [] when the site is
      // disconnected. It emits `networkChanged` with the network name.
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

    async announce(tx: TxJson, opts: { timeoutMs?: number } = {}): Promise<void> {
      const p = need();
      if (typeof p.pushTx !== 'function') throw new WalletError('unsupported', `${LABEL} has no pushTx.`);
      try {
        await withTimeout(p.pushTx(JSON.stringify(tx)), opts.timeoutMs ?? ANNOUNCE_TIMEOUT_MS, `${LABEL}.pushTx`);
      } catch (e) {
        // The wallet's node already holds it, from the network or an earlier push. That is what was
        // wanted.
        if (ALREADY.test(errText(e))) return;
        throw toWalletError(e, LABEL);
      }
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
