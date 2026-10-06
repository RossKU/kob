// Wallet adapters for the gate page. Each adapter exposes:
//   id, label, detect() -> bool, connect() -> { address, pubkey (x-only hex), network, version, raw }
//   signInput0(test, opts) -> { signedTxJson?, walletSigscript?, request, response }   (asks the wallet to sign ONLY input 0)
// Signing never broadcasts; the page broadcasts through the node RPC.

const normPub = (p) => {
  if (typeof p !== 'string') throw new Error('wallet returned no public key');
  const h = p.replace(/^0x/, '').toLowerCase();
  if (h.length === 64) return h;
  if (h.length === 66) return h.slice(2); // compressed -> x-only
  throw new Error('unexpected public key format: ' + p);
};
const errText = (e) => (e && typeof e === 'object' ? [e.code != null ? `code ${e.code}` : '', e.message ?? String(e)].filter(Boolean).join(': ') : String(e));
export { errText };

function withTimeout(p, ms, what) {
  let t;
  return Promise.race([p, new Promise((_, rej) => { t = setTimeout(() => rej(new Error(`${what}: no response after ${ms / 1000}s (popup not answered?)`)), ms); })]).finally(() => clearTimeout(t));
}
export const SIGN_TIMEOUT_MS = 180000;

// ---------------------------------------------------------------- KasWare
export const kasware = {
  id: 'kasware', label: 'KasWare',
  detect: () => !!window.kasware,
  async connect() {
    const p = window.kasware;
    const accounts = await p.requestAccounts();
    // KasWare 0.10.0 reports TN10 as 'kaspa_testnet_10' (verified live), not 'testnet-10'
    const normNet = (n) => (n === 'kaspa_testnet_10' ? 'testnet-10' : n);
    let network = normNet(await p.getNetwork());
    if (network !== 'testnet-10') { try { await p.switchNetwork('testnet-10'); } catch (e) { /* reported below */ } network = normNet(await p.getNetwork()); }
    const pub = await p.getPublicKey();
    let version = '?';
    try { version = await p.getVersion(); } catch { /* optional */ }
    return { address: accounts[0], pubkey: normPub(pub), network, version, raw: { accounts, pub } };
  },
  async signInput0(test, opts = {}) {
    const sighashType = opts.sighashType ?? 1;
    const request = { txJsonString: test.txJson(), options: { signInputs: [{ index: 0, sighashType }] } };
    const response = await withTimeout(window.kasware.signPskt(request), SIGN_TIMEOUT_MS, 'kasware.signPskt');
    return { request: { method: 'signPskt', sighashType }, signedTxJson: response, response };
  },
};

// ---------------------------------------------------------------- Kaspire
export const kaspire = {
  id: 'kaspire', label: 'Kaspire',
  detect: () => !!(window.kaspire && window.kaspire.request),
  async connect() {
    const p = window.kaspire;
    let accounts = await p.request({ method: 'requestAccounts' });
    let network = await p.request({ method: 'getNetwork' });
    if (network !== 'testnet-10') {
      try { await p.request({ method: 'switchNetwork', params: { network: 'testnet-10' } }); } catch { /* reported below */ }
      network = await p.request({ method: 'getNetwork' });
      if (network === 'testnet-10') accounts = await p.request({ method: 'requestAccounts' }); // address prefix changes with the network
    }
    const pub = await p.request({ method: 'getPublicKey' });
    return { address: accounts[0], pubkey: normPub(pub), network, version: String(p.version ?? '?'), raw: { accounts, pub } };
  },
  async signInput0(test, opts = {}) {
    const sighashType = opts.sighashType ?? 1;
    const params = { psktTransactionJson: test.txJson(), submitTransaction: false, signInputs: [{ index: 0, sighashType }] };
    // opts.kaspireMode: 'ordered-args' (default for covenant tests) | 'wrap-signature' | 'none'
    const mode = opts.kaspireMode ?? (test.kaspire ? 'ordered-args' : 'none');
    if (test.kaspire && mode !== 'none') {
      const sc = mode === 'ordered-args' ? test.kaspire.signatureScript : { mode: 'wrap-signature' };
      params.scripts = [{ inputIndex: 0, scriptHex: test.kaspire.scriptHex, signType: sighashType, signatureScript: sc }];
    }
    const response = await withTimeout(window.kaspire.request({ method: 'signPskt', params }), SIGN_TIMEOUT_MS, 'kaspire.signPskt');
    return { request: { method: 'signPskt', sighashType, mode, hasScripts: !!params.scripts }, signedTxJson: response.psktTransactionJson ?? response, response };
  },
};

// ---------------------------------------------------------------- Kastle
export const kastle = {
  id: 'kastle', label: 'Kastle',
  detect: () => !!window.kastle,
  async connect() {
    const p = window.kastle;
    const ok = await p.connect();
    if (!ok) throw new Error('kastle.connect() returned false');
    let network = await p.request('kas:get_network');
    if (network !== 'testnet-10') { try { await p.request('kas:switch_network', 'testnet-10'); } catch { /* reported below */ } network = await p.request('kas:get_network'); }
    const acc = await p.getAccount();
    let version = '?';
    try { version = await p.request('kas:get_version'); } catch { /* optional */ }
    return { address: acc.address, pubkey: normPub(acc.publicKey), network, version: String(version), raw: acc };
  },
  // opts.kastleVariant: 'empty-script' (scriptHex "": documented workaround, returns only the signature push)
  //                     'redeem-script' (scriptHex = redeem: wallet emits <sig><push redeem>, we still take the signature)
  //                     'plain' (no scripts: expected to leave P2SH inputs unsigned - issue #353)
  async signInput0(test, opts = {}) {
    const variant = opts.kastleVariant ?? (test.kind === 'p2pk' ? 'plain' : 'empty-script');
    let scripts;
    if (variant === 'empty-script') scripts = [{ inputIndex: 0, scriptHex: '', signType: 'All' }];
    if (variant === 'redeem-script') scripts = [{ inputIndex: 0, scriptHex: test.kaspire ? test.kaspire.scriptHex : '', signType: 'All' }];
    const network = 'testnet-10';
    const response = await withTimeout(scripts ? window.kastle.signTx(network, test.txJson(), scripts) : window.kastle.signTx(network, test.txJson()), SIGN_TIMEOUT_MS, 'kastle.signTx');
    return { request: { method: 'signTx', variant, scripts }, signedTxJson: response, response };
  },
};

// ---------------------------------------------------------------- KIP-12 announced providers (draft)
export const kip12 = {
  providers: [],
  discover() {
    window.addEventListener('kaspa:provider', (ev) => {
      const d = ev.detail;
      if (d && !kip12.providers.some((p) => p.info?.uuid === d.info?.uuid && p.info?.name === d.info?.name)) kip12.providers.push(d);
    });
    window.dispatchEvent(new Event('kaspa:requestProvider'));
  },
};

export const ADAPTERS = { kasware, kaspire, kastle };
