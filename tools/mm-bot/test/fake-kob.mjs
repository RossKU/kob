// A stand-in kob CLI: FAKE_KOB_MODE ok | plan-error | submit-fail | usage | chain (a ledger in
// FAKE_KOB_CHAIN). Calls go to FAKE_KOB_LOG.
import { appendFileSync, readFileSync, writeFileSync } from 'node:fs';

const args = process.argv.slice(2);
appendFileSync(process.env.FAKE_KOB_LOG, JSON.stringify(args) + '\n');
const mode = process.env.FAKE_KOB_MODE ?? 'ok';
const dry = args.includes('--dry-run');
const tx = 'ab'.repeat(32);
const doc = (extra) => JSON.stringify({ action: 'place limit sell', network: 'testnet-10', plan: {}, issues: [], transaction: { txid: tx, fee: '427000' }, new_covenant_ids: ['cd'.repeat(32)], result: null, ...extra }, null, 2);

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
    const side = val('--side');
    const amount = Number(val('--amount').replace('base', ''));
    const price = Number(val('--price').replace('sompi', ''));
    const id = ('c' + (c.n + 1).toString(16)).padStart(64, '0');
    const escrow = Math.ceil((amount * price) / 1e6);
    if (side === 'sell' && c.tokenUtxos > 3) { process.stderr.write('error: TOKEN_UTXOS_FRAGMENTED'); process.exit(1); }
    submit({ ids: [id], apply: () => {
      if (side === 'sell') { c.tokenFree -= amount; c.kasFree -= 1e8; c.orders[id] = { side: 1, price, amount, value: 1e8, txid, expiresAt: gtd() }; }
      else { c.kasFree -= escrow + 1e8; c.orders[id] = { side: 2, price, amount, value: escrow + 1e8, txid, expiresAt: gtd() }; }
    } });
  }
  if (args[0] === 'replace') {
    const id = args[1];
    const o = c.orders[id];
    if (!o) { process.stderr.write('error: unknown order'); process.exit(1); }
    submit({ plan: { inPlace: true }, apply: () => { if (args.includes('--price')) o.price = Number(val('--price').replace('sompi', '')); if (args.includes('--gtd')) o.expiresAt = gtd(); o.txid = txid; o.value -= fee; c.kasFree += fee; } }); // an amend's fee comes from the order
  }
  if (args[0] === 'cancel') {
    const id = args[1];
    const o = c.orders[id];
    if (!o) { process.stderr.write('error: the order is not live'); process.exit(1); }
    submit({ apply: () => { if (o.side === 1) { c.tokenFree += o.amount; c.tokenUtxos += 1; } c.kasFree += o.value; c.ended[id] = 'cancelled'; delete c.orders[id]; } });
  }
  if (args[0] === 'merge') {
    submit({ apply: () => { c.tokenUtxos = 1; } });
  }
  process.stderr.write('error: unknown command');
  process.exit(2);
}
