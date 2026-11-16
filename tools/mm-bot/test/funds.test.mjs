// The shortfall math, the deposit messages and when they are sent.
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { alertsToSend, fundChecks, fundLevel, ladderNeeds, shortfall } from '../src/funds.mjs';
import { sizedQuotes } from '../src/sizing.mjs';

const ADDR = 'kaspatest:qzsanlgngufgqyddqqmtqau5n43jwn2a8w9vsdghwmpse9z77tp0kmzydclw7';
const TOKEN = { id: 'cc9957fe0d485f79994ed583aa7e16bacf194d9b9d6d77aeedc129349ddad12c', label: 'kzUSDC' };
const RATES = { tokenUsd: 1, kasUsd: 0.025 };
const FAIR = 40; // 1 / 0.025
// 1000 USD capital, 3 levels of 15 % (150 USD = 150 tokens), sides capped at 50 %
const Q = sizedQuotes(1000, { levels: 3, spreadPct: 1.0, levelStepPct: 0.5, levelSizePct: 15, maxAskPct: 50, maxBidPct: 50, kasReserve: 50 }, RATES.tokenUsd, RATES.kasUsd);
const base = { fair: FAIR, q: Q, capitalUsd: 1000, warnBelowPct: 12.5, rates: RATES, address: ADDR, token: TOKEN, bidsQuoting: true, asksQuoting: true };

test('what the full ladder needs', () => {
  const n = ladderNeeds(FAIR, Q);
  // bids: 150 tokens at 40 x (0.995, 0.99, 0.985) = 5970 + 5940 + 5910 = 17,820 KAS (under the 50 % cap of 20,000 KAS)
  assert.ok(Math.abs(n.bidKas - 17_820) < 1e-6);
  assert.ok(Math.abs(n.askTokens - 450) < 1e-9);
  assert.equal(n.reserveKas, 50);
  // the cap binds when the levels add up to more than the side may hold
  const capped = ladderNeeds(FAIR, { ...Q, levels: 5 });
  assert.equal(capped.askTokens, 500);
  assert.equal(capped.bidKas, 20_000);
});

test('shortfall and level', () => {
  assert.equal(shortfall(1240, 5000), 3760);
  assert.equal(shortfall(6000, 5000), 0);
  assert.equal(fundLevel(10, 125, false), 'idle');
  assert.equal(fundLevel(100, 125, true), 'warn');
  assert.equal(fundLevel(125, 125, true), 'ok');
});

test('an idle bid side says which asset, held, needed in % / USD / units, the shortfall and the address', () => {
  const c = fundChecks({ ...base, kasHeld: 1240, kasFree: 1240, tokenHeld: 450, bidsQuoting: false });
  const bids = c.find((x) => x.key === 'bids');
  assert.equal(bids.level, 'idle');
  assert.equal(bids.asset, 'KAS');
  assert.ok(Math.abs(bids.needed - 17_870) < 1e-6);
  assert.ok(Math.abs(bids.shortfall - 16_630) < 1e-6);
  assert.equal(bids.message, `Insufficient KAS: bids need 44.6% = 446 USD = 17,820 KAS plus a 50 KAS reserve, 1,240 KAS (3.1%) held; deposit 16,630 KAS to ${ADDR} to resume bids.`);
  assert.equal(c.find((x) => x.key === 'asks').level, 'ok');
});

test('a low token side warns before it runs out, naming the token by ticker and covenant id', () => {
  const c = fundChecks({ ...base, kasHeld: 20_000, kasFree: 200, tokenHeld: 100 });
  const asks = c.find((x) => x.key === 'asks');
  assert.equal(asks.level, 'warn', '100 USD is under 12.5 % of 1000 USD');
  assert.equal(asks.shortfall, 350);
  assert.equal(asks.message, `Low kzUSDC (${TOKEN.id}): asks need 45% = 450 USD = 450 kzUSDC, 100 kzUSDC (10%) held; deposit 350 kzUSDC to ${ADDR} to quote the full ask ladder.`);
  assert.equal(fundChecks({ ...base, kasHeld: 20_000, kasFree: 200, tokenHeld: 130 }).find((x) => x.key === 'asks').level, 'ok');
});

test('the fee reserve warns when free KAS is under it', () => {
  const c = fundChecks({ ...base, kasHeld: 17_900, kasFree: 12.5, tokenHeld: 450 });
  const fees = c.find((x) => x.key === 'fees');
  assert.equal(fees.level, 'warn');
  assert.equal(fees.shortfall, 37.5);
  assert.match(fees.message, /^Low KAS for fees and carriers: 12\.5 KAS free of a 50 KAS reserve; deposit 37\.5 KAS to kaspatest:/);
});

test('alerts: once per change, a reminder every remindMin, and "funds received" on recovery', () => {
  const mem = {};
  const idle = fundChecks({ ...base, kasHeld: 1240, kasFree: 1240, tokenHeld: 450, bidsQuoting: false });
  const t = 1_000_000;
  let a = alertsToSend(idle, mem, t, 60);
  assert.deepEqual(a.map((x) => `${x.key}/${x.level}`), ['bids/idle']);
  assert.deepEqual(alertsToSend(idle, mem, t + 30_000, 60), [], 'not every cycle');
  a = alertsToSend(idle, mem, t + 3_600_000, 60);
  assert.equal(a.length, 1);
  assert.match(a[0].message, /^Reminder: Insufficient KAS/);
  const ok = fundChecks({ ...base, kasHeld: 20_000, kasFree: 2270, tokenHeld: 450 });
  a = alertsToSend(ok, mem, t + 3_700_000, 60);
  assert.deepEqual(a.map((x) => [x.key, x.level, x.message]), [['bids', 'info', 'Funds received, bid quoting resumed.']]);
  assert.deepEqual(alertsToSend(ok, mem, t + 3_800_000, 60), []);
});
