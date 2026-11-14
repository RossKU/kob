// Kaspire adapter (`window.kaspire.request({method, params})`, HUB21). It was exercised on TN10
// with Kaspire 0.5.1 (provider 1.2.0).
// - `signPskt` takes `{ psktTransactionJson, submitTransaction: false, signInputs, scripts }`. For
//   covenant (P2SH) inputs it needs a per-input `scripts` entry
//   `{ inputIndex, scriptHex: <redeem>, signType, signatureScript }`, else it returns a bare
//   signature.
// - `signatureScript.mode` is `ordered-args` or `wrap-signature`. With `ordered-args` the wallet
//   emits `args.. dispatch-tag redeem` itself, byte-identical to ours, which independently
//   validates the ABI encoding. With `wrap-signature` it emits `<sig><redeem>`.
// - The address prefix changes with the network, so accounts are requested again after a network
//   switch.
// Either way the app only extracts the signature and assembles the script itself through kob-wasm
// `finalize`, which verifies it. A wallet that mis-assembles cannot produce a wrong transaction.
//
// Read from the 0.5.31 background script: `signPskt` looks no input up on any node, so the app's
// visibility gate (wallet/visibility.ts) is only a precaution here. Its `pushTx` and
// `getUtxoEntries` are not used. `pushTx` posts the string it is given as is to the REST API's
// `/transactions`, which takes the REST shape of a transaction, not this app's. `getUtxoEntries`
// lists only the selected account, whatever address it is asked about, from that REST API: it
// cannot answer for covenant inputs, and it says nothing about a sign check that does not exist.
import type { BuiltTx, Hex, InputSignature, SignRequest, TemplateName } from '../kob/types';
import type { KobWasm } from '../kob/wasm';
import { normalizeNetwork } from '../data/kaspa-sdk';
import { WalletError, type SignOptions, type WalletAdapter, type WalletInfo } from './types';
import { CONNECT_TIMEOUT_MS, REFRESH_TIMEOUT_MS, SIGN_TIMEOUT_MS, collectSignatures, normalizePubkey, toWalletError, withTimeout } from './sigs';
import { DEFAULT_POLL_MS, hasEmitter, listenProvider, pollProvider, type EmitterLike } from './events';

/** The Kaspire provider object, as the extension injects it. */
export interface KaspireProvider extends EmitterLike {
  request(args: { method: string; params?: unknown }): Promise<unknown>;
  version?: string;
}

/** How a covenant input's signature script is assembled by the wallet. */
export type KaspireSignatureScript =
  | { mode: 'wrap-signature' }
  | { mode: 'ordered-args'; args: ({ type: 'signature'; prefixHex: string } | { type: 'data'; hex: string })[] };

/** The per-input `scripts` entry of `signPskt` for a covenant input. */
export interface KaspireScriptEntry {
  inputIndex: number;
  scriptHex: Hex;
  signType: number;
  signatureScript: KaspireSignatureScript;
}

/** What the Kaspire adapter needs. */
export interface KaspireDeps {
  getProvider?: () => KaspireProvider | null | undefined;
  /**
   * The dispatch tag (hex) of a template entry. It enables `ordered-args` for entries whose only
   * argument is the signature.
   */
  dispatchTag?: (template: TemplateName, entry: string) => Hex | null;
  /**
   * Forces one mode for every covenant input. Default `auto`: `ordered-args` where derivable, else
   * `wrap-signature`.
   */
  mode?: 'auto' | 'wrap-signature';
}

const LABEL = 'Kaspire';

/**
 * The dispatch tags out of kob-wasm's template table (`TemplateInfo.entries`: entry name to 4-byte
 * tag hex).
 */
export function dispatchTagFrom(kob: KobWasm): (template: TemplateName, entry: string) => Hex | null {
  let table: Map<string, Record<string, string>> | null = null;
  return (template, entry) => {
    table ??= new Map(kob.templates().map((t) => [t.name, t.entries]));
    return table.get(template)?.[entry] ?? null;
  };
}

/**
 * The signature script mode for one input. `ordered-args` mirrors the plan only when that is
 * trivial and certain: a template entry whose sole argument is the signature (`cancel`), which
 * gives `[signature, dispatch tag]`. Everything else would need the ABI encoder, which lives in
 * Rust, and uses `wrap-signature`. That covers int and bytes arguments and KCC-20 leader
 * witnesses with field-wise state arrays.
 */
export function kaspireSignatureScript(built: BuiltTx, req: SignRequest, deps: Pick<KaspireDeps, 'dispatchTag' | 'mode'> = {}): KaspireSignatureScript {
  const plan = built.plans[req.inputIndex];
  if (deps.mode !== 'wrap-signature' && plan?.kind === 'entry' && plan.args.length === 1 && plan.args[0]!.kind === 'sig' && deps.dispatchTag) {
    const tag = deps.dispatchTag(plan.template, plan.entry);
    if (tag) return { mode: 'ordered-args', args: [{ type: 'signature', prefixHex: '' }, { type: 'data', hex: tag }] };
  }
  // The maker's cancel of an order under a retired template is the same `cancel(sig)`. kob-protocol
  // checks that its ABI and tag are today's.
  if (deps.mode !== 'wrap-signature' && plan?.kind === 'retiredCancel' && deps.dispatchTag) {
    const tag = deps.dispatchTag(plan.template, 'cancel');
    if (tag) return { mode: 'ordered-args', args: [{ type: 'signature', prefixHex: '' }, { type: 'data', hex: tag }] };
  }
  return { mode: 'wrap-signature' };
}

/** The adapter for Kaspire. It reads the provider at call time. */
export function createKaspireAdapter(deps: KaspireDeps = {}): WalletAdapter {
  const provider = () => (deps.getProvider ? deps.getProvider() : (globalThis as { kaspire?: KaspireProvider }).kaspire) ?? null;
  const need = (): KaspireProvider => {
    const p = provider();
    if (!p || typeof p.request !== 'function') throw new WalletError('unsupported', 'Kaspire is not installed in this browser.');
    return p;
  };
  return {
    id: 'kaspire',
    label: LABEL,
    // Signs covenant (P2SH) inputs, proven on TN10 (tools/wallet-gate).
    covenantSigning: 'proven',
    detect: () => {
      const p = provider();
      return !!p && typeof p.request === 'function';
    },

    async connect(wantNetwork: string): Promise<WalletInfo> {
      const p = need();
      const want = normalizeNetwork(wantNetwork);
      const call = <T>(method: string, params?: unknown) => withTimeout(p.request({ method, params }) as Promise<T>, CONNECT_TIMEOUT_MS, `${LABEL}.${method}`);
      try {
        let accounts = await call<string[]>('requestAccounts');
        let network = normalizeNetwork(await call<string>('getNetwork'));
        if (network !== want) {
          try {
            await call('switchNetwork', { network: want });
          } catch {
            /* reported through the result's `network` */
          }
          network = normalizeNetwork(await call<string>('getNetwork'));
          if (network === want) accounts = await call<string[]>('requestAccounts'); // the address prefix changes with the network
        }
        if (!Array.isArray(accounts) || typeof accounts[0] !== 'string') throw new WalletError('other', `${LABEL} returned no account.`);
        const pubkey = normalizePubkey(await call<string>('getPublicKey'));
        return { id: 'kaspire', label: LABEL, address: accounts[0], pubkey, network, version: String(p.version ?? '?') };
      } catch (e) {
        throw toWalletError(e, LABEL);
      }
    },

    async refresh(): Promise<WalletInfo> {
      const p = need();
      const call = <T>(method: string) => withTimeout(p.request({ method }) as Promise<T>, REFRESH_TIMEOUT_MS, `${LABEL}.${method}`);
      try {
        // getAccounts is silent. requestAccounts would also fire `accountsChanged` back at us.
        const accounts = await call<string[]>('getAccounts');
        if (!Array.isArray(accounts) || typeof accounts[0] !== 'string') throw new WalletError('other', `${LABEL} no longer exposes an account.`);
        const network = normalizeNetwork(await call<string>('getNetwork'));
        const pubkey = normalizePubkey(await call<string>('getPublicKey'));
        return { id: 'kaspire', label: LABEL, address: accounts[0], pubkey, network, version: String(p.version ?? '?') };
      } catch (e) {
        throw toWalletError(e, LABEL);
      }
    },

    subscribe(onEvent, o = {}) {
      const p = provider();
      if (!p || typeof p.request !== 'function') return () => undefined;
      // Kaspire (provider 1.2.0) emits accountsChanged(string[]), networkChanged(name) and
      // disconnect({code: 4900}). It also emits them for its own requestAccounts and switchNetwork
      // calls, which is why the app compares every re-read with what it already has.
      if (hasEmitter(p)) return listenProvider(p, { accounts: ['accountsChanged'], network: ['networkChanged'], disconnect: ['disconnect'] }, onEvent);
      return pollProvider(
        async () => {
          const accounts = (await p.request({ method: 'getAccounts' })) as string[];
          return { account: accounts?.[0] ?? '', network: normalizeNetwork(await p.request({ method: 'getNetwork' })) };
        },
        onEvent,
        o.pollMs ?? DEFAULT_POLL_MS,
      );
    },

    async signTx(built: BuiltTx, opts: SignOptions = {}): Promise<InputSignature[]> {
      const p = need();
      const signInputs = built.sign.map((s) => ({ index: s.inputIndex, sighashType: s.sighashType }));
      const scripts: KaspireScriptEntry[] = built.sign
        .filter((s) => s.redeemScript !== null)
        .map((s) => ({ inputIndex: s.inputIndex, scriptHex: s.redeemScript as Hex, signType: s.sighashType, signatureScript: kaspireSignatureScript(built, s, deps) }));
      const params = {
        psktTransactionJson: JSON.stringify(built.tx),
        submitTransaction: false, // the app broadcasts through its own node connection
        signInputs,
        ...(scripts.length ? { scripts } : {}),
      };
      let response: unknown;
      try {
        response = await withTimeout(p.request({ method: 'signPskt', params }), opts.timeoutMs ?? SIGN_TIMEOUT_MS, `${LABEL}.signPskt`);
      } catch (e) {
        throw toWalletError(e, LABEL);
      }
      const signed = (response as { psktTransactionJson?: unknown } | null)?.psktTransactionJson ?? response;
      return collectSignatures(built, signed, LABEL);
    },
  };
}
