// A stand-in kob CLI: FAKE_KOB_MODE ok | plan-error | submit-fail | usage | chain (a ledger in
// FAKE_KOB_CHAIN). Calls go to FAKE_KOB_LOG.
import { appendFileSync, readFileSync, writeFileSync } from 'node:fs';

const args = process.argv.slice(2);
appendFileSync(process.env.FAKE_KOB_LOG, JSON.stringify(args) + '\n');
const mode = process.env.FAKE_KOB_MODE ?? 'ok';
// FAKE_KOB_NOTE: the CLI's congestion note on stderr (R7T-1).
if (process.env.FAKE_KOB_NOTE) process.stderr.write('note: blocks are contended (the node estimates 3 sompi per gram for the next minute). KOB transactions are ranked by their storage mass, which the normal fee does not cover, so they may wait.\n');
const dry = args.includes('--dry-run');
const tx = 'ab'.repeat(32);
// The template hash of each order kind in this build (the indexer names it in its views).
export const TPL = { KobAsk: 'a5'.repeat(32), KobBid: 'b5'.repeat(32) };
const doc = (extra) => JSON.stringify({ action: 'place limit sell', network: 'testnet-10', plan: {}, issues: [], transaction: { txid: tx, fee: '427000' }, new_covenant_ids: ['cd'.repeat(32)], result: null, ...extra }, null, 2);

// FAKE_KOB_VERSION: another build swapped in under the running bot (R7T-5).
if (args[0] === '--version') { process.stdout.write(`${process.env.FAKE_KOB_VERSION ?? 'kob 0.0.0 (fake build)'}\n`); process.exit(0); }
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
        (c.outputs ??= []).push({ txid: tx, index: 0, amount: o.value - 427000 });
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
    // --min-fill: kept in the order's state (a whole-fill ask has it equal to its amount).
    const minFill = args.includes('--min-fill') ? Number(val('--min-fill').replace('base', '')) : null;
    const st = stateOf(side === 'sell' ? 1 : 2, price, amount, minFill, txid);
    submit({ ids: [id], plan: { states: [st], request: { value: String(side === 'sell' ? 1e8 : escrow + 1e8) } }, apply: () => {
      // FAKE_KOB_PENDING: submitted, but it waits in the mempool until the test mines it (R7T-1).
      const book = process.env.FAKE_KOB_PENDING ? ((c.mempool ??= {}).place ??= {}) : c.orders;
      if (side === 'sell') { c.tokenFree -= amount; c.kasFree -= 1e8; book[id] = { side: 1, price, amount, value: 1e8, txid, expiryDaa: txid, expiresAt: gtd(), ...(minFill != null ? { minFill } : {}) }; }
      else { c.kasFree -= escrow + 1e8; book[id] = { side: 2, price, amount, value: escrow + 1e8, txid, expiryDaa: txid, expiresAt: gtd() }; }
    } });
  }
  if (args[0] === 'replace') {
    const id = args[1];
    const o = c.orders[id];
    if (!o) { process.stderr.write('error: unknown order'); process.exit(1); }
    const np = args.includes('--price') ? Number(val('--price').replace('sompi', '')) : o.price;
    // FAKE_KOB_REPLACE_NEW: a cancel-replace (strays on the order); `lost`: a fill of 1,500,007
    // lands first and the replace never confirms (R6T-2). No change comes back: it rides on the
    // replacement.
    const how = process.env.FAKE_KOB_REPLACE_NEW;
    if (how) {
      const nid = ('d' + (c.n + 1).toString(16)).padStart(64, '0');
      submit({ ids: [nid], plan: { inPlace: false, current: stateOf(o.side, o.price, o.amount, o.minFill, o.expiryDaa), states: [stateOf(o.side, np, o.amount, o.minFill, txid)], value: String(o.value - fee) }, apply: () => {
        // The fill moves the order to a new outpoint, which the replace does not spend.
        if (how === 'lost') { o.amount -= 1_500_007; o.txid = `f${txid.slice(1)}`; return; }
        c.orders[nid] = { ...o, price: np, txid, expiryDaa: txid, expiresAt: gtd(), value: o.value - fee };
        c.ended[id] = 'cancelled';
        delete c.orders[id];
        c.kasFree += fee;
      } });
    }
    submit({ plan: { inPlace: true, current: stateOf(o.side, o.price, o.amount, o.minFill, o.expiryDaa), replacement: stateOf(o.side, np, o.amount, o.minFill, txid), value: String(o.value - fee) }, apply:() => {
      // FAKE_KOB_PENDING: the amend waits in the mempool, spending the order's outpoint (R7T-1).
      if (process.env.FAKE_KOB_PENDING) { ((c.mempool ??= {}).amend ??= []).push({ id, input: o.txid, txid, expiryDaa: txid, price: np, expiresAt: args.includes('--gtd') ? gtd() : o.expiresAt }); return; }
      if (args.includes('--price')) o.price = Number(val('--price').replace('sompi', '')); if (args.includes('--gtd')) o.expiresAt = gtd(); o.txid = txid; o.expiryDaa = txid; o.value -= fee; c.kasFree += fee; } }); // an amend's fee comes from the order
  }
  if (args[0] === 'cancel') {
    const id = args[1];
    const o = c.orders[id];
    if (!o) { process.stderr.write('error: the order is not live'); process.exit(1); }
    // An order someone else made under the bot's key, with no custody (R3T-2).
    if (o.foreign) { process.stderr.write('error: custody not found on the node: not cancelled; nothing signed\n'); process.exit(1); }
    submit({ apply: () => { if (o.side === 1) { c.tokenFree += o.amount; c.tokenUtxos += 1; } c.kasFree += o.value; c.ended[id] = 'cancelled'; (c.outputs ??= []).push({ txid, index: 0, amount: o.value - fee }); delete c.orders[id]; } });
  }
  if (args[0] === 'recover') recover(c, fee);
  if (args[0] === 'merge') {
    submit({ apply: () => { c.tokenUtxos = 1; } });
  }
  process.stderr.write('error: unknown command');
  process.exit(2);
}

/** The order state as the CLI prints it (serde AnyState), with the fields the bot reads. */
// An amend keeps the minimum fill, at most the amount left (kob replace). `expiryDaa` stands for
// the expiry each send computes from --gtd (R7T-I6): here the sending transaction's id, so that
// two amends to one price are two scripts, as on chain.
function stateOf(side, price, amount, minFill = null, expiryDaa = null) {
  const st = { maker: 'a1d9fd1347128011ad0036b077949d63274d5d3b8ac8351776c30c945ef2c2fb', tokenCovId: '01'.repeat(32), scale: '1000000', price: String(price), extensionCommitment: '00'.repeat(32), ...(expiryDaa ? { expiryDaa } : {}) };
  const mf = minFill == null ? {} : { minFill: String(Math.min(minFill, amount)) };
  return side === 1 ? { kind: 'KobAsk', state: { ...st, ...mf, amountLeft: String(amount) } } : { kind: 'KobBid', state: st };
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
  args.forEach((a, i) => { if (a === '--amount-left') { const [id, n] = args[i + 1].split('='); (hints[id] ??= []).push(Number(n)); } });
  const byId = new Map();
  for (const it of items) {
    const id = it.covenant_id;
    if (byId.get(id)?.status === 'live') continue;
    const o = c.orders[id];
    const kind = it.state.kind;
    // The CLI's notes: a bid's script never changes, so not finding it is proof it is spent; an
    // ask's amount may have moved to one the search did not reach.
    const note = kind === 'KobBid' ? "spent: no unspent output at the order's script (1 candidate)"
      : "spent, or partly filled to an amount the search did not reach (3 candidates: the known states, the --amount-left hints, then amountLeft - k x minFill; that search is not exhaustive: pass the order's amount left from an indexer view with --amount-left)";
    const e = { covenant_id: id, kind, template_hash: it.template_hash, status: 'not_found', note };
    const st = it.state.state;
    if (it.template_hash === TPL[kind] && o && (o.side === 1) === (kind === 'KobAsk') && o.price === Number(st.price)
      && (st.expiryDaa == null || o.expiryDaa == null || st.expiryDaa === o.expiryDaa)
      && (o.side === 2 || o.amount === Number(st.amountLeft) || (hints[id] ?? []).includes(o.amount))) {
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
      // FAKE_KOB_CANCEL_LOST=n: the cancel is submitted, but a fill of n base units lands first and
      // the cancel never confirms (R6T-2): the ask moves to amount - n, no output comes back.
      if (args.includes('--yes') && process.env.FAKE_KOB_CANCEL_LOST && o.side === 1) {
        c.n += 1;
        o.amount -= Number(process.env.FAKE_KOB_CANCEL_LOST);
      } else if (args.includes('--yes')) {
        c.n += 1;
        (c.outputs ??= []).push({ txid, index: 0, amount: o.value - fee });
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
