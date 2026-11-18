// A stand-in kob CLI with a small chain in FAKE_KOB_CHAIN (JSON); every call goes to FAKE_KOB_LOG.
import { appendFileSync, existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';

const args = process.argv.slice(2);
appendFileSync(process.env.FAKE_KOB_LOG, `${JSON.stringify(args)}\n`);
const file = process.env.FAKE_KOB_CHAIN;
const c = existsSync(file) ? JSON.parse(readFileSync(file, 'utf8'))
  : { n: 0, kasFree: 1000e8, tusdFree: 1000e8, tusdVerified: true, orders: {}, book: { best_bid: '24000000', best_ask: '26000000', last_fill: '25000000' } };
const val = (flag) => (args.includes(flag) ? args[args.indexOf(flag) + 1] : null);
const dry = args.includes('--dry-run');
const yes = args.includes('--yes');
const FEE = 50000;
const MAKER = 'ef'.repeat(32);
const TUSD = '55'.repeat(32);
const base = (s) => Math.round(units(s) * 1e8); // TUSD has 8 decimals
const save = () => writeFileSync(file, JSON.stringify(c));
const print = (o, code = 0) => { process.stdout.write(JSON.stringify(o, null, 2)); save(); process.exit(code); };
const kasOf = (s) => (/sompi$/i.test(s) ? Number(s.replace(/sompi$/i, '')) : Math.round(Number(s.replace(/kas$/i, '')) * 1e8));
const units = (s) => Number(s.replace(/[A-Za-z]+$/, ''));
const txid = () => (c.n + 1).toString(16).padStart(64, '0');

/** A signing command: the plan, then (with --yes) the submission that applies `apply`. */
function sign(action, net, plan = {}, ids = [], apply = () => {}, issues = [], tusdNet = 0) {
  const tokens = tusdNet ? { [TUSD]: String(tusdNet) } : {};
  const doc = { action, network: 'testnet-10', plan, issues, transaction: { txid: txid(), inputs: ['in #0 your KAS'], outputs: ['out #0'], fee: String(FEE), net: { kas: String(net), tokens }, signatures: 1 }, new_covenant_ids: ids, result: null };
  if (issues.some((i) => i.severity === 'error')) { process.stderr.write('error: the plan has errors: nothing signed\n'); print(doc, 1); }
  if (dry) { process.stderr.write('dry run: nothing signed or submitted\n'); print(doc); }
  if (!yes) { process.stderr.write('not signed: pass --yes to sign and submit\n'); print(doc, 1); }
  if (process.env.FAKE_KOB_SUBMIT_FAIL) { process.stderr.write('error: submit: node rejected the request: orphan\n'); print(doc, 1); }
  c.n += 1;
  c.kasFree += net;
  c.tusdFree += tusdNet;
  apply();
  // the node took it and the reply was lost (kob-cli flow.rs: the plan with result null, exit 1)
  if (process.env.FAKE_KOB_SUBMIT_LOST) { process.stderr.write('error: submit: socket: timed out\n'); print(doc, 1); }
  print({ ...doc, result: { status: 'submitted', txid: doc.transaction.txid, fee: String(FEE), covenant_ids: ids, file: null, engine_validated: true } });
}

// FAKE_KOB_FEATURES=claim,delivery: the help of a CLI with bid delivery
const features = (process.env.FAKE_KOB_FEATURES ?? '').split(',');
if (args.includes('--help')) {
  if (args[0] === 'claim' && !features.includes('claim')) { process.stderr.write('error: unknown command `claim`\n'); process.exit(2); }
  process.stdout.write(args[0] === 'claim' ? 'kob claim: take what a collected buy order bought\n'
    : `kob place: place an order\n  --amount A\n${features.includes('delivery') ? '  --delivery D   how a buy hands over what it buys\n' : ''}`);
  process.exit(0);
}

switch (args[0]) {
  case 'claim':
    sign(`claim ${args[1]}`, -FEE, { order: args[1] });
    break;
  case 'balances':
    print({ maker: MAKER, kas: { free: String(c.kasFree), locked: '0', total: String(c.kasFree) },
      tokens: [{ token: TUSD, ticker: 'TUSD', decimals: 8, free: String(c.tusdFree), free_verified: c.tusdVerified }], skipped: [] });
    break;
  case 'tokens':
    print({ network: 'testnet-10', tokens: [{ ticker: 'TUSD', covenant_id: '55'.repeat(32), decimals: 8, open_asks: 1, open_bids: 1, warnings: [] }] });
    break;
  case 'book':
    print({ token: '55'.repeat(32), quote: null, network: 'testnet-10', scale: 1e8, asks: [], bids: [], ...c.book, crossed: false });
    break;
  case 'orders':
    print({ maker: MAKER, network: 'testnet-10', orders: Object.entries(c.orders).map(([id, o]) => ({ covenant_id: id, ...o })) });
    break;
  case 'place': {
    const side = val('--side');
    const price = kasOf(val('--price'));
    const amount = units(val('--amount'));
    const id = (`c${c.n + 1}`).padStart(64, '0');
    const value = (side === 'buy' ? Math.round(amount * price) : 0) + 1e8;
    sign(`place limit ${side}`, -(value + FEE), { states: [] }, [id], () => { c.orders[id] = { side, price, amount, value }; }, [], side === 'sell' ? -base(val('--amount')) : 0);
    break;
  }
  case 'replace': {
    const o = c.orders[args[1]];
    if (!o) { process.stderr.write('error: unknown order\n'); process.exit(1); }
    sign(`amend in place ${args[1]}`, -FEE, { order: args[1], inPlace: true }, [], () => { o.price = kasOf(val('--price')); });
    break;
  }
  case 'cancel': {
    const o = c.orders[args[1]];
    if (!o) { process.stderr.write('error: the order is not live\n'); process.exit(1); }
    sign(`cancel ${args[1]}`, o.value - FEE, { action: 'cancel', covenant_id: args[1] }, [], () => { delete c.orders[args[1]]; });
    break;
  }
  case 'take': {
    if (val('--token') !== TUSD) { process.stderr.write('error: the order trades another token than --token: nothing signed\n'); process.exit(1); }
    if (yes && !val('--max-price') && !val('--min-price')) { process.stderr.write('error: a price bound is required when the confirmation is skipped\n'); process.exit(2); }
    const price = Number(c.book.best_ask);
    const amount = units(val('--amount'));
    const max = val('--max-price') ? kasOf(val('--max-price')) : null;
    if (max != null && price > max && process.env.FAKE_KOB_TAKE_UNBUILT) {
      // the CLI's other form: the fill could not be built either, so the refusal is only the stderr text
      process.stderr.write('error: this fill pays 0.26 KAS per TUSD (the order\'s price 0.26 KAS per TUSD at the lock time (DAA 5), less its tip 0 KAS per TUSD that you keep), above your --max-price 0.2 KAS per TUSD: nothing signed\n');
      process.exit(1);
    }
    const issues = max != null && price > max ?[{ code: 'TAKE_PRICE_ABOVE_MAX', severity: 'error', message: 'this fill pays more than your --max-price: nothing signed' }] : [];
    if (val('--min-price')) { sign(`take ${args[1]} (sell into a buy order)`, Math.round(amount * Number(c.book.best_bid)) - FEE, { order: args[1] }, [], () => {}, [], -base(val('--amount'))); }
    sign(`take ${args[1]} (buy from a sell order)`, -(Math.round(amount * price) + FEE), { order: args[1], price: String(price), bound: max != null ? { kind: 'max', per_token: String(max) } : null }, [], () => {}, issues);
    break;
  }
  case 'send':
    sign(`send ${args[3]} to ${args[2]}`, args[1] === 'KAS' ? -(kasOf(args[3]) + FEE) : -(1e8 + FEE), { to: args[2], amount: String(args[3]) }, [], () => {}, [], args[1] === 'KAS' ? 0 : -base(args[3]));
    break;
  case 'merge': {
    const doc = { action: `merge ${args[1]}`, network: 'testnet-10', token: '55'.repeat(32), utxos_before: 3, utxos_after: 1, transactions: 1, merged_amount: '100', fee: String(FEE), links: [] };
    if (dry) print(doc);
    c.n += 1;
    c.kasFree -= FEE;
    print({ ...doc, links: [{ link: 1, status: 'submitted', txid: txid(), fee: String(FEE) }] });
    break;
  }
  case 'token': {
    const [txidIn, , amountIn] = val('--fund-utxo').split(':');
    const dir = val('--out-dir');
    mkdirSync(dir, { recursive: true });
    const carrier = Number(val('--carrier') ?? 1e9);
    const fund = Number(amountIn);
    const cov = 'd0'.repeat(32);
    writeFileSync(join(dir, 'preparation_tx.json'), JSON.stringify({ outputs: [{ value: carrier + 3000 }, { value: fund - carrier - 3000 - 2000 }], inputs: [{ previousOutpoint: { transactionId: txidIn } }] }));
    writeFileSync(join(dir, 'genesis_tx.json'), JSON.stringify({ outputs: [{ value: carrier }] }));
    writeFileSync(join(dir, 'creation.json'), JSON.stringify({ covenant_id: cov, preparation_txid: 'a1'.repeat(32), genesis_txid: 'a2'.repeat(32), signed: true }));
    if (!dry && !yes) { process.stderr.write('error: not signed: pass --yes\n'); process.exit(1); }
    if (yes) { c.n += 2; c.kasFree -= carrier + 5000; }
    process.stdout.write(`token ${val('--ticker')}\ncovenant id ${cov}\n${dry ? 'dry run: nothing submitted' : 'genesis submitted'}\n`);
    save();
    process.exit(0);
    break;
  }
  default:
    process.stderr.write(`error: unknown command ${args[0]}\n`);
    process.exit(2);
}
