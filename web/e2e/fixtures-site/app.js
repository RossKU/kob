// Placeholder page for the e2e infrastructure tests. It plays the role of the app: reads window.__KOB_CONFIG__, publishes the official
// SDK as window.__kobKaspa when features.test is set (same rule as the real app), connects a wallet, and runs the signing split
// build (kob-wasm) -> wallet signs -> finalize (kob-wasm) -> submit to the node. The wallet handling mirrors tools/wallet-gate/web/wallets.mjs.
const cfg = window.__KOB_CONFIG__ ?? {};
const $ = (id) => document.querySelector(`[data-testid="${id}"]`);
const setStatus = (s) => ($('tx-status').textContent = s);

const boot = (async () => {
  const kaspa = await import('/sdk/kaspa.js');
  await kaspa.default({ module_or_path: '/sdk/kaspa_bg.wasm' });
  if (cfg.features?.test) window.__kobKaspa = kaspa;
  const kob = await import('/kob/kob_wasm.js');
  await kob.default({ module_or_path: '/kob/kob_wasm_bg.wasm' });
  return { kaspa, kob };
})();

const normPub = (p) => {
  const h = String(p).replace(/^0x/, '').toLowerCase();
  return h.length === 66 ? h.slice(2) : h;
};
const normNet = (n) => (n === 'kaspa_testnet_10' ? 'testnet-10' : n === 'kaspa_mainnet' ? 'mainnet' : n);

// extensions inject their provider late: poll like the real adapters do
async function waitFor(name, ms = 5000) {
  const t0 = Date.now();
  while (!window[name]) {
    if (Date.now() - t0 > ms) throw new Error(`${name} provider not found`);
    await new Promise((r) => setTimeout(r, 50));
  }
  return window[name];
}

const connectors = {
  async kasware() {
    const p = await waitFor('kasware');
    const [address] = await p.requestAccounts();
    return { address, pubkey: normPub(await p.getPublicKey()), network: normNet(await p.getNetwork()) };
  },
  async kaspire() {
    const p = await waitFor('kaspire');
    const [address] = await p.request({ method: 'requestAccounts' });
    return { address, pubkey: normPub(await p.request({ method: 'getPublicKey' })), network: normNet(await p.request({ method: 'getNetwork' })) };
  },
  async kastle() {
    const p = await waitFor('kastle');
    await p.connect();
    const acc = await p.getAccount();
    return { address: acc.address, pubkey: normPub(acc.publicKey), network: normNet(await p.request('kas:get_network')) };
  },
};

function pushes(hex) {
  const b = Uint8Array.from(hex.match(/../g) ?? [], (x) => parseInt(x, 16));
  const out = [];
  for (let i = 0; i < b.length; ) {
    const op = b[i++];
    let len = op;
    if (op === 0x4c) len = b[i++];
    else if (op === 0x4d) { len = b[i] | (b[i + 1] << 8); i += 2; }
    out.push(b.slice(i, i + len));
    i += len;
  }
  return out;
}
const hex = (u) => Array.from(u, (x) => x.toString(16).padStart(2, '0')).join('');
const sigFrom = (sigscript) => {
  const p = pushes(sigscript).find((x) => x.length === 65);
  if (!p) throw new Error('no signature in the wallet response');
  return hex(p);
};

const signers = {
  async kasware(built) {
    const out = await window.kasware.signPskt({ txJsonString: JSON.stringify(built.tx), options: { signInputs: built.sign.map((r) => ({ index: r.inputIndex, sighashType: r.sighashType })) } });
    return JSON.parse(out);
  },
  async kaspire(built) {
    const scripts = built.sign.filter((r) => r.redeemScript).map((r) => ({ inputIndex: r.inputIndex, scriptHex: r.redeemScript, signType: 1, signatureScript: { mode: 'wrap-signature' } }));
    const res = await window.kaspire.request({ method: 'signPskt', params: { psktTransactionJson: JSON.stringify(built.tx), submitTransaction: false, signInputs: built.sign.map((r) => ({ index: r.inputIndex, sighashType: r.sighashType })), scripts } });
    return JSON.parse(res.psktTransactionJson);
  },
  async kastle(built) {
    const scripts = built.sign.filter((r) => r.redeemScript).map((r) => ({ inputIndex: r.inputIndex, scriptHex: '', signType: 'All' }));
    return window.kastle.signTx(cfg.network, JSON.stringify(built.tx), scripts);
  },
};

let connected = null;

async function connect(id) {
  await boot;
  const info = await connectors[id]();
  connected = { id, ...info };
  $('wallet-address').textContent = info.address;
  $('wallet-network').textContent = info.network;
  return connected;
}

/** request: kob-wasm ActionRequest (JSON). Returns {txid, fee, signedInputs}. */
async function signAndSubmit(request) {
  const { kob } = await boot;
  if (!connected) throw new Error('no wallet connected');
  setStatus('building');
  const built = JSON.parse(kob.build(JSON.stringify(request)));
  setStatus('awaiting signature');
  const signedTx = await signers[connected.id](built);
  const signatures = built.sign.map((r) => ({ inputIndex: r.inputIndex, signature: sigFrom(signedTx.inputs[r.inputIndex].signatureScript) }));
  const signed = JSON.parse(kob.finalize(JSON.stringify(built), JSON.stringify(signatures), JSON.stringify({ tightenBudgets: true })));
  setStatus('submitting');
  const res = await fetch(`${cfg.nodeUrl}/node/submit`, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ transaction: signed.tx }) });
  const body = await res.json();
  if (!res.ok) {
    setStatus('rejected: ' + body.error);
    throw new Error(body.error);
  }
  setStatus('accepted');
  $('tx-id').textContent = body.transactionId;
  return { txid: body.transactionId, fee: signed.fee.fee, covenants: built.covenants };
}

for (const id of ['kasware', 'kaspire', 'kastle']) {
  const b = $(`wallet-connect-${id}`);
  b.disabled = false;
  b.addEventListener('click', () => connect(id).catch((e) => setStatus('wallet error: ' + (e.message ?? e))));
}
boot.then(() => setStatus('ready'), (e) => setStatus('boot failed: ' + (e.message ?? e)));

window.__infra = { ready: boot.then(() => true), connect, signAndSubmit, config: cfg };
