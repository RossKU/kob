// Every soak key's live token UTXOs, verified on the node: candidates from the indexer (`/v1/token-utxos?owner=`), the bots' local trackers
// (run/state/tracker-<key>.json) and, with EXTRA=<file>, a previous output of this script; each kept only if the P2SH of its state holds the
// outpoint. Writes run/holdings.json (OUT=<name>); `--seed` rewrites every tracker to exactly the verified set (the pre-run holdings a fresh
// indexer never lists live only there: run it before the bots start, see README "Redeploy on main ae87d77"). Read-only otherwise.
//   node scripts/holdings.cjs [--seed]   (API=http://127.0.0.1:8091, NODE=ws://..., RUN=<run dir>)
const fs = require('fs');
const RUN = process.env.RUN || __dirname + '/../run';
const k = require(__dirname + '/../../../web/vendor/kaspa-node/kaspa.js');
const w = require(RUN + '/wasm-node/kob_wasm.js');
const keys = JSON.parse(fs.readFileSync(RUN + '/keys.json'));
const st = JSON.parse(fs.readFileSync(RUN + '/state.json'));
const TOK = Object.fromEntries(['token', 'token2', 'token3'].filter((s) => st[s]).map((s) => [st[s].covenantId, st[s].ticker]));
const API = process.env.API || 'http://127.0.0.1:8091';
const ws = new WebSocket(process.env.NODE || JSON.parse(fs.readFileSync(RUN + '/config.json')).nodeUrl); let id = 0; const pend = new Map();
const call = (method, params) => new Promise((r) => { const i = ++id; pend.set(i, r); ws.send(JSON.stringify({ id: i, method, params })); });
ws.onmessage = (e) => { const m = JSON.parse(e.data); const r = pend.get(m.id); if (r) { pend.delete(m.id); r(m); } };
ws.onopen = async () => {
  const out = {};
  for (const [name, key] of Object.entries(keys)) {
    const pk = key.publicKey.toLowerCase(); const cand = new Map();
    for (const tok of Object.keys(TOK)) {
      let cursor = '';
      for (;;) {
        const r = await (await fetch(`${API}/v1/token-utxos?owner=${pk}&token=${tok}&spent=false&limit=500${cursor ? `&cursor=${cursor}` : ''}`)).json();
        for (const x of r.items) if (x.role === 'owned' && x.state) cand.set(`${x.txid}:${x.index}`, { transactionId: x.txid, index: x.index, tokenCovId: tok, program: 'KCC20Ref_8x8', state: x.state, carrier: String(x.value), addedAt: Date.now() });
        cursor = r.next_cursor; if (!cursor || !r.items.length) break;
      }
    }
    const tf = `${RUN}/state/tracker-${name}.json`;
    if (fs.existsSync(tf)) { const s = JSON.parse(fs.readFileSync(tf)); for (const v of Object.values(s)) for (const it of JSON.parse(v).items) if (TOK[it.tokenCovId]) cand.set(`${it.transactionId}:${it.index}`, it); }
    if (process.env.EXTRA) for (const it of (JSON.parse(fs.readFileSync(process.env.EXTRA))[name] ?? [])) cand.set(`${it.transactionId}:${it.index}`, it);
    const live = [];
    const byAddr = new Map();
    for (const c of cand.values()) {
      const spk = w.tokenScriptPublicKey(c.program, JSON.stringify(c.state));
      const a = k.addressFromScriptPublicKey(new k.ScriptPublicKey(0, spk.slice(4)), 'testnet-10').toString();
      (byAddr.get(a) ?? byAddr.set(a, []).get(a)).push(c);
    }
    const addrs = [...byAddr.keys()];
    for (let i = 0; i < addrs.length; i += 200) {
      const r = await call('getUtxosByAddresses', { addresses: addrs.slice(i, i + 200) });
      const have = new Set(r.params.entries.map((e) => `${e.outpoint.transactionId}:${e.outpoint.index}`));
      for (const a of addrs.slice(i, i + 200)) for (const c of byAddr.get(a)) if (have.has(`${c.transactionId}:${c.index}`)) live.push(c);
    }
    out[name] = live;
  }
  fs.writeFileSync(RUN + '/' + (process.env.OUT || 'holdings.json'), JSON.stringify(out));
  for (const [name, live] of Object.entries(out)) {
    const sum = {}; for (const c of live) { const t = TOK[c.tokenCovId]; sum[t] ??= { n: 0, amt: 0n, carrier: 0n }; sum[t].n++; sum[t].amt += BigInt(c.state.amount); sum[t].carrier += BigInt(c.carrier); }
    console.log(name.padEnd(9), Object.entries(sum).map(([t, s]) => `${t} ${s.n} utxos ${(Number(s.amt) / 1e8).toFixed(4)} (carriers ${Number(s.carrier) / 1e8} KAS)`).join(' | '));
    if (process.argv.includes('--seed') && !process.env.NOSEED) {
      const pk = keys[name].publicKey.toLowerCase();
      fs.writeFileSync(`${RUN}/state/tracker-${name}.json`, JSON.stringify({ [`kob.tokens.v1.testnet-10.${pk}`]: JSON.stringify({ v: 1, items: live }) }));
    }
  }
  const tot = {}; for (const live of Object.values(out)) for (const c of live) tot[TOK[c.tokenCovId]] = (tot[TOK[c.tokenCovId]] ?? 0n) + BigInt(c.state.amount);
  console.log('totals', Object.fromEntries(Object.entries(tot).map(([t, v]) => [t, Number(v) / 1e8])));
  ws.close();
};
