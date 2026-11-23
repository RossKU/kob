import assert from 'node:assert/strict';
import { existsSync, readdirSync, readFileSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { test } from 'node:test';
import { KobService, sentNothing } from '../src/service.mjs';
import { PRIMARY, setup } from './helpers.mjs';

const TUSD = '55'.repeat(32);

const place = { token: 'TUSD', side: 'buy', amount: '2TUSD', price: '0.25KAS' };

test('confirm mode: a write only plans; the confirm with the same arguments sends once', async () => {
  const t = setup();
  const svc = new KobService(t.cfg);
  const p = await svc.write('kob_place_limit', place);
  assert.equal(p.status, 'plan');
  assert.match(p.planId, /^[0-9a-f]{12}$/);
  assert.equal(p.spendKas, '1.5005', 'escrow 0.5 + carrier 1 + fee');
  assert.equal(t.yesCalls().length, 0, 'nothing signed without confirm');
  const s = await svc.write('kob_place_limit', { ...place, confirm: p.planId });
  assert.equal(s.status, 'submitted');
  assert.equal(s.txids.length, 1);
  assert.equal(t.yesCalls().length, 1);
  assert.equal(s.spentTodayKas, '1.5005');
  const again = await svc.write('kob_place_limit', { ...place, confirm: p.planId });
  assert.match(again.reason, /^PLAN_UNKNOWN/, 'a plan sends once');
  assert.equal(t.yesCalls().length, 1);
  const outcomes = t.audit().map((e) => e.outcome);
  assert.deepEqual(outcomes, ['plan', 'submitted', 'refused']);
  assert.equal(t.audit()[1].txids[0], s.txids[0]);
});

test('confirm: changed arguments and an expired plan are refused', async () => {
  const t = setup();
  let now = Date.now();
  const svc = new KobService(t.cfg, { now: () => now });
  const p = await svc.write('kob_place_limit', place);
  const changed = await svc.write('kob_place_limit', { ...place, amount: '20TUSD', confirm: p.planId });
  assert.match(changed.reason, /^PLAN_MISMATCH/);
  now += 301_000;
  const late = await svc.write('kob_place_limit', { ...place, confirm: p.planId });
  assert.match(late.reason, /^PLAN_EXPIRED/);
  assert.equal(t.yesCalls().length, 0);
});

test('auto mode sends within the limits in one call', async () => {
  const t = setup({ mode: 'auto' });
  const r = await new KobService(t.cfg).write('kob_place_limit', place);
  assert.equal(r.status, 'submitted');
  assert.equal(t.yesCalls().length, 1);
});

test('per-transaction and daily caps refuse before signing; the ledger keeps the day', async () => {
  const t = setup({ mode: 'auto', limits: { perTxKas: 60, perDayKas: 100 } });
  const svc = new KobService(t.cfg);
  const big = await svc.write('kob_place_limit', { ...place, amount: '400TUSD' }); // 100 KAS escrow
  assert.match(big.reason, /^PER_TX_CAP/);
  const a = await svc.write('kob_place_limit', { ...place, amount: '200TUSD' }); // 50 + 1 + fee
  assert.equal(a.status, 'submitted');
  const b = await svc.write('kob_place_limit', { ...place, amount: '200TUSD' });
  assert.match(b.reason, /^DAILY_CAP/);
  assert.equal(t.yesCalls().length, 1);
  // A new server on the same data directory sees what was spent.
  const c = await new KobService(t.cfg).write('kob_place_limit', { ...place, amount: '200TUSD' });
  assert.match(c.reason, /^DAILY_CAP/);
});

test('a fee above maxFeeKas is refused', async () => {
  const t = setup({ mode: 'auto', limits: { maxFeeKas: 0.0001 } });
  const p = await new KobService(t.cfg).write('kob_place_limit', place);
  assert.match(p.reason, /^MAX_FEE/);
  assert.equal(t.yesCalls().length, 0);
});

test('the kill switch refuses every write without running the CLI; reads still work', async () => {
  const t = setup({ mode: 'auto' });
  writeFileSync(t.cfg.killSwitchFile, '');
  const svc = new KobService(t.cfg);
  const r = await svc.write('kob_place_limit', place);
  assert.match(r.reason, /^KILL_SWITCH/);
  const s = await svc.write('kob_send', { asset: 'KAS', to: PRIMARY, amount: '1KAS' });
  assert.match(s.reason, /^KILL_SWITCH/);
  assert.equal(t.calls().length, 0, 'the CLI was not run');
  assert.equal((await svc.book({ token: 'TUSD' })).best_ask, '26000000');
  assert.match((await svc.status()).killSwitch, /^ON/);
});

test('kob_send pays only an allow-listed address', async () => {
  const t = setup({ mode: 'auto' });
  const svc = new KobService(t.cfg);
  const other = 'kaspatest:qreyws54r9ku5r7v7mwwmz2hdf603u7w0n807rhh66d5u2lwsxan208z2433y';
  const r = await svc.write('kob_send', { asset: 'KAS', to: other, amount: '5KAS' });
  assert.match(r.reason, /^NOT_ALLOW_LISTED/);
  assert.equal(t.calls().length, 0);
  const ok = await svc.write('kob_send', { asset: 'KAS', to: PRIMARY.toUpperCase(), amount: '5KAS' });
  assert.equal(ok.status, 'submitted');
  assert.deepEqual(t.yesCalls()[0].slice(0, 4), ['send', 'KAS', PRIMARY, '5KAS']);
});

test('take: a bound is required, and a worse price is refused by the CLI before signing', async () => {
  const t = setup({ mode: 'auto' });
  const svc = new KobService(t.cfg);
  const id = 'ab'.repeat(32);
  assert.match((await svc.write('kob_take', { id, token: TUSD, amount: '1TUSD' })).reason, /^PRICE_BOUND_REQUIRED/);
  assert.match((await svc.write('kob_take', { id, token: TUSD, amount: '1TUSD', max_price: '0.3KAS', min_price: '0.2KAS' })).reason, /^PRICE_BOUND_REQUIRED/);
  const r = await svc.write('kob_take', { id, token: TUSD, amount: '1TUSD', max_price: '0.2KAS' });
  assert.match(r.reason, /TAKE_PRICE_ABOVE_MAX/);
  assert.equal(t.yesCalls().length, 0);
  const ok = await svc.write('kob_take', { id, token: TUSD, amount: '1TUSD', max_price: '0.27KAS' });
  assert.equal(ok.status, 'submitted');
});

test('the price band refuses a buy far above the last fill', async () => {
  const t = setup({ mode: 'auto' });
  const r = await new KobService(t.cfg).write('kob_place_limit', { ...place, price: '0.5KAS' });
  assert.match(r.reason, /^PRICE_BAND/);
  assert.equal(t.yesCalls().length, 0);
});

test('arguments that would read as options or units without a unit are refused', async () => {
  const t = setup({ mode: 'auto' });
  const svc = new KobService(t.cfg);
  assert.match((await svc.write('kob_place_limit', { ...place, token: '--yes' })).reason, /^BAD_ARGUMENT/);
  assert.match((await svc.write('kob_place_limit', { ...place, amount: '2' })).reason, /^BAD_ARGUMENT/);
  assert.match((await svc.write('kob_token_create', { name: '--key-file', ticker: 'TT', decimals: 0, supply: '10', fund_utxo: `${'aa'.repeat(32)}:0:5000000000` })).reason, /^BAD_ARGUMENT/);
  assert.equal(t.calls().filter((a) => a[0] !== 'balances').length, 0);
});

test('a send whose reply is lost counts toward the cap (MCP-1)', async () => {
  const t = setup({ mode: 'auto', limits: { perTxKas: 20, perDayKas: 30 } });
  process.env.FAKE_KOB_SUBMIT_LOST = '1';
  const buy = { token: 'TUSD', side: 'buy', amount: '40TUSD', price: '0.25KAS' }; // 10 + 1 + fee
  try {
    const svc = new KobService(t.cfg);
    for (let i = 0; i < 2; i++) {
      const r = await svc.write('kob_place_limit', buy);
      assert.equal(r.status, 'failed');
      assert.equal(r.counted, true);
    }
    assert.match((await svc.write('kob_place_limit', buy)).reason, /^DAILY_CAP/);
    assert.equal(t.yesCalls().length, 2, 'the third send is refused before signing');
    const s = await svc.status();
    assert.equal(s.today.spentKas, '22.001');
    assert.equal(s.today.unknownOutcomes.length, 2);
    assert.equal(s.today.unknownOutcomes[0].txids.length, 1, 'the planned txid, to look up');
  } finally {
    delete process.env.FAKE_KOB_SUBMIT_LOST;
  }
});

/** A resting sell of 200 TUSD at 0.25 KAS (20 % of the 1,000 held), placed in auto mode. */
async function restingSell() {
  const t = setup({ mode: 'auto' });
  const svc = new KobService(t.cfg);
  const s = await svc.write('kob_place_limit', { token: 'TUSD', side: 'sell', amount: '200TUSD', price: '0.25KAS' });
  assert.equal(s.status, 'submitted');
  return { t, svc, id: s.newCovenantIds[0] };
}

test('kob_replace: a sell repriced far below the reference is refused, with no indexer view of the order (E-M4)', async () => {
  const { t, svc, id } = await restingSell();
  // The indexer (http://127.0.0.1:9) is down: the side and token come from the node-checked plan.
  const r = await svc.write('kob_replace', { id, price: '0.01KAS' });
  assert.equal(r.status, 'refused');
  assert.match(r.reason, /^PRICE_BAND: a sell at 0\.01 KAS is more than 25% below/);
  assert.equal(t.yesCalls().length, 1, 'only the placement was signed');
  const bid = await svc.write('kob_place_limit', place);
  assert.match((await svc.write('kob_replace', { id: bid.newCovenantIds[0], price: '1KAS' })).reason, /^PRICE_BAND: a buy at 1 KAS/);
});

test('kob_replace: a sell priced down counts its custody under the token caps; priced up it does not (E-M4)', async () => {
  const { t, svc, id } = await restingSell();
  // 200 out at placement + 200 (the repriced custody) is within 50 % of the day's 1,000.
  const down = await svc.write('kob_replace', { id, price: '0.2KAS' });
  assert.equal(down.status, 'submitted');
  assert.deepEqual(t.ledger().at(-2).tokens, { [TUSD]: String(200e8) });
  const up = await svc.write('kob_replace', { id, price: '0.3KAS' });
  assert.equal(up.status, 'submitted', 'a sell priced up moves nothing against the maker');
  // Each cut counts again: the fourth 200 out today is above 50 % of the day's holding.
  assert.equal((await svc.write('kob_replace', { id, price: '0.21KAS' })).status, 'submitted');
  const again = await svc.write('kob_replace', { id, price: '0.2KAS' });
  assert.match(again.reason, /TOKEN_DAILY_CAP/);
  assert.equal(t.yesCalls().length, 4);
});

test('kob_replace: with no band reference, or no current state in the plan, only a move for the maker passes (E-M4)', async () => {
  const { t, svc, id } = await restingSell();
  const chain = JSON.parse(readFileSync(process.env.FAKE_KOB_CHAIN, 'utf8'));
  writeFileSync(process.env.FAKE_KOB_CHAIN, JSON.stringify({ ...chain, book: { best_bid: null, best_ask: '25000000', last_fill: null } }));
  assert.match((await svc.write('kob_replace', { id, price: '0.24KAS' })).reason, /^PRICE_BAND_UNCHECKED: no last fill/);
  assert.equal((await svc.write('kob_replace', { id, price: '0.3KAS' })).status, 'submitted');
  writeFileSync(process.env.FAKE_KOB_CHAIN, JSON.stringify({ ...JSON.parse(readFileSync(process.env.FAKE_KOB_CHAIN, 'utf8')), book: chain.book }));
  // An older CLI's plan has no current state: any new price counts as a move against the maker.
  svc.kob.env.FAKE_KOB_NO_CURRENT = '1';
  assert.equal((await svc.write('kob_replace', { id, price: '0.31KAS' })).status, 'submitted');
  assert.deepEqual(t.ledger().at(-2).tokens, { [TUSD]: String(200e8) });
});

test('the ledger skips a torn line and the next entry starts a new line (MCP-5)', async () => {
  const t = setup({ mode: 'auto' });
  const svc = new KobService(t.cfg);
  writeFileSync(svc.ledger.file, '{"ts":"2026-11-17T00:00:00.000Z","kind":"rese');
  assert.equal((await svc.write('kob_place_limit', place)).status, 'submitted');
  assert.equal((await svc.status()).today.spentKas, '1.5005');
  assert.deepEqual(svc.ledger.entries().map((e) => e.kind), ['reserve', 'settle']);
});

test('a node that rejects the submit keeps the reservation: it may have relayed it (E-L3)', async () => {
  const t = setup({ mode: 'auto' });
  process.env.FAKE_KOB_SUBMIT_FAIL = '1';
  try {
    const svc = new KobService(t.cfg);
    const r = await svc.write('kob_place_limit', place);
    assert.equal(r.status, 'failed');
    assert.equal(r.counted, true);
    assert.match(r.note, /outcome is unknown/);
    const s = await svc.status();
    assert.equal(s.today.spentKas, '1.5005');
    assert.equal(s.today.unknownOutcomes.length, 1);
    assert.equal(s.today.unknownOutcomes[0].txids.length, 1, 'the planned txid, to look up');
  } finally {
    delete process.env.FAKE_KOB_SUBMIT_FAIL;
  }
  assert.deepEqual(t.ledger().map((e) => e.kind), ['reserve', 'settle']);
  assert.equal(t.ledger()[1].outcome, 'unknown');
});

test('sentNothing: a submit error never reads as nothing sent; refusals before signing do (E-L3)', () => {
  assert.equal(sentNothing('error: submit: node rejected the request: orphan'), false);
  assert.equal(sentNothing('error: submit: socket: timed out'), false);
  assert.equal(sentNothing('error: node wss://n: node rejected the request: bad address'), true, 'a read before signing');
  assert.equal(sentNothing('error: engine validation failed (nothing submitted): x'), true);
  assert.equal(sentNothing('error: the plan has errors: nothing signed'), true);
});

test('token create: the plan counts the carrier and both fees; the confirm sends the same funding', async () => {
  const t = setup();
  const svc = new KobService(t.cfg);
  const args = { name: 'Test MCP', ticker: 'TMCP', decimals: 2, supply: '100000', fund_utxo: `${'aa'.repeat(32)}:1:5000000000` };
  const p = await svc.write('kob_token_create', args);
  assert.equal(p.status, 'plan');
  assert.equal(p.spendKas, '10.00005', 'carrier 10 KAS + 0.00003 + 0.00002 fees');
  assert.equal(p.feeKas, '0.00005');
  const s = await svc.write('kob_token_create', { ...args, confirm: p.planId });
  assert.equal(s.status, 'submitted');
  assert.deepEqual(s.txids, ['a1'.repeat(32), 'a2'.repeat(32)]);
  assert.deepEqual(s.newCovenantIds, ['d0'.repeat(32)]);
  const sent = t.yesCalls()[0];
  assert.equal(sent[sent.indexOf('--fund-utxo') + 1].startsWith(`${'aa'.repeat(32)}:1:5000000000:`), true);
});

test('token create: sell on a curve passes the sale flags; the sale options need sell curve', async () => {
  const t = setup();
  const svc = new KobService(t.cfg);
  const args = { name: 'Test MCP', ticker: 'TMCV', decimals: 2, supply: '100000', fund_utxo: `${"aa".repeat(32)}:1:5000000000`, sell: 'curve', keep_percent: 10, levels: 50, total: '500KAS' };
  const p = await svc.write('kob_token_create', args);
  assert.equal(p.status, 'plan');
  const dry = t.calls().find((c) => c.includes('create'));
  for (const [k, v] of [['--sell', 'curve'], ['--keep', '10'], ['--levels', '50'], ['--total', '500KAS']]) assert.equal(dry[dry.indexOf(k) + 1], v, k);
  assert.match((await svc.write('kob_token_create', { ...args, sell: undefined })).reason, /^BAD_ARGUMENT: keep_percent/);
  assert.match((await svc.write('kob_token_create', { ...args, keep_percent: 1.234 })).reason, /^BAD_ARGUMENT/);
});

test('token create: lock_until locks the kept share (a curve\'s kept part, else keep_percent, default all)', async () => {
  const t = setup();
  const svc = new KobService(t.cfg);
  const base = { name: 'Test MCP', ticker: 'TMCL', decimals: 0, supply: '1000', fund_utxo: `${"aa".repeat(32)}:1:5000000000` };
  assert.equal((await svc.write('kob_token_create', { ...base, lock_until: '+90d', keep_percent: 20 })).status, 'plan');
  const dry = t.calls().find((c) => c.includes('create'));
  for (const [k, v] of [['--lock-until', '+90d'], ['--keep', '20']]) assert.equal(dry[dry.indexOf(k) + 1], v, k);
  assert.equal((await svc.write('kob_token_create', { ...base, ticker: 'TMCM', lock_until: '2030-01-01' })).status, 'plan');
  assert.match((await svc.write('kob_token_create', { ...base, lock_until: 'next year' })).reason, /^BAD_ARGUMENT: lock_until/);
  assert.match((await svc.write('kob_token_create', { ...base, keep_percent: 20 })).reason, /^BAD_ARGUMENT: keep_percent/);
  assert.match((await svc.write('kob_token_create', { ...base, sell: 'curve', lock_until: '+90d' })).reason, /^BAD_ARGUMENT: lock_until locks the kept share/);
});

test('token create: lock_until is at most 10 years away, and a unix time is milliseconds (13 digits)', async () => {
  const t = setup();
  const svc = new KobService(t.cfg);
  const base = { name: 'Test MCP', ticker: 'TMCN', decimals: 0, supply: '1000', fund_utxo: `${"aa".repeat(32)}:1:5000000000`, keep_percent: 20 };
  assert.equal((await svc.write('kob_token_create', { ...base, lock_until: '+3650d' })).status, 'plan');
  assert.equal((await svc.write('kob_token_create', { ...base, ticker: 'TMCO', lock_until: String(Date.now() + 365 * 86_400_000) })).status, 'plan');
  const year = new Date().getUTCFullYear();
  for (const far of ['+99999d', '+3700d', '+999999h', `${year + 11}-01-01`, String(Date.now() + 3_700 * 86_400_000)]) {
    assert.match((await svc.write('kob_token_create', { ...base, lock_until: far })).reason, /^BAD_ARGUMENT: lock_until .*10 years/, far);
  }
  // microseconds or seconds, not milliseconds
  for (const unit of [String(BigInt(Date.now() + 90 * 86_400_000) * 1000n), String(Math.floor(Date.now() / 1000) + 90 * 86_400)]) {
    assert.match((await svc.write('kob_token_create', { ...base, lock_until: unit })).reason, /^BAD_ARGUMENT: lock_until/, unit);
  }
});

test('fee estimate signs nothing and stores no plan', async () => {
  const t = setup();
  const svc = new KobService(t.cfg);
  const e = await svc.write('kob_place_limit', place, { estimate: true });
  assert.equal(e.status, 'estimate');
  assert.equal(e.feeKas, '0.0005');
  assert.equal(svc.plans.size, 0);
  assert.equal(t.yesCalls().length, 0);
});

test('the key path never appears in what the tools return or log', async () => {
  const t = setup({ mode: 'auto' });
  const svc = new KobService(t.cfg);
  const leak = `cannot read ${t.cfg.keyFile}`;
  assert.equal(svc.redact(leak).includes(t.cfg.keyFile), false);
  await svc.write('kob_place_limit', place);
  await svc.write('kob_send', { asset: 'KAS', to: 'kaspatest:x', amount: '1KAS' });
  const auditText = readFileSync(svc.audit.file, 'utf8');
  assert.equal(auditText.includes(t.cfg.keyFile), false);
  assert.equal(auditText.includes(t.cfg.keyFile.replace(/\\/g, '\\\\')), false);
});

test('take: the bound refusal is read in both CLI forms (JSON issue, or stderr text when the fill cannot be built)', async () => {
  const t = setup({ mode: 'auto' });
  const svc = new KobService(t.cfg);
  const id = 'ab'.repeat(32);
  assert.match((await svc.write('kob_take', { id, token: TUSD, amount: '1TUSD', max_price: '0.2KAS' })).reason, /^CLI_REFUSED \(exit 1\): TAKE_PRICE_ABOVE_MAX /);
  process.env.FAKE_KOB_TAKE_UNBUILT = '1';
  try {
    const r = await new KobService(t.cfg).write('kob_take', { id, token: TUSD, amount: '1TUSD', max_price: '0.2KAS' });
    assert.match(r.reason, /^CLI_REFUSED \(exit 1\): TAKE_PRICE_ABOVE_MAX: error: this fill pays/);
  } finally {
    delete process.env.FAKE_KOB_TAKE_UNBUILT;
  }
  assert.equal(t.yesCalls().length, 0);
});

test('token creations are capped per UTC day (launchesPerDay)', async () => {
  const t = setup({ mode: 'auto', limits: { launchesPerDay: 1 } });
  const svc = new KobService(t.cfg);
  const args = { name: 'Test MCP', ticker: 'TMCP', decimals: 2, supply: '100000', fund_utxo: `${'aa'.repeat(32)}:1:5000000000` };
  assert.equal((await svc.write('kob_token_create', args)).status, 'submitted');
  assert.match((await svc.write('kob_token_create', { ...args, ticker: 'TMCQ' })).reason, /^LAUNCH_CAP/);
  assert.equal(t.yesCalls().length, 1);
  assert.equal((await svc.status()).today.launches, 1);
});

test('claim and delivery: refused unless the CLI help names them; offered when it does', async () => {
  const t = setup({ mode: 'auto' });
  const svc = new KobService(t.cfg);
  assert.deepEqual(await svc.detectFeatures(), { claim: false, delivery: false });
  assert.match((await svc.write('kob_claim', { id: 'ab'.repeat(32) })).reason, /^NOT_OFFERED/);
  assert.match((await svc.write('kob_place_limit', { ...place, delivery: 'collected' })).reason, /^NOT_OFFERED/);
  assert.match((await svc.status()).cli.claim, /not in this CLI build/);
  process.env.FAKE_KOB_FEATURES = 'claim,delivery';
  try {
    const svc2 = new KobService(t.cfg);
    assert.deepEqual(await svc2.detectFeatures(), { claim: true, delivery: true });
    assert.equal((await svc2.write('kob_claim', { id: 'ab'.repeat(32) })).status, 'submitted');
    assert.equal((await svc2.write('kob_place_limit', { ...place, delivery: 'collected' })).status, 'submitted');
    assert.ok(t.yesCalls().some((a) => a.includes('--delivery') && a.includes('collected')));
  } finally {
    delete process.env.FAKE_KOB_FEATURES;
  }
});

test('token outflows are capped: a share of the confirmed holding by default (MCP-2)', async () => {
  const t = setup({ mode: 'auto', limits: { perTxKas: 2, perDayKas: 4 } }); // the wallet holds 1,000 TUSD
  const svc = new KobService(t.cfg);
  const sell = (amount) => svc.write('kob_place_limit', { token: 'TUSD', side: 'sell', amount, price: '0.25KAS' });
  const huge = await sell('1000000TUSD');
  assert.match(huge.reason, /^TOKEN_PER_TX_CAP/, 'the KAS caps see only the carrier and the fee; the token cap refuses');
  assert.match((await sell('300TUSD')).reason, /^TOKEN_PER_TX_CAP/, 'above 25 % of the holding');
  assert.equal((await sell('250TUSD')).status, 'submitted');
  assert.equal((await sell('250TUSD')).status, 'submitted', '500 of the day\'s 1,000');
  assert.match((await sell('1TUSD')).reason, /^TOKEN_DAILY_CAP/);
  assert.equal(t.yesCalls().length, 2);
  // A sale into a buy order and a token send count too.
  assert.match((await svc.write('kob_take', { id: 'ab'.repeat(32), token: TUSD, amount: '1TUSD', min_price: '0.2KAS' })).reason, /^TOKEN_DAILY_CAP/);
  assert.match((await svc.write('kob_send', { asset: 'TUSD', to: PRIMARY, amount: '1TUSD' })).reason, /^TOKEN_DAILY_CAP/);
});

test('token outflows: an own cap per token; an unconfirmed holding is refused', async () => {
  const id = '55'.repeat(32);
  const t = setup({ mode: 'auto', limits: { tokens: { [id]: { perTx: '50000000000', perDay: '60000000000' } } } });
  const svc = new KobService(t.cfg);
  const sell = (amount) => svc.write('kob_place_limit', { token: 'TUSD', side: 'sell', amount, price: '0.25KAS' });
  assert.match((await sell('501TUSD')).reason, /^TOKEN_PER_TX_CAP/);
  assert.equal((await sell('500TUSD')).status, 'submitted');
  assert.match((await sell('101TUSD')).reason, /^TOKEN_DAILY_CAP/);
  const u = setup({ mode: 'auto' });
  writeFileSync(process.env.FAKE_KOB_CHAIN, JSON.stringify({ n: 0, kasFree: 1000e8, tusdFree: 1000e8, tusdVerified: false, orders: {}, book: { best_bid: '24000000', best_ask: '26000000', last_fill: '25000000' } }));
  assert.match((await new KobService(u.cfg).write('kob_place_limit', { token: 'TUSD', side: 'sell', amount: '1TUSD', price: '0.25KAS' })).reason, /^TOKEN_CAP: the wallet's holding/);
  assert.equal(u.yesCalls().length, 0);
});

test('take: the token is required and pinned (sec-ops M15)', async () => {
  const t = setup({ mode: 'auto' });
  const svc = new KobService(t.cfg);
  const id = 'ab'.repeat(32);
  assert.match((await svc.write('kob_take', { id, amount: '1TUSD', max_price: '0.27KAS' })).reason, /^BAD_ARGUMENT: token/);
  assert.match((await svc.write('kob_take', { id, token: 'TUSD', amount: '1TUSD', max_price: '0.27KAS' })).reason, /^BAD_ARGUMENT: token/);
  assert.match((await svc.write('kob_take', { id, token: '66'.repeat(32), amount: '1TUSD', max_price: '0.27KAS' })).reason, /^CLI_REFUSED.*another token/);
  assert.equal((await svc.write('kob_take', { id, token: TUSD, amount: '1TUSD', max_price: '0.27KAS' })).status, 'submitted');
  const sent = t.yesCalls()[0];
  assert.equal(sent[sent.indexOf('--token') + 1], TUSD);
  assert.ok(t.calls().some((a) => a[0] === 'take' && a.includes('--dry-run') && a.includes('--token')));
});

const chainBook = (book) => {
  const chain = JSON.parse(readFileSync(process.env.FAKE_KOB_CHAIN, 'utf8'));
  writeFileSync(process.env.FAKE_KOB_CHAIN, JSON.stringify({ ...chain, book: { ...chain.book, ...book } }));
};
const chainSet = (over) => {
  const chain = JSON.parse(readFileSync(process.env.FAKE_KOB_CHAIN, 'utf8'));
  writeFileSync(process.env.FAKE_KOB_CHAIN, JSON.stringify({ ...chain, ...over }));
};

test('kob_replace: a tip-only amend is banded on the all-in price and its tip is capped (T-1)', async () => {
  const { t, svc, id } = await restingSell();
  // 0.25 - 0.2 tip = 0.05 KAS all-in to the maker: far below the reference.
  const r = await svc.write('kob_replace', { id, tip: '0.2KAS' });
  assert.match(r.reason, /^PRICE_BAND: a sell at 0\.05 KAS/);
  // Within the band the tip caps still hold: 2 % of the price is above the 1 % default.
  assert.match((await svc.write('kob_replace', { id, tip: '0.005KAS' })).reason, /^TIP_CAP: .* above 1% of the price/);
  assert.equal(t.yesCalls().length, 1, 'only the placement was signed');
  // A small tip passes, and the custody counts as a new sell (the all-in price went down).
  const ok = await svc.write('kob_replace', { id, tip: '0.001KAS' });
  assert.equal(ok.status, 'submitted');
  assert.deepEqual(t.ledger().at(-2).tokens, { [TUSD]: String(200e8) });
});

test('kob_replace: with the band off, a tip-only amend still meets the tip caps (T-1)', async () => {
  const t = setup({ mode: 'auto', limits: { priceBandPct: null } });
  const svc = new KobService(t.cfg);
  const s = await svc.write('kob_place_limit', { token: 'TUSD', side: 'sell', amount: '200TUSD', price: '0.25KAS' });
  const r = await svc.write('kob_replace', { id: s.newCovenantIds[0], tip: '0.2499KAS' });
  assert.match(r.reason, /TIP_CAP/);
  assert.equal(t.yesCalls().length, 1);
});

test('kob_place_limit: the tip is capped and the band judges the all-in price (T-1, T-9)', async () => {
  const t = setup({ mode: 'auto' });
  const svc = new KobService(t.cfg);
  // 0.3 + 0.02 tip = 0.32 a token: above +25 % of the 0.25 last fill.
  assert.match((await svc.write('kob_place_limit', { ...place, price: '0.3KAS', tip: '0.02KAS' })).reason, /^PRICE_BAND: a buy at 0\.32 KAS/);
  assert.match((await svc.write('kob_place_limit', { ...place, tip: '0.01KAS' })).reason, /^TIP_CAP: .* above 1% of the price/);
  const sell = { token: 'TUSD', side: 'sell', amount: '200TUSD', price: '0.25KAS', tip: '0.0025KAS' };
  assert.equal((await svc.write('kob_place_limit', sell)).status, 'submitted', '0.5 KAS of tips over 200 TUSD');
  const t2 = setup({ mode: 'auto', limits: { maxTipKas: 0.1 } });
  assert.match((await new KobService(t2.cfg).write('kob_place_limit', sell)).reason, /^TIP_CAP: the order's tips come to 0\.5 KAS/);
});

test('kob_replace: a buy priced up counts its whole escrow under the KAS caps (T-3)', async () => {
  const t = setup({ mode: 'auto' }); // 100 KAS per transaction, 150 per day
  const svc = new KobService(t.cfg);
  const b = await svc.write('kob_place_limit', { ...place, amount: '200TUSD' }); // 50 escrow + 1 carrier
  const id = b.newCovenantIds[0];
  const up = await svc.write('kob_replace', { id, price: '0.26KAS' });
  assert.equal(up.status, 'submitted');
  assert.equal(up.spendKas, '51', 'the escrow and its carrier, at the new price');
  assert.equal(up.spentTodayKas, '102.0005', 'the commitment stays counted after the send');
  assert.match((await svc.write('kob_replace', { id, price: '0.27KAS' })).reason, /^DAILY_CAP/);
  // A buy priced down moves nothing against the maker: only the fee.
  assert.equal((await svc.write('kob_replace', { id, price: '0.25KAS' })).status, 'submitted');
  assert.equal(t.yesCalls().length, 3);
});

test('price band: a printed last fill alone does not move the reference (T-3, K-L3)', async () => {
  const t = setup({ mode: 'auto' });
  const svc = new KobService(t.cfg);
  await svc.balances(); // the stand-in writes its chain
  // A second key prints a fill at 0.4; the bids that add up to 10 KAS stand at 0.24, the ask at 0.26.
  chainBook({ last_fill: '40000000', bids: [{ price: '24000000', amount: String(50e8), orders: 1 }], asks: [{ price: '26000000', amount: String(1e8), orders: 1 }] });
  const r = await svc.write('kob_place_limit', { ...place, price: '0.45KAS' });
  assert.match(r.reason, /^PRICE_BAND: a buy at 0\.45 KAS .*0\.25 KAS per whole token \(10 KAS bid depth\)/);
  // A sell is judged against the higher one: a printed low fill does not let it sell cheap.
  chainBook({ last_fill: '1000000' });
  assert.match((await svc.write('kob_place_limit', { token: 'TUSD', side: 'sell', amount: '1TUSD', price: '0.1KAS' })).reason, /^PRICE_BAND: a sell/);
  assert.equal(t.yesCalls().length, 0);
});

test('kob_claim with sell_price: banded, and the claimed tokens count under the token caps (T-2, M17)', async () => {
  const t = setup({ mode: 'auto' });
  process.env.FAKE_KOB_FEATURES = 'claim';
  try {
    const svc = new KobService(t.cfg);
    await svc.detectFeatures();
    const id = 'ab'.repeat(32);
    svc.indexer.get = async () => ({ covenant_id: id, token: TUSD });
    assert.match((await svc.write('kob_claim', { id, sell_price: '0.01KAS' })).reason, /^PRICE_BAND: a sell at 0\.01 KAS/);
    // 300 claimed TUSD for sale: above 25 % of the 1,000 held.
    chainSet({ bought: 300e8 });
    assert.match((await svc.write('kob_claim', { id, sell_price: '0.25KAS' })).reason, /^TOKEN_PER_TX_CAP/);
    chainSet({ bought: 200e8 });
    assert.equal((await svc.write('kob_claim', { id, sell_price: '0.25KAS' })).status, 'submitted');
    assert.deepEqual(t.ledger().at(-2).tokens, { [TUSD]: String(200e8) });
    // No indexer view of the order: the token is not confirmed, so the sale is refused; a plain claim goes.
    svc.indexer.get = async () => { throw new Error('down'); };
    assert.match((await svc.write('kob_claim', { id, sell_price: '0.25KAS' })).reason, /^PRICE_BAND_UNCHECKED: the token of the claimed order/);
    assert.equal((await svc.write('kob_claim', { id })).status, 'submitted');
    assert.equal(t.yesCalls().length, 2);
  } finally {
    delete process.env.FAKE_KOB_FEATURES;
  }
});

test('a STOP file written during the dry run stops the send (L23)', async () => {
  const t = setup({ mode: 'auto' });
  const svc = new KobService(t.cfg);
  svc.kob.env.FAKE_KOB_STOP_ON_DRY = t.cfg.killSwitchFile;
  const r = await svc.write('kob_place_limit', place);
  assert.match(r.reason, /^KILL_SWITCH/);
  assert.equal(t.yesCalls().length, 0);
  assert.equal(t.ledger().length, 0, 'nothing reserved');
});

test('a book that cannot be read refuses place and take instead of skipping the band (L23)', async () => {
  const t = setup({ mode: 'auto' });
  const svc = new KobService(t.cfg);
  svc.kob.env.FAKE_KOB_BOOK_FAIL = '1';
  assert.match((await svc.write('kob_place_limit', place)).reason, /^PRICE_BAND_UNCHECKED: the book/);
  assert.match((await svc.write('kob_take', { id: 'ab'.repeat(32), token: TUSD, amount: '1TUSD', max_price: '0.27KAS' })).reason, /^PRICE_BAND_UNCHECKED: the book/);
  assert.equal(t.yesCalls().length, 0);
  // With the band off no book is read.
  const u = setup({ mode: 'auto', limits: { priceBandPct: null } });
  const off = new KobService(u.cfg);
  off.kob.env.FAKE_KOB_BOOK_FAIL = '1';
  assert.equal((await off.write('kob_place_limit', place)).status, 'submitted');
});

test('fee estimate: enum arguments are checked, and a creation estimate leaves no files (L23)', async () => {
  const t = setup();
  const svc = new KobService(t.cfg);
  assert.match((await svc.write('kob_place_limit', { ...place, side: '--yes' }, { estimate: true })).reason, /^BAD_ARGUMENT: side/);
  assert.match((await svc.write('kob_place_limit', { ...place, crossing: '--yes' }, { estimate: true })).reason, /^BAD_ARGUMENT: crossing/);
  assert.match((await svc.write('kob_place_limit', { ...place, decimals: '--yes' }, { estimate: true })).reason, /^BAD_ARGUMENT: decimals/);
  const args = { name: 'Test MCP', ticker: 'TMCP', decimals: 2, supply: '100000', fund_utxo: `${'aa'.repeat(32)}:1:5000000000` };
  assert.match((await svc.write('kob_token_create', { ...args, program: '--yes' }, { estimate: true })).reason, /^BAD_ARGUMENT: program/);
  assert.equal((await svc.write('kob_token_create', args, { estimate: true })).status, 'estimate');
  const dir = join(t.cfg.dataDir, 'creations');
  assert.deepEqual(existsSync(dir) ? readdirSync(dir) : [], []);
  assert.equal(t.yesCalls().length, 0);
});
