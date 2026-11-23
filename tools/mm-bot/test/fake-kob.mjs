// A stand-in kob CLI: FAKE_KOB_MODE ok | plan-error | submit-fail | usage | chain (a ledger in
// FAKE_KOB_CHAIN). Calls go to FAKE_KOB_LOG.
import { appendFileSync, readFileSync, writeFileSync } from 'node:fs';

const args = process.argv.slice(2);
appendFileSync(process.env.FAKE_KOB_LOG, JSON.stringify(args) + '\n');
const mode = process.env.FAKE_KOB_MODE ?? 'ok';
const dry = args.includes('--dry-run');
const tx = 'ab'.repeat(32);
// The template hash of each order kind in this build (the indexer names it in its views).
export const TPL = { KobAsk: 'a5'.repeat(32), KobBid: 'b5'.repeat(32) };
const doc = (extra) => JSON.stringify({ action: 'place limit sell', network: 'testnet-10', plan: {}, issues: [], transaction: { txid: tx, fee: '427000' }, new_covenant_ids: ['cd'.repeat(32)], result: null, ...extra }, null, 2);

if (args[0] === '--version') { process.stdout.write('kob 0.0.0 (fake build)\n'); process.exit(0); }
if (mode === 'usage') {
  process.stderr.write('error: the amount needs a unit\n');
  process.exit(2);
}
if (args[0] === 'cancel' && args.includes('--view')) {
  // A cancel per saved view, checked on the node only (no indexer): one document each.
  const items = JSON.parse(readFileSync(args[args.indexOf('--view') + 1], 'utf8')).items ?? [];
  // In chain mode the node is the ledger: a view whose order is gone or has moved is stale.
  const c = mode === 'chain' ? JSON.parse(readFileSync(process.env.FAKE_KOB_CHAIN, 'utf8')) : null;
  if (c && process.env.FAKE_KOB_NODE_DOWN) { process.stderr.write('error: node: connection refused\n'); process.exit(1); }
  let code = 0;
  for (const v of items) {
    if (c) {
      const o = c.orders[v.covenant_id];
      if (!o || o.txid !== v.current?.txid) {
        process.stderr.write(`refused: stale view: the node holds no unspent output at the order's script with its covenant id at ${v.current?.txid}:0\n`);
        code = 1;
        continue;
      }
      if (!dry) {
        if (o.side === 1) { c.tokenFree += o.amount; c.tokenUtxos += 1; }
        c.kasFree += o.value - 427000;
        c.ended[v.covenant_id] = 'cancelled';
        delete c.orders[v.covenant_id];
        writeFileSync(process.env.FAKE_KOB_CHAIN, JSON.stringify(c));
      }
    }
    process.stdout.write(JSON.stringify({ action: `cancel ${v.covenant_id}`, network: 'testnet-10', plan: {}, issues: [], transaction: { txid: tx, fee: '427000' }, new_covenant_ids: [],
      result: dry ? null : { status: 'submitted', txid: tx, fee: '427000', covenant_ids: [], file: null, engine_validated: true } }));
  }
  process.exit(code);
}
if (mode === 'chain') chain();
if (args[0] === 'balances') {
  process.stdout.write(JSON.stringify({ maker: 'ef'.repeat(32), kas: { free: '100000000000', locked: '0', total: '100000000000' },
    tokens: [{ token: '01'.repeat(32), decimals: 6, free: '5000000', in_orders: '0', free_utxos: 2, free_verified: true }], skipped: [] }));
  process.exit(0);
}
if (mode === 'plan-error') {
  process.stdout.write(doc({ issues: [{ code: 'TOKEN_UTXOS_FRAGMENTED', severity: 'error', message: 'more than 3 token inputs' }] }));
  process.stderr.write('error: the plan has errors: nothing signed\n');
  process.exit(1);
}
if (dry) {
  process.stdout.write(doc({}));
  process.stderr.write('dry run: nothing signed or submitted\n');
  process.exit(0);
}
if (mode === 'submit-fail') {
  process.stderr.write('error: submission failed: orphan\n');
  process.exit(1);
}
process.stdout.write(doc({ result: { status: 'submitted', txid: tx, fee: '427000', covenant_ids: ['cd'.repeat(32)], file: null, engine_validated: true } }));
process.exit(0);

function chain() {
  const file = process.env.FAKE_KOB_CHAIN;
  const c = JSON.parse(readFileSync(file, 'utf8'));
  const val = (flag) => args[args.indexOf(flag) + 1];
  const gtd = () => (args.includes('--gtd') ? Date.parse(val('--gtd')) : null); // the expiry, ms (null: a 90-day GTC)
  const out = (o) => { process.stdout.write(JSON.stringify(o)); process.exit(0); };
  const fee = 500000;
  if (args[0] === 'balances') {
    out({ maker: c.maker, kas: { free: String(c.kasFree), locked: '0', total: String(c.kasFree) },
      tokens: [{ token: c.token, decimals: 6, free: String(c.tokenFree), in_orders: '0', free_utxos: c.tokenUtxos, free_verified: true }], skipped: [] });
  }
  const txid = (c.n + 1).toString(16).padStart(64, '0');
  const submit = (extra = {}) => {
    if (dry) out({ issues: [], transaction: { txid, fee: String(fee) }, new_covenant_ids: extra.ids ?? [], plan: extra.plan ?? {}, result: null });
    c.n += 1;
    c.kasFree -= fee;
    extra.apply?.();
    writeFileSync(file, JSON.stringify(c));
    out({ issues: [], transaction: { txid, fee: String(fee) }, new_covenant_ids: extra.ids ?? [], plan: extra.plan ?? {}, result: { status: 'submitted', txid, fee: String(fee), covenant_ids: extra.ids ?? [] } });
  };
  if (args[0] === 'place') {
    // FAKE_KOB_REFUSE_PLACE: every placement is refused before signing (R2-T3).
    if (process.env.FAKE_KOB_REFUSE_PLACE) { process.stderr.write('error: its decimals come from the indexer alone: give --decimals 6; nothing signed\n'); process.exit(1); }
    const side = val('--side');
    const amount = Number(val('--amount').replace('base', ''));
    const price = Number(val('--price').replace('sompi', ''));
    const id = ('c' + (c.n + 1).toString(16)).padStart(64, '0');
    const escrow = Math.ceil((amount * price) / 1e6);
    if (side === 'sell' && c.tokenUtxos > 3) { process.stderr.write('error: TOKEN_UTXOS_FRAGMENTED'); process.exit(1); }
    const st = stateOf(side === 'sell' ? 1 : 2, price, amount);
    submit({ ids: [id], plan: { states: [st], request: { value: String(side === 'sell' ? 1e8 : escrow + 1e8) } }, apply: () => {
      if (side === 'sell') { c.tokenFree -= amount; c.kasFree -= 1e8; c.orders[id] = { side: 1, price, amount, value: 1e8, txid, expiresAt: gtd() }; }
      else { c.kasFree -= escrow + 1e8; c.orders[id] = { side: 2, price, amount, value: escrow + 1e8, txid, expiresAt: gtd() }; }
    } });
  }
  if (args[0] === 'replace') {
    const id = args[1];
    const o = c.orders[id];
    if (!o) { process.stderr.write('error: unknown order'); process.exit(1); }
    const np = args.includes('--price') ? Number(val('--price').replace('sompi', '')) : o.price;
    submit({ plan: { inPlace: true, current: stateOf(o.side, o.price, o.amount), replacement: stateOf(o.side, np, o.amount), value: String(o.value - fee) }, apply:() => { if (args.includes('--price')) o.price = Number(val('--price').replace('sompi', '')); if (args.includes('--gtd')) o.expiresAt = gtd(); o.txid = txid; o.value -= fee; c.kasFree += fee; } }); // an amend's fee comes from the order
  }
  if (args[0] === 'cancel') {
    const id = args[1];
    const o = c.orders[id];
    if (!o) { process.stderr.write('error: the order is not live'); process.exit(1); }
    submit({ apply: () => { if (o.side === 1) { c.tokenFree += o.amount; c.tokenUtxos += 1; } c.kasFree += o.value; c.ended[id] = 'cancelled'; delete c.orders[id]; } });
  }
  if (args[0] === 'recover') recover(c, fee);
  if (args[0] === 'merge') {
    submit({ apply: () => { c.tokenUtxos = 1; } });
  }
  process.stderr.write('error: unknown command');
  process.exit(2);
}

/** The order state as the CLI prints it (serde AnyState), with the fields the bot reads. */
function stateOf(side, price, amount) {
  const st = { maker: 'a1d9fd1347128011ad0036b077949d63274d5d3b8ac8351776c30c945ef2c2fb', tokenCovId: '01'.repeat(32), scale: '1000000', price: String(price), extensionCommitment: '00'.repeat(32) };
  return side === 1 ? { kind: 'KobAsk', state: { ...st, amountLeft: String(amount) } } : { kind: 'KobBid', state: st };
}

/**
 * kob recover: a record is live when the chain holds its order, its kind's template and its state
 * (the price; an ask's amountLeft, or the --amount-left hint for it). With --cancel the live ones
 * are cancelled (--yes) or signed only (--dry-run). FAKE_KOB_NODE_DOWN: the node does not answer.
 */
function recover(c, fee) {
  if (process.env.FAKE_KOB_NODE_DOWN) { process.stderr.write('error: node: connection refused\n'); process.exit(1); }
  const val = (flag) => args[args.indexOf(flag) + 1];
  const items = JSON.parse(readFileSync(val('--from'), 'utf8')).items;
  const hints = {};
  args.forEach((a, i) => { if (a === '--amount-left') { const [id, n] = args[i + 1].split('='); hints[id] = Number(n); } });
  const byId = new Map();
  for (const it of items) {
    const id = it.covenant_id;
    if (byId.get(id)?.status === 'live') continue;
    const o = c.orders[id];
    const kind = it.state.kind;
    const e = { covenant_id: id, kind, template_hash: it.template_hash, status: 'not_found', note: 'spent' };
    const st = it.state.state;
    if (it.template_hash === TPL[kind] && o && (o.side === 1) === (kind === 'KobAsk') && o.price === Number(st.price)
      && (o.side === 2 || o.amount === Number(st.amountLeft) || o.amount === hints[id])) {
      const left = o.side === 1 ? o.amount : Math.floor(((o.value - 1e8) * 1e6) / o.price);
      Object.assign(e, { status: 'live', note: '', outpoint: `${o.txid}:0`, value: String(o.value), amount_left: String(left) });
    }
    byId.set(id, e);
  }
  let code = 0;
  if (args.includes('--cancel')) {
    for (const e of byId.values()) {
      if (e.status !== 'live') continue;
      if (process.env.FAKE_KOB_CANCEL_FAIL) { e.cancel = { status: 'failed', reason: 'submit: orphan' }; code = 1; continue; }
      const o = c.orders[e.covenant_id];
      const txid = (c.n + 1).toString(16).padStart(64, '0');
      if (args.includes('--yes')) {
        c.n += 1;
        if (o.side === 1) { c.tokenFree += o.amount; c.tokenUtxos += 1; }
        c.kasFree += o.value - fee;
        c.ended[e.covenant_id] = 'cancelled';
        delete c.orders[e.covenant_id];
      }
      e.cancel = { status: args.includes('--yes') ? 'submitted' : 'signed', txid, fee: String(fee) };
    }
  }
  writeFileSync(process.env.FAKE_KOB_CHAIN, JSON.stringify(c));
  process.stdout.write(JSON.stringify({ network: 'testnet-10', orders: [...byId.values()], rejected: [], summary: {} }, null, 2));
  process.exit(code);
}
