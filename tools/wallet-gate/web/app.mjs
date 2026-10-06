// KOB wallet gate page: connect a browser wallet, run T1/T2/T3 on testnet-10, record results.
import init, * as k from '/vendor/kaspa-web/kaspa.js';
import * as S from '/lib/script.mjs';
import * as C from '/lib/contracts.mjs';
import * as T from '/lib/txbuild.mjs';
import * as F from '/lib/flows.mjs';
import { ADAPTERS, kip12, errText } from './wallets.mjs';

const $ = (id) => document.getElementById(id);
const G = { k, cfg: null, rpc: null, adapter: null, wallet: null, setup: null, ctx: null, records: [], art: null };
window.__gate = G;

const log = (msg, cls = '') => {
  const el = $('log');
  const line = document.createElement('div');
  if (cls) line.className = cls;
  line.textContent = `${new Date().toISOString().slice(11, 19)}  ${msg}`;
  el.prepend(line);
};
const jsonBig = (v) => JSON.stringify(v, (_, x) => (typeof x === 'bigint' ? x.toString() : x), 2);

// ------------------------------------------------------------------ boot
async function boot() {
  await init('/vendor/kaspa-web/kaspa_bg.wasm');
  G.cfg = await (await fetch('/api/config')).json();
  const [kcc, bid] = await Promise.all(['KCC20Ref.json', 'BidOrder.template.json'].map(async (f) => (await fetch('/artifacts/' + f)).json()));
  G.art = { kcc, bid };
  $('node').textContent = G.cfg.nodeWs || '(public resolver)';
  try {
    G.rpc = await T.connectRpc(k, G.cfg.nodeWs, G.cfg.network);
    const dag = await G.rpc.getBlockDagInfo();
    $('nodeinfo').textContent = `connected, ${dag.network}, virtual DAA ${dag.virtualDaaScore}`;
    log('node connected: ' + $('nodeinfo').textContent, 'ok');
  } catch (e) {
    $('nodeinfo').textContent = 'NOT CONNECTED: ' + errText(e);
    log('node connect failed: ' + errText(e), 'bad');
  }
  kip12.discover();
  renderWallets();
  setTimeout(renderWallets, 1500); // extensions inject late
  window.addEventListener('kaspire#initialized', renderWallets);
  try { G.records = JSON.parse(localStorage.getItem('kob-wallet-gate-records') || '[]'); } catch { G.records = []; }
  renderRecords();
  $('boot').textContent = 'ready';
  window.__gateReady = true;
}

function renderWallets() {
  const box = $('wallets');
  box.innerHTML = '';
  for (const [id, a] of Object.entries(ADAPTERS)) {
    const b = document.createElement('button');
    b.textContent = `${a.detect() ? 'Connect' : 'not detected:'} ${a.label}`;
    b.disabled = !a.detect();
    b.id = 'connect-' + id;
    b.onclick = () => connectWallet(id).catch(() => {});
    box.append(b);
  }
  if (kip12.providers.length) {
    const s = document.createElement('div');
    s.className = 'muted';
    s.textContent = 'KIP-12 announced providers: ' + kip12.providers.map((p) => p.info?.name ?? '?').join(', ');
    box.append(s);
  }
}

// ------------------------------------------------------------------ wallet + setup
async function connectWallet(id) {
  const a = ADAPTERS[id];
  log(`connecting ${a.label} ...`);
  try {
    const w = await a.connect();
    G.adapter = a;
    G.wallet = w;
    $('walletinfo').textContent = `${a.label} ${w.version} | network ${w.network} | ${w.address} | pubkey ${w.pubkey}`;
    log(`${a.label} connected: ${w.address}`, 'ok');
    if (w.network !== 'testnet-10') log(`WARNING: wallet network is ${w.network}, expected testnet-10`, 'bad');
    const addr = C.addressOfPubkey(k, w.pubkey);
    if (addr !== w.address) log(`note: address from pubkey (${addr}) differs from wallet address (${w.address})`, 'bad');
    await loadSetup(w.pubkey);
    return w;
  } catch (e) {
    log(`${a.label} connect failed: ${errText(e)}`, 'bad');
    $('walletinfo').textContent = `${a.label}: connect failed: ${errText(e)}`;
    throw e;
  }
}

async function loadSetup(pubkey, text) {
  try {
    const setup = text ? JSON.parse(text) : await (await fetch(`/state/setup-${pubkey}.json`)).json();
    if (setup.walletPubkey !== pubkey) throw new Error('setup file is for another pubkey: ' + setup.walletPubkey);
    G.setup = setup;
    G.ctx = F.deriveContext(k, setup, G.art.kcc, G.art.bid);
    $('setupinfo').textContent = `token cov id ${setup.tokenCovId.slice(0, 16)}...  tokens ${setup.tokens.length}, bids ${setup.bids.length}, funds ${setup.funds.length}; derived addresses match: token=${G.ctx.checks.tokenAddressMatches} bid=${G.ctx.checks.bidAddressMatches}`;
    log('setup loaded for ' + pubkey.slice(0, 12), G.ctx.checks.tokenAddressMatches && G.ctx.checks.bidAddressMatches ? 'ok' : 'bad');
  } catch (e) {
    G.setup = G.ctx = null;
    $('setupinfo').textContent = `no setup for this wallet key yet (${errText(e)}). Run: node scripts/setup.mjs --wallet-address ${G.wallet?.address ?? '<address>'}`;
    log('setup not loaded: ' + errText(e), 'bad');
  }
}

// ------------------------------------------------------------------ tests
async function liveEntry(address, txid, index) {
  const es = await T.utxosOf(G.rpc, address);
  return es.find((e) => (e.entry ?? e).outpoint.transactionId === txid && (e.entry ?? e).outpoint.index === index);
}
async function pickUtxo(id, skip = 0) {
  const s = G.setup;
  const list = id === 'T1' ? s.funds.map((f) => ({ ...f, address: s.walletAddress })) : id === 'T2' ? s.bids : s.tokens;
  let seen = 0;
  for (const it of list) {
    const e = await liveEntry(it.address, it.txid, it.index);
    if (e) { if (seen++ === skip) return { entry: e, item: it }; }
  }
  return null;
}

/** Runs one test end-to-end. opts: { skip, sighashType, kastleVariant, kaspireMode, popup, displayed, note, useWalletTx, recipient } */
async function runTest(id, opts = {}) {
  if (!G.adapter || !G.wallet) throw new Error('connect a wallet first');
  if (!G.ctx) throw new Error('no setup loaded for this wallet key');
  const rec = {
    at: new Date().toISOString(), test: id, wallet: G.adapter.id, walletVersion: G.wallet.version, walletNetwork: G.wallet.network,
    walletAddress: G.wallet.address, walletPubkey: G.wallet.pubkey, options: { ...opts },
    popupAppeared: opts.popup ?? 'unknown', popupDisplayed: opts.displayed ?? '', note: opts.note ?? '',
  };
  const t0 = performance.now();
  try {
    const picked = await pickUtxo(id, opts.skip ?? 0);
    if (!picked) throw new Error(`no unspent ${id} UTXO left in the setup file: run setup again`);
    rec.utxo = `${picked.item.txid}:${picked.item.index}`;
    const recipient = opts.recipient || G.setup.recipientPubkey || (await recipientDefault());
    const test = id === 'T1' ? F.buildT1(k, G.ctx, picked.entry) : id === 'T2' ? F.buildT2(k, G.ctx, picked.item, picked.entry) : F.buildT3(k, G.ctx, picked.entry, recipient);
    rec.plan = { inputs: test.plan.inputs.length, outputs: test.plan.outputs.length, fee: test.fee.toString(), computeBudget: test.plan.inputs[0].budget, inputCovenantId: test.plan.inputs[0].covenantId ?? null };
    rec.unsignedTxJson = JSON.parse(test.txJson());
    log(`${id}: built unsigned tx v1 (utxo ${rec.utxo}), asking ${G.adapter.label} to sign input 0 only ...`);

    // ---- ask the wallet
    const t1 = performance.now();
    let signed;
    try {
      signed = await G.adapter.signInput0(test, opts);
    } catch (e) {
      rec.walletError = errText(e);
      rec.walletMs = Math.round(performance.now() - t1);
      throw new Error('wallet signing failed: ' + rec.walletError);
    }
    rec.walletMs = Math.round(performance.now() - t1);
    rec.walletRequest = signed.request;

    // ---- extract signature
    let ex;
    try {
      ex = F.sigFromSignedTx(signed.signedTxJson, 0);
    } catch (e) {
      rec.signedTxJson = safeParse(signed.signedTxJson);
      rec.signatureReturned = false;
      throw new Error('no signature in wallet response: ' + errText(e));
    }
    rec.signatureReturned = true;
    rec.signatureHex = S.hex(ex.sig65);
    rec.sighashByte = ex.sig65[64];
    rec.walletSigscriptHex = ex.walletSigscript;
    rec.walletSigscriptBytes = ex.walletSigscript.length / 2;
    rec.walletSigscriptPushLens = S.parsePushes(S.unhex(ex.walletSigscript)).map((p) => p.length);
    const returned = safeParse(signed.signedTxJson);
    rec.walletKeptFields = returned && {
      version: returned.version === 1,
      computeBudget: returned.inputs?.[0]?.computeBudget === test.plan.inputs[0].budget,
      outputsIdentical: stable(returned.outputs) === stable(rec.unsignedTxJson.outputs),
    };

    // ---- assemble OUR sigscript and compare with what the wallet itself emitted
    const final = F.finalize(test, ex.sig65);
    rec.assembledSigscriptBytes = final.sigscript.length / 2;
    rec.walletSigscriptEqualsOurs = ex.walletSigscript === final.sigscript;
    let txToSend = final.tx;
    if (opts.useWalletTx) {
      txToSend = k.Transaction.deserializeFromSafeJSON(typeof signed.signedTxJson === 'string' ? signed.signedTxJson : JSON.stringify(signed.signedTxJson));
      txToSend.inputs[0].signatureScript = final.sigscript;
      rec.broadcastUsed = 'wallet-returned tx with our sigscript';
    } else rec.broadcastUsed = 'dapp tx with our sigscript';
    log(`${id}: signature received (${rec.signatureHex.slice(0, 16)}..., sighash ${rec.sighashByte}); assembled sigscript ${rec.assembledSigscriptBytes} B; wallet's own sigscript ${rec.walletSigscriptBytes} B (equal: ${rec.walletSigscriptEqualsOurs})`);

    // ---- broadcast + acceptance
    if (opts.broadcast === false) { rec.broadcast = 'skipped'; }
    else {
      try {
        rec.txid = await T.submit(G.rpc, txToSend);
        log(`${id}: submitted ${rec.txid}`, 'ok');
      } catch (e) {
        rec.rejected = errText(e);
        throw new Error('node rejected the tx: ' + rec.rejected);
      }
      const acc = await T.waitAccepted(G.rpc, test.outAddress, rec.txid, test.outIndex, 90000);
      rec.accepted = acc.accepted;
      rec.acceptedAfterMs = acc.ms;
      log(`${id}: ${acc.accepted ? 'ACCEPTED (output visible in UTXO set)' : 'not seen in UTXO set within 90s'} ${rec.txid}`, acc.accepted ? 'ok' : 'bad');
    }
    rec.result = rec.accepted ? 'PASS' : rec.broadcast === 'skipped' ? 'SIGNED (not broadcast)' : 'FAIL';
  } catch (e) {
    rec.error = errText(e);
    rec.result = 'FAIL';
    log(`${id}: FAIL - ${rec.error}`, 'bad');
  }
  rec.totalMs = Math.round(performance.now() - t0);
  G.records.push(rec);
  persist();
  renderRecords();
  try { fetch('/api/result', { method: 'POST', headers: { 'content-type': 'application/json' }, body: jsonBig(rec) }); } catch { /* optional */ }
  return rec;
}

async function recipientDefault() {
  // "new owner" for T3 = the dev pubkey when the server exposes it, else a fixed dummy x-only key
  try { const c = await (await fetch('/api/config')).json(); if (c.recipientPubkey) return c.recipientPubkey; } catch { /* ignore */ }
  return '5b'.repeat(32);
}
// key-order independent JSON (wallets may re-serialise with sorted keys)
const stable = (v) => JSON.stringify(v, (_, x) => (x && typeof x === 'object' && !Array.isArray(x) ? Object.fromEntries(Object.entries(x).sort(([p], [q]) => (p < q ? -1 : 1))) : x));
const safeParse = (x) => { try { return typeof x === 'string' ? JSON.parse(x) : x; } catch { return null; } };

// ------------------------------------------------------------------ results UI
function persist() { try { localStorage.setItem('kob-wallet-gate-records', JSON.stringify(G.records)); } catch { /* ignore */ } }
function renderRecords() {
  const tb = $('records');
  tb.innerHTML = '';
  for (const r of G.records) {
    const tr = document.createElement('tr');
    const cells = [r.at.slice(11, 19), r.wallet + ' ' + (r.walletVersion ?? ''), r.test + (r.options?.kastleVariant ? ' [' + r.options.kastleVariant + ']' : '') , r.popupAppeared, r.signatureReturned ? 'yes' : 'no', r.walletSigscriptEqualsOurs ?? '', r.accepted ? 'yes' : 'no', r.txid ? r.txid.slice(0, 16) + '...' : '', r.error ?? ''];
    for (const c of cells) { const td = document.createElement('td'); td.textContent = String(c); tr.append(td); }
    tr.className = r.result === 'PASS' ? 'ok' : r.result === 'FAIL' ? 'bad' : '';
    tb.append(tr);
  }
}
function download() {
  const blob = new Blob([jsonBig({ generatedAt: new Date().toISOString(), node: G.cfg?.nodeWs, records: G.records })], { type: 'application/json' });
  const a = document.createElement('a');
  a.href = URL.createObjectURL(blob);
  a.download = `wallet-gate-results-${Date.now()}.json`;
  a.click();
}

G.connectWallet = connectWallet;
G.runTest = runTest;
G.loadSetup = loadSetup;

function wire() {
  for (const id of ['T1', 'T2', 'T3']) {
    $('run-' + id).onclick = () => runTest(id, uiOpts()).catch((e) => log(errText(e), 'bad'));
  }
  $('dl').onclick = download;
  $('clear').onclick = () => { G.records = []; persist(); renderRecords(); };
  $('loadsetup').onclick = () => loadSetup(G.wallet?.pubkey, $('setupjson').value).catch(() => {});
}
function uiOpts() {
  return {
    popup: $('popup').value, displayed: $('displayed').value, note: $('note').value,
    kastleVariant: $('kvariant').value || undefined, kaspireMode: $('kmode').value || undefined,
    sighashType: Number($('sighash').value) || 1, useWalletTx: $('usewallettx').checked,
  };
}
wire();
boot().catch((e) => { log('boot failed: ' + errText(e), 'bad'); $('boot').textContent = 'boot failed: ' + errText(e); });
