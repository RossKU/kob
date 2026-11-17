import assert from 'node:assert/strict';
import { readFileSync, writeFileSync } from 'node:fs';
import { test } from 'node:test';
import { KobService } from '../src/service.mjs';
import { PRIMARY, setup } from './helpers.mjs';

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
  // a new server on the same data directory sees what was spent
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
  assert.match((await svc.write('kob_take', { id, amount: '1TUSD' })).reason, /^PRICE_BOUND_REQUIRED/);
  assert.match((await svc.write('kob_take', { id, amount: '1TUSD', max_price: '0.3KAS', min_price: '0.2KAS' })).reason, /^PRICE_BOUND_REQUIRED/);
  const r = await svc.write('kob_take', { id, amount: '1TUSD', max_price: '0.2KAS' });
  assert.match(r.reason, /TAKE_PRICE_ABOVE_MAX/);
  assert.equal(t.yesCalls().length, 0);
  const ok = await svc.write('kob_take', { id, amount: '1TUSD', max_price: '0.27KAS' });
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

test('a failed submission releases its reservation', async () => {
  const t = setup({ mode: 'auto' });
  process.env.FAKE_KOB_SUBMIT_FAIL = '1';
  try {
    const r = await new KobService(t.cfg).write('kob_place_limit', place);
    assert.equal(r.status, 'failed');
    assert.equal(r.counted, false);
  } finally {
    delete process.env.FAKE_KOB_SUBMIT_FAIL;
  }
  assert.deepEqual(t.ledger().map((e) => e.kind), ['reserve', 'release']);
  assert.equal(new KobService(t.cfg).ledger.spentOn(t.ledger()[0].day), 0n);
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
  assert.match((await svc.write('kob_take', { id, amount: '1TUSD', max_price: '0.2KAS' })).reason, /^CLI_REFUSED \(exit 1\): TAKE_PRICE_ABOVE_MAX /);
  process.env.FAKE_KOB_TAKE_UNBUILT = '1';
  try {
    const r = await new KobService(t.cfg).write('kob_take', { id, amount: '1TUSD', max_price: '0.2KAS' });
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
