// Scenarios of the Create token features: a creator share lock
// (KCC20 OWNER_P2SH + CLTV, declared as `lock` in the KCC-23 declaration) and its Unlock once the
// lock time passed; a sell curve created in the token's genesis (shape Linear and shape Chromatic,
// unbound), a crossing Collected buy that an executor fills against the curve, the Claim of what
// the buy collected, and the cancels. Every wallet-signed transaction is checked on the node as in
// scenarios.mjs. Nothing runs on import.
//
// The lock UI offers whole days or a date (the start of that day in the viewer's time zone). A lock
// of a few minutes is a date in a time zone whose next midnight is a few minutes away: the app
// page's zone is set with the DevTools protocol (Emulation.setTimezoneOverride) while the creation
// is planned and signed; the lock time itself is absolute (unix ms in the script and the
// declaration).
import { collapse, log, shot, sleep } from './common.mjs';
import { connectWallet, goto, openApp, outputsOf, runFlow, signOnConfirmScreen } from './driver.mjs';
import {
  SUPPLY, Skip, TOKEN, balanceRow, bodyText, cancelStep, clickRefresh, num, openOrders, place, popupView, readRecords, rowView, scenario, stepOf,
  sweepOpenOrders, trackedFrom, verifyCreated, verifySpent, waitBalance, waitLive,
} from './scenarios.mjs';

const tid = (h, id) => h.page.getByTestId(id);
const short = (s) => (s ? `${s.slice(0, 10)}...` : '-');
/** The Unlock button shows this long after the lock time (ui/orders/unlock.ts UNLOCK_MARGIN_MS). */
const UNLOCK_MARGIN_MS = 90_000;
/**
 * The lock is at least this far from the moment the creation is planned (the two approvals and the
 * review must fit).
 */
const LOCK_LEAD_MS = 6 * 60_000;

/**
 * `rc` with another token as "token A" (openTicket / place / cancelStep read `rc.token`);
 * everything else, `h` included, from `rc`.
 */
const withToken = (rc, token) => override(rc, { token });

/**
 * An object that reads `rc` but has `props` as its own (defined, never assigned: an inherited
 * setter would write through to `rc`).
 */
function override(rc, props) {
  const o = Object.create(rc);
  for (const [k, v] of Object.entries(props)) Object.defineProperty(o, k, { value: v, writable: true, enumerable: true, configurable: true });
  return o;
}

// ------------------------------------------------------------------------------------------------
// time zone of the lock

/**
 * A time zone whose next local midnight is `leadMs` from now (rounded up to the whole minute): a
 * custom ICU offset zone `GMT+hh:mm` (Chromium takes it in Emulation.setTimezoneOverride), with the
 * date input's value (that day) and the instant (unix ms). The named zones have offsets in 30 / 45
 * minute steps at best, which leaves up to an hour to wait. The page computes the instant itself
 * (`new Date(y, m - 1, d)` in the overridden zone); this is the harness's own, compared with it.
 */
export function pickLockZone(leadMs = LOCK_LEAD_MS, now = Date.now()) {
  const until = Math.ceil((now + leadMs) / 60_000) * 60_000;
  // local = utc + offset: midnight at `until` needs offset = -(minutes of the UTC day at `until`),
  // taken into [-12:00, +12:00)
  let off = -Math.round((until % 86_400_000) / 60_000);
  off = ((off % 1440) + 1440) % 1440;
  if (off >= 720) off -= 1440;
  const a = Math.abs(off);
  const zone = `GMT${off < 0 ? '-' : '+'}${String(Math.floor(a / 60)).padStart(2, '0')}:${String(a % 60).padStart(2, '0')}`;
  const date = new Date(until + off * 60_000).toISOString().slice(0, 10);
  return { zone, offsetMinutes: off, date, until, msToMidnight: until - now };
}

/**
 * The script-number push of `n` as hex (little endian, minimal, a 0x00 when the top bit is set):
 * how a CLTV lock time sits in the script.
 */
function scriptNumHex(n) {
  const bytes = [];
  let v = BigInt(n);
  while (v > 0n) {
    bytes.push(Number(v & 0xffn));
    v >>= 8n;
  }
  if (bytes.length && bytes[bytes.length - 1] & 0x80) bytes.push(0);
  return bytes.map((b) => b.toString(16).padStart(2, '0')).join('');
}

async function setZone(h, zone) {
  h.cdp ??= await h.page.context().newCDPSession(h.page);
  await h.cdp.send('Emulation.setTimezoneOverride', { timezoneId: zone ?? '' });
}

/**
 * Moves the page's `Date.now` by `offsetMs` (the real clock is kept as `__kobRealNow`); 0 puts it
 * back. A reload ends it.
 */
const shiftClock = (page, offsetMs) =>
  page.evaluate((off) => {
    const real = (window.__kobRealNow ??= Date.now.bind(Date));
    Date.now = off ? () => real() + off : real;
  }, offsetMs);

// ------------------------------------------------------------------------------------------------
// Create token with a sale / lock

const parseKas = (s) => Number(String(s ?? '').replace(/[^0-9.]/g, ''));

/**
 * One creation on `#/create` with the published public-mint program: `sale` (null = keep all, or {
 * keepPct, shape: 'linear' | 'chromatic', levels, multiple, octaves, totalKas }) and `lock` (null
 * or { zone, date }). Signs on the confirmation screen (two approvals: preparation, genesis),
 * checks both on the node and the result panel, and returns what the later steps need.
 */
async function createToken(rc, r, label, { sale = null, lock = null, expectConfirm = [] } = {}) {
  const { h } = rc;
  const t0 = Date.now();
  const ticker = `R${label[0].toUpperCase()}${{ kasware: 'KW', kaspire: 'KP', kastle: 'KS' }[rc.name] ?? 'XX'}${Math.random().toString(36).slice(2, 6).toUpperCase().replace(/[OIL0-9]/g, 'X')}`.slice(0, 8);
  const recordsBefore = new Set(Object.keys(await readRecords(h)));
  await goto(h, '#/create');
  if (await tid(h, 'issue-another').count()) await tid(h, 'issue-another').click();
  await tid(h, 'issue-form').waitFor({ timeout: 60_000 });
  await tid(h, 'issue-advanced').getByRole('button').first().click();
  if ((await tid(h, 'issue-program-public-mint').getAttribute('aria-checked')) !== 'true') await tid(h, 'issue-program-public-mint').click();
  await tid(h, 'issue-name').fill(`KOB real ${rc.name} ${label}`);
  await tid(h, 'issue-ticker').fill(ticker);
  await tid(h, 'issue-decimals').fill('8');
  await tid(h, 'issue-supply').fill(String(SUPPLY));
  // 1 KAS per token output (Advanced > Carrier; the TN10 default is 10 KAS, which every token UTXO
  // keeps)
  await tid(h, 'issue-carrier').fill('1');
  const form = {};
  if (sale) {
    await tid(h, 'issue-after-curve').click();
    await tid(h, 'sale-keep').fill(String(sale.keepPct));
    if (sale.shape === 'chromatic') {
      await tid(h, 'sale-shape-chromatic').click();
      await tid(h, 'sale-octaves').fill(String(sale.octaves));
    } else {
      await tid(h, 'sale-shape-linear').click();
      await tid(h, 'sale-levels').fill(String(sale.levels));
      await tid(h, 'sale-multiple').fill(String(sale.multiple));
    }
    await tid(h, 'sale-total').fill(String(sale.totalKas));
    await tid(h, 'sale-graph').waitFor({ timeout: 30_000 });
    await sleep(800);
    Object.assign(form, await h.page.evaluate(() => {
      const t = (id) => (document.querySelector(`[data-testid="${id}"]`)?.textContent ?? '').replace(/\s+/g, ' ').trim();
      return {
        split: t('sale-split'), first: t('sale-first'), last: t('sale-last'), average: t('sale-average'), realised: t('sale-realised'),
        octavesRange: t('sale-octaves-range'), graphSteps: document.querySelector('[data-testid="sale-graph"]')?.getAttribute('data-steps') ?? null,
        curveError: t('sale-curve-error'),
      };
    }));
    if (form.curveError) throw new Error(`the curve form says: ${form.curveError}`);
  }
  if (lock) {
    // the page's zone: the date chosen starts a few minutes from now there (set until the creation
    // is signed: the plan re-reads the date)
    await setZone(h, lock.zone);
    await tid(h, 'issue-lock-choice').selectOption('date');
    await tid(h, 'issue-lock-date').fill(lock.date);
    await tid(h, 'issue-lock-until').waitFor({ timeout: 15_000 });
    form.lockUntilShown = collapse(await tid(h, 'issue-lock-until').innerText());
    form.lockUntilPage = await h.page.evaluate((d) => { const [y, m, dd] = d.split('-').map(Number); return new Date(y, m - 1, dd).getTime(); }, lock.date);
    if (form.lockUntilPage !== lock.until) throw new Error(`the page reads the lock date ${lock.date} in ${lock.zone} as ${form.lockUntilPage}, the harness as ${lock.until}`);
    const err = await tid(h, 'issue-lock-error').innerText().catch(() => '');
    if (err) throw new Error(`the lock field says: ${err}`);
  }
  const review = tid(h, 'issue-review');
  for (let i = 0; i < 60 && !(await review.isEnabled()); i++) await sleep(1000);
  if (!(await review.isEnabled())) throw new Error(`issue review stays disabled: ${collapse(await tid(h, 'issue-view').innerText().catch(() => bodyText(h))).slice(0, 900)}`);
  form.summary = collapse(await tid(h, 'issue-summary').innerText().catch(() => ''));
  const formShot = await shot(h.page, rc.name, `${label}-form`);
  await review.click();
  let s;
  const submitsBefore = h.submits.length;
  let notSent = null;
  try {
    try {
      s = await signOnConfirmScreen(h, rc.ctx, rc.drv, `${label}-create`, rc.name);
    } catch (e) {
      // the genesis was not sent (the wallet's node did not list the preparation's output yet:
      // "Nothing was sent; try again"): the page offers "Finish the creation" (issue-pending-send),
      // which asks for the genesis again (wallet/visibility.ts, README)
      if (!/Nothing was sent|not sent/i.test(String(e?.message))) throw e;
      notSent = { error: String(e.message).slice(0, 800), screenshot: await shot(h.page, rc.name, `${label}-not-sent`) };
      await tid(h, 'confirm-close').click().catch(() => {});
      s = await finishCreation(rc, label);
      s.finished = notSent;
    }
  } finally {
    if (lock) await setZone(h, null).catch(() => {});
  }
  const step = { ...stepOf(`create ${label} (${ticker})`, s), form, confirmText: collapse(s.confirmText ?? '').slice(0, 3000), genesisNotSentFirst: notSent ?? undefined };
  step.screenshots.unshift(formShot);
  r.steps.push(step);
  // (a creation finished later has no confirmation text of its own: its first attempt's screen was
  // the one read)
  if (!notSent) for (const re of expectConfirm) if (!re.test(step.confirmText)) throw new Error(`the confirmation screen does not say ${re}: ${step.confirmText.slice(0, 900)}`);
  if (!s.submitted) throw new Error('the submitted genesis was not captured from the node connection');
  // the preparation: the first submit of this creation with a KCC-23 payload (0x00 + JSON); the
  // genesis is the last one
  const prep = h.submits.slice(submitsBefore).find((x) => typeof x.tx?.payload === 'string' && x.tx.payload.startsWith('00'))?.tx ?? null;
  const declared = typeof prep?.payload === 'string' && prep.payload.startsWith('00') ? Buffer.from(prep.payload.slice(2), 'hex').toString('utf8') : null;
  const spent = s.submitted.inputs?.[0]?.transactionId ?? s.submitted.inputs?.[0]?.previousOutpoint?.transactionId;
  if (!declared || s.submitted.inputs?.length !== 1 || (prep.id && spent !== prep.id)) throw new Error(`the genesis does not spend a KCC-23 preparation sent before it (prep payload ${String(prep?.payload).slice(0, 40)})`);
  if (!declared.includes(`"symbol":"${ticker}"`)) throw new Error(`the preparation declares ${declared.slice(0, 400)}, not ${ticker}`);
  step.declaration = declared;
  step.verify = await verifyCreated(rc, s.submitted, s.txid, `genesis ${label}`);
  await tid(h, 'issue-result').waitFor({ timeout: 60_000 });
  const tokenId = await tid(h, 'issue-result-tokenId').getAttribute('data-value');
  const registryEntry = JSON.parse(await tid(h, 'issue-registry-entry').innerText());
  if (registryEntry.covenant_id !== tokenId) throw new Error('registry entry covenant id differs from the shown token id');
  step.result = await h.page.evaluate(() => {
    const t = (id) => (document.querySelector(`[data-testid="${id}"]`)?.textContent ?? '').replace(/\s+/g, ' ').trim();
    return { holders: t('issue-result-holders'), curve: t('issue-result-curve') };
  });
  step.screenshots.push(await shot(h.page, rc.name, `${label}-result`));
  await sleep(1500);
  const after = await readRecords(h);
  const records = Object.entries(after).filter(([id, rec]) => !recordsBefore.has(id) && rec.txid === s.txid).map(([id, rec]) => ({ covenantId: id, kind: rec.kind, output: rec.output }));
  step.records = records;
  log(`[${rc.name}] created ${label} ${ticker} token ${short(tokenId)} in ${Date.now() - t0} ms, genesis ${short(s.txid)}`);
  const token = { ticker, tokenId, registryEntry, issueTxid: s.txid, supply: String(SUPPLY) };
  return { token, genesisTxid: s.txid, genesisTx: s.submitted, declaration: declared, records, form, step };
}

/**
 * "Finish the creation" after a genesis that was not sent: one approval (the preparation is on the
 * node), the result panel.
 */
async function finishCreation(rc, label) {
  const { h } = rc;
  const t0 = Date.now();
  const btn = tid(h, 'issue-pending-send');
  await btn.waitFor({ timeout: 60_000 });
  await sleep(5000);
  const before = h.submits.length;
  const { popups } = await rc.drv.approve(rc.ctx, async () => {
    await btn.click();
    const t1 = Date.now();
    for (;;) {
      if (await tid(h, 'issue-result').count()) return;
      const err = collapse(await tid(h, 'issue-pending-error').innerText().catch(() => ''));
      if (err) throw new Error(`Finish the creation: ${err}`);
      if (Date.now() - t1 > 240_000) throw new Error('Finish the creation: no result after 240 s');
      await sleep(500);
    }
  });
  const txid = await tid(h, 'issue-result-txid').getAttribute('data-value');
  const submitted = h.submits.slice(before).filter((x) => x.tx).at(-1)?.tx ?? null;
  const screenshots = [await shot(h.page, rc.name, `${label}-finished`)];
  log(`[${rc.name}] ${label}: finished the creation, genesis ${short(txid)}`);
  return { txid, kind: null, appStatus: null, summary: '', shownAs: 'Finish the creation', acks: [], confirmText: '', popups, screenshots, totalMs: Date.now() - t0, submitted };
}

/**
 * Lists the new token in the run's test registry (and the mock indexer) and reloads the app, as the
 * issuance of token A does.
 */
async function listToken(rc, token) {
  rc.extraTokens.push(token);
  await rc.setRegistry();
}

// ------------------------------------------------------------------------------------------------
// lock

/**
 * Create token with "Lock my share until" a date a few minutes ahead (keep all: the wallet's whole
 * share is locked). Checks the declaration's `lock` script carries the lock time, the Portfolio's
 * "locked until" line, that Unlock is not offered before the lock time (plus the margin), and on
 * TN10 that an early unlock is not accepted: the app refuses it on the node's past median time,
 * and, with that check taken out in a second page of the same wallet (features.test, the test
 * hook), the wallet-signed early unlock is refused by the node.
 */
export async function runLock(rc) {
  return scenario(rc, 'lock', async (r) => {
    const { h } = rc;
    const pick = pickLockZone();
    if (!pick) throw new Error('no time zone has a midnight in reach');
    r.steps.push({ name: 'lock time', zone: pick.zone, date: pick.date, until: pick.until, untilIso: new Date(pick.until).toISOString(), minutesAhead: Math.round(pick.msToMidnight / 600) / 100 });
    log(`[${rc.name}] lock: ${pick.date} in ${pick.zone} = ${new Date(pick.until).toISOString()} (${Math.round(pick.msToMidnight / 1000)} s ahead)`);
    const c = await createToken(rc, r, 'lock', { lock: pick, expectConfirm: [/lock/i] });
    const lockScript = (() => {
      try {
        return JSON.parse(c.declaration).lock ?? null;
      } catch {
        return null;
      }
    })();
    c.step.lockScript = lockScript;
    if (!lockScript) throw new Error(`the KCC-23 declaration has no "lock": ${c.declaration.slice(0, 500)}`);
    if (!lockScript.includes(scriptNumHex(pick.until))) throw new Error(`the declared lock script ${lockScript} does not push the lock time ${pick.until} (${scriptNumHex(pick.until)})`);
    if (!/locked/i.test(c.step.result.holders)) throw new Error(`the result panel does not show the locked holder: ${c.step.result.holders}`);
    rc.lockRun = { token: c.token, until: pick.until, genesisTxid: c.genesisTxid, genesisTx: c.genesisTx, lockScript };
    await listToken(rc, c.token);

    // Portfolio: "locked until" under Available; no Unlock before the lock time + margin
    const tokenId = c.token.tokenId;
    const view = await pollBalances(rc, () => lockedView(h, tokenId), (v) => !!v.locked, 180_000);
    if (!view.locked) throw new Error(`the Portfolio shows no "locked until" for ${short(tokenId)} after 180 s: ${JSON.stringify(view)}`);
    const early = { name: 'Portfolio before the lock time', view, msBeforeUntil: pick.until - Date.now(), screenshot: await shot(h.page, rc.name, 'lock-portfolio-locked') };
    r.steps.push(early);
    if (!/locked until/i.test(view.locked)) throw new Error(`the locked line says "${view.locked}"`);
    if (view.unlock) throw new Error(`Unlock is offered ${Math.round((pick.until - Date.now()) / 1000)} s before the lock time`);

    if (rc.result.mode === 'mock') {
      r.steps.push({ name: 'early unlock on the node', skipped: 'the mock node has no past median time and no finality check: nothing to prove offline' });
      return;
    }
    await earlyUnlock(rc, r);
  });
}

/**
 * Opens Portfolio > Balances and polls `read()` every 2 s until `pred` holds; the tab is opened
 * afresh only every 45 s (each opening reloads the balances, which take several seconds on TN10
 * with many token UTXOs: opening it every few seconds never let them finish).
 */
async function pollBalances(rc, read, pred, timeoutMs) {
  const t0 = Date.now();
  let opened = 0;
  let v = null;
  for (;;) {
    if (Date.now() - opened > 45_000) {
      await openOrders(rc, [], { tab: 'balances' });
      opened = Date.now();
    }
    v = await read();
    if (pred(v)) return v;
    if (Date.now() - t0 > timeoutMs) return v;
    await sleep(2000);
  }
}

/** The Balances row of a locked token: the "locked until" line and whether Unlock is there. */
const lockedView = (h, token) =>
  h.page.evaluate((token) => {
    const row = document.querySelector(`[data-testid="balance-row-${token}"]`);
    const t = (el) => (el?.textContent ?? '').replace(/\s+/g, ' ').trim();
    const cell = row?.querySelector('.dt-c-available');
    return {
      listed: !!row,
      available: t(cell?.querySelector('[data-value]') ?? null) || null,
      locked: t(row?.querySelector('[data-testid="balance-locked"]') ?? null) || null,
      unlock: !!document.querySelector(`[data-testid="balance-unlock-${token}"]`),
      cli: !!document.querySelector(`[data-testid="balance-unlock-cli-${token}"]`),
    };
  }, token);

/**
 * TN10 only. A second page of the same wallet with the app's test hook (features.test:
 * `window.__kob.services`) and its clock moved past the lock time: (1) Unlock shows, the press asks
 * the node's past median time and refuses ("Not open yet"); nothing is sent. (2) With the services'
 * `getPastMedianTime` taken out (the harness, not the app), the press plans the unlock: the lock
 * coin (1 KAS to the lock's address) is signed and accepted, the unlock (lock time = the lock's
 * time) is signed by the wallet and must be REFUSED by the node.
 */
async function earlyUnlock(rc, r) {
  const L = rc.lockRun;
  const h2 = await openApp(rc.ctx, rc.server.url, { ...rc.appConfig, features: { ...rc.appConfig.features, test: true } });
  try {
    await h2.page.getByTestId('wallet-address').waitFor({ timeout: 15_000 }).catch(() => {});
    if (!(await h2.page.getByTestId('wallet-address').count())) await connectWallet(h2, rc.ctx, rc.drv);
    const offset = L.until + UNLOCK_MARGIN_MS + 30_000 - Date.now();
    await shiftClock(h2.page, offset);
    const rc2 = override(rc, { token: L.token, h: h2 });
    const press = async () => {
      const v = await pollBalances(rc2, () => lockedView(h2, L.token.tokenId), (x) => x.unlock, 180_000);
      if (!v.unlock) throw new Error(`the page with its clock moved ${Math.round(offset / 1000)} s ahead offers no Unlock: ${JSON.stringify(v)}`);
      await tid(h2, `balance-unlock-${L.token.tokenId}`).click();
    };
    // (1) the app's own check against the node's clock
    const sent0 = h2.submits.length;
    await press();
    const t0 = Date.now();
    let refusal = '';
    let raw = '';
    while (Date.now() - t0 < 30_000 && !refusal) {
      refusal = collapse(await tid(h2, 'orders-action-error').innerText().catch(() => ''));
      // the raw text sits under "Details" (kit RawDetails): textContent reads it with the details
      // closed
      raw = collapse(await h2.page.locator('[data-testid="orders-action-error"] [data-testid="raw-details"]').textContent({ timeout: 2000 }).catch(() => ''));
      if (!refusal && (await tid(h2, 'flow-status').count())) break;
      await sleep(500);
    }
    const appCheck = { name: 'early unlock: the app asks the node clock', clockAheadMs: offset, refusal, raw, sent: h2.submits.length - sent0, screenshot: await shot(h2.page, rc.name, 'lock-early-app-refusal') };
    r.steps.push(appCheck);
    // the app's sentence is "Not open yet on the network ..." (orders.unlock.notOpen); the
    // Portfolio's banner shows it through the generic builder-error mapping (i18n/build-error.ts:
    // /lock ?time/ -> build.timing), its own text under Details
    if (!/Not open yet/i.test(`${refusal} ${raw}`) || appCheck.sent) throw new Error(`an early unlock was not refused by the app before signing: ${JSON.stringify(appCheck).slice(0, 600)}`);
    log(`[${rc.name}] lock: early Unlock refused by the app: "${refusal.slice(0, 120)}" (details: ${raw.slice(0, 160)})`);

    // (2) the node: the same press without the app's node-clock check, signed by the wallet
    const patched = await h2.page.evaluate(() => {
      const n = window.__kob?.services?.node;
      if (!n) return false;
      n.getPastMedianTime = async () => null;
      return true;
    });
    if (!patched) throw new Error('the test hook window.__kob.services is not there (features.test)');
    const sent1 = h2.submits.length;
    await press();
    let flowError = null;
    let flow = null;
    try {
      flow = await runFlow(h2, rc.ctx, rc.drv, 'lock-early-unlock', rc.name);
    } catch (e) {
      flowError = String(e?.message ?? e).slice(0, 1200);
    }
    // a flow whose last step the node refused still ends on "Done: n transaction(s) submitted"
    // (`flow-done`, the steps before it), the refused step marked Failed with the node's sentence:
    // read from the flow card
    const flowText = collapse(await tid(h2, 'tx-flow').innerText().catch(() => bodyText(h2)));
    const nodeSaid = /The node reported an error: ([^.]*(?:.[^ ]*)?)/i.exec(flowText)?.[1] ?? null;
    if (!flowError && nodeSaid) flowError = `the flow's unlock step failed: The node reported an error: ${nodeSaid}`;
    const subs = h2.submits.slice(sent1).filter((x) => x.tx);
    const node = { name: 'early unlock: wallet-signed, sent to the node', flowError, flowText: flowText.slice(0, 600), flowDone: flow?.done ?? null, submitted: subs.map((x) => ({ txid: x.txid ?? null, lockTime: x.tx.lockTime ?? null, inputs: x.tx.inputs?.length, outputs: x.tx.outputs?.length })), popups: popupView(flow?.steps?.[0]?.popups ?? []), screenshot: await shot(h2.page, rc.name, 'lock-early-node') };
    r.steps.push(node);
    // the lock coin (first submit, output 0 at the lock's address) is on the node; the locked
    // genesis outputs are still unspent
    const coin = subs.find((x) => String(x.tx.lockTime ?? '0') === '0');
    const unlockTry = subs.find((x) => String(x.tx.lockTime ?? '0') !== '0');
    if (coin?.txid) {
      const o = outputsOf(rc.node, coin.tx)[0];
      const w = await rc.node.waitOutpoint(o.address, coin.txid, 0, true);
      node.lockCoin = { txid: coin.txid, address: o.address, unspent: w.ok, ms: w.ms };
      L.lockCoinTxid = coin.txid;
    }
    const locked = outputsOf(rc.node, L.genesisTx).filter((o) => o.p2sh && o.address);
    node.lockedOutputs = [];
    for (const o of locked) node.lockedOutputs.push({ outpoint: `${short(L.genesisTxid)}:${o.index}`, unspent: await rc.node.isUnspent(o.address, L.genesisTxid, o.index) });
    if (!unlockTry) throw new Error(`no early unlock reached the node (the flow: ${flowError ?? flow?.done})`);
    if (!flowError) throw new Error(`the node ACCEPTED an unlock before the lock time: ${JSON.stringify(node).slice(0, 800)}`);
    if (!node.lockedOutputs.every((x) => x.unspent)) throw new Error(`a locked genesis output is spent after the early unlock: ${JSON.stringify(node.lockedOutputs)}`);
    log(`[${rc.name}] lock: early unlock refused by the node: ${flowError.slice(0, 200)}`);
  } finally {
    await h2.page.close().catch(() => {});
  }
}

/**
 * After the lock time + margin: Unlock on the Portfolio, one wallet flow, the locked genesis
 * outputs spent, the whole supply back as the wallet's.
 */
export async function runUnlock(rc) {
  return scenario(rc, 'unlock', async (r) => {
    const { h } = rc;
    const L = rc.lockRun;
    if (!L) throw new Skip('needs the lock scenario (--only lock,...,unlock)');
    const tokenId = L.token.tokenId;
    const mock = rc.result.mode === 'mock';
    const openAt = L.until + UNLOCK_MARGIN_MS + 5_000;
    if (mock) {
      // the mock node has no clock of its own: the page's clock is moved past the lock time instead
      // of waiting
      await shiftClock(h.page, Math.max(0, openAt - Date.now()));
    } else if (Date.now() < openAt) {
      log(`[${rc.name}] unlock: waiting ${Math.round((openAt - Date.now()) / 1000)} s for the lock time + margin`);
      await sleep(openAt - Date.now());
    }
    const t0 = Date.now();
    let step = null;
    for (let attempt = 1; !step; attempt++) {
      const v = await pollBalances(rc, () => lockedView(h, tokenId), (x) => x.unlock, 180_000);
      if (!v.unlock) throw new Error(`no Unlock on the Portfolio ${Math.round((Date.now() - L.until) / 1000)} s after the lock time: ${JSON.stringify(v)}`);
      const shot1 = await shot(h.page, rc.name, 'unlock-offered');
      await tid(h, `balance-unlock-${tokenId}`).click();
      try {
        const { steps, done } = await runFlow(h, rc.ctx, rc.drv, 'unlock', rc.name);
        const last = steps.at(-1);
        if (!last?.submitted) throw new Error('the unlock transaction was not captured');
        step = { ...stepOf('unlock', last), offered: v, done, transactions: steps.map((x) => x.txid), attempts: attempt, msAfterUntil: Date.now() - L.until };
        step.screenshots.unshift(shot1);
        step.lockTime = last.submitted.lockTime ?? null;
        r.steps.push(step);
        step.verify = await verifySpent(rc, last.submitted, last.txid, new Map([[L.genesisTxid, L.genesisTx]]), 'unlock');
        const got = await trackedFrom(h, last.txid, tokenId);
        step.tracked = got.items;
        if (got.total !== SUPPLY * TOKEN) throw new Error(`the unlock should give the wallet the whole supply ${SUPPLY * TOKEN}, the app tracks ${got.total}`);
      } catch (e) {
        const msg = String(e?.message ?? e);
        if (/Not open yet|time terms do not fit/i.test(msg) && Date.now() - t0 < 600_000) {
          r.steps.push({ name: `unlock attempt ${attempt}: not open yet on the node`, error: msg.slice(0, 300) });
          await sleep(20_000);
          continue;
        }
        throw e;
      }
    }
    await openOrders(rc, [], { tab: 'balances' });
    step.balancesAfter = await waitBalance(rc, tokenId, (b) => num(b.free) === Number(SUPPLY), `${L.token.ticker}: the whole supply free`);
    step.lockedAfter = await lockedView(h, tokenId);
    step.screenshots.push(await shot(h.page, rc.name, 'unlock-after'));
    if (step.lockedAfter.locked) throw new Error(`the Portfolio still shows a lock after the unlock: ${JSON.stringify(step.lockedAfter)}`);
    if (mock) await shiftClock(h.page, 0);
    log(`[${rc.name}] unlock: ${step.transactions.map(short).join(' ')} ${Math.round(step.msAfterUntil / 1000)} s after the lock time`);
  });
}

// ------------------------------------------------------------------------------------------------
// curves

/** The curve row of Open orders: type, live, its level line. */
const curveRow = async (h, id) => ({
  ...(await rowView(h, id)),
  level: await h.page.evaluate((id) => (document.querySelector(`[data-testid="order-curve-level-${id}"]`)?.textContent ?? '').replace(/\s+/g, ' ').trim(), id),
  bought: await h.page.evaluate((id) => (document.querySelector(`[data-testid="order-bought-${id}"]`)?.textContent ?? '').replace(/\s+/g, ' ').trim(), id),
});

/**
 * Creates a token with a curve in its genesis and checks the curve in Open orders (one row, type
 * curve, level 1 of N, live).
 */
async function createCurve(rc, r, label, sale, levels) {
  const c = await createToken(rc, r, label, { sale, expectConfirm: [new RegExp(`on a curve: ${levels} levels`)] });
  if (!/curve order is on the book/i.test(c.step.result.curve)) throw new Error(`the result panel does not say the curve is on the book: "${c.step.result.curve}"`);
  const curve = c.records.find((x) => x.covenantId !== c.token.tokenId);
  if (!curve) throw new Error(`the app stored no placement record of the curve (records of the genesis: ${JSON.stringify(c.records)})`);
  await listToken(rc, c.token);
  const id = curve.covenantId;
  await openOrders(rc, [id]);
  await waitLive(rc, [id], 90_000);
  const row = await curveRow(rc.h, id);
  r.steps.push({ name: `${label}: the curve in Open orders`, row, screenshot: await shot(rc.h.page, rc.name, `${label}-open-orders`) });
  if (row.type !== 'curve') throw new Error(`the curve row has type ${row.type}`);
  if (row.level !== `Level 1 / ${levels}`) throw new Error(`the curve row says "${row.level}", expected "Level 1 / ${levels}"`);
  const sell = SUPPLY - (SUPPLY * BigInt(Math.round(sale.keepPct * 100))) / 10_000n;
  const bal = await waitBalance(rc, c.token.tokenId, (b) => num(b.escrowed) === Number(sell), `${c.token.ticker}: ${sell} in orders (the curve's custody)`);
  r.steps.push({ name: `${label}: Balances with the curve`, balances: bal });
  return { ...c, curveId: id, sell };
}

/**
 * Linear curve sale in the genesis (10% kept, 10 levels, 40 KAS if sold out, last level 5x the
 * first), checked in Open orders and Balances. The app refuses a buy into the wallet's own curve
 * ("would trade against your own order"), so the buy goes to a curve of the counterparty (the DEV
 * key, in its own browser with the in-page signer, as the market scenario): the counterparty
 * creates the same curve in its genesis, the wallet places a crossing Collected limit buy of two
 * levels at 1.15x the first level's price (below the second level's), an executor on the network
 * fills one level of the curve into it (with --mock the mock fills both). The Claim of what the buy
 * collected is its own result (`claim`); then the wallet cancels the buy and its own curve, the
 * counterparty its curve.
 */
export async function runCurve(rc) {
  return scenario(rc, 'curve', async (r) => {
    const sale = { keepPct: 10, shape: 'linear', levels: 10, multiple: 5, totalKas: 40 };
    const own = await createCurve(rc, r, 'curve', sale, 10);
    rc.curveRun = { token: own.token, filled: false };
    if (!rc.openCounterparty) throw new Error('no counterparty key (DEV_PRIVATE_KEY) for the curve the wallet buys from');
    const cp = await rc.openCounterparty();
    // own properties only: `rc.h` is an accessor of the wallet's page, assigning through it would
    // replace the wallet's page
    const cpRc = override(rc, { ...cp, name: `${rc.name}-counterparty`, address: await cp.h.page.getByTestId('wallet-address').getAttribute('data-value') });
    const cpRefresh = async () => {
      await cp.h.page.reload();
      await cp.h.page.getByTestId('main').waitFor({ timeout: 60_000 }).catch(() => {});
      await cp.h.page.getByTestId('wallet-address').waitFor({ timeout: 15_000 }).catch(() => {});
      if (!(await cp.h.page.getByTestId('wallet-address').count())) await connectWallet(cp.h, cp.ctx, cp.drv);
    };
    try {
      if (rc.result.mode === 'mock') await rc.mock.giveKas('maker', 100, 4);
      // the counterparty's curve: the same terms, created in its genesis (listToken reloads the
      // wallet's page; the counterparty's is reloaded here)
      Object.defineProperty(cpRc, 'setRegistry', { value: async () => { await rc.setRegistry(); await cpRefresh(); } });
      const theirs = await createCurve(cpRc, r, 'cp-curve', sale, 10);
      const tok = theirs.token;
      const level = theirs.sell / 10n; // whole tokens per level
      const firstSompi = Math.round(parseKas(theirs.form.first) * 1e8);
      const bidSompi = Math.floor(firstSompi * 1.15);
      const price = (bidSompi / 1e8).toFixed(8).replace(/0+$/, '');
      Object.assign(rc.curveRun, { token: tok, curveId: theirs.curveId, level });
      // the crossing buy: a plain limit (fills at its limit, so an executor keeps the difference to
      // the curve's level price), Collected delivery
      const bid = await place(withToken(rc, tok), r, 'curve-buy', { type: 'limit', side: 'buy', fields: { amount: String(2n * level), price, minFill: String(level), delivery: 'collected', crossing: 'limit' }, expectSummary: /Buy limit order/ });
      const bidId = bid.covenantIds[0];
      Object.assign(rc.curveRun, { bidId, bidTxid: bid.txid, bidTx: bid.tx, price });
      log(`[${rc.name}] curve: buy ${2n * level} @ ${price} KAS (first level ${theirs.form.first}) from the counterparty's curve, placed ${short(bid.txid)}`);
      if (rc.result.mode === 'mock') {
        // no executor offline: the mock fills one level of the curve and one level into the buy
        await rc.mock.post('/mock/fill', { covenant_id: theirs.curveId, amount: String(level * TOKEN), taker: 'carol' });
        await rc.mock.post('/mock/fill', { covenant_id: bidId, amount: String(level * TOKEN), price: bidSompi, taker: 'carol' });
      }
      // wait for the fill: the buy holds what it bought (Collected)
      const t0 = Date.now();
      let views = null;
      for (;;) {
        await openOrders(rc, [bidId]).catch(() => {});
        views = { bid: await curveRow(rc.h, bidId) };
        if (views.bid.bought) break;
        if (Date.now() - t0 > 300_000) break;
        await sleep(10_000);
      }
      await openOrders(cpRc, [theirs.curveId]).catch(() => {});
      views.curve = await curveRow(cp.h, theirs.curveId);
      const fill = { name: "the wallet's buy against the counterparty's curve", views, waitedMs: Date.now() - t0, screenshot: await shot(rc.h.page, rc.name, 'curve-buy-after-fill') };
      r.steps.push(fill);
      if (rc.indexerUrl) {
        fill.indexer = {};
        for (const [k, id] of [['curve', theirs.curveId], ['bid', bidId]]) {
          const j = await fetch(`${rc.indexerUrl}/v1/orders/${id}`).then((x) => x.json()).catch(() => null);
          fill.indexer[k] = j ? { status: j.status, curve: j.curve ?? undefined, bought: j.bought ?? undefined, current: j.current ? `${j.current.txid}:${j.current.index}` : undefined } : null;
        }
      }
      rc.curveRun.filled = !!views.bid.bought;
      rc.curveRun.fillViews = views;
      log(`[${rc.name}] curve: ${rc.curveRun.filled ? `filled: curve ${views.curve.level}, buy holds "${views.bid.bought}"` : `no fill after ${Math.round(fill.waitedMs / 1000)} s`}`);
      rc.claimResult = await runClaim(rc);
      // the buy (its KAS back, and what it holds), the counterparty's curve, the wallet's own curve
      // (its custody back)
      const R = rc.curveRun;
      const bidCreatedBy = new Map([[R.bidTxid, R.bidTx], ...(R.claimTxid ? [[R.claimTxid, R.claimTx]] : [])]);
      const cb = await cancelStep(withToken(rc, tok), r, 'curve-buy', { placed: bidCreatedBy, covenantIds: [bidId], looseSpent: !!R.filled && !R.claimTxid });
      const ct = await cancelStep(withToken(cpRc, tok), r, 'cp-curve', { placed: new Map([[theirs.genesisTxid, theirs.genesisTx]]), covenantIds: [theirs.curveId], looseSpent: !!R.filled });
      log(`[${rc.name}] curve: buy cancelled ${short(cb.txid)}, counterparty curve cancelled ${short(ct.txid)}`);
    } finally {
      const swept = await sweepOpenOrders(cpRc).catch((e) => [{ error: String(e?.message ?? e).slice(0, 200) }]);
      if (swept.length) r.steps.push({ name: 'counterparty sweep', swept });
      await cp.close().catch(() => {});
    }
    const co = await cancelStep(withToken(rc, own.token), r, 'curve', { placed: new Map([[own.genesisTxid, own.genesisTx]]), covenantIds: [own.curveId], tokensBack: { token: own.token.tokenId, amount: own.sell * TOKEN } });
    log(`[${rc.name}] curve: own curve ${short(own.curveId)} cancelled ${short(co.txid)}`);
  });
}

/**
 * The Claim of what the Collected buy of `runCurve` bought: Balances' Claim, its dialog (amount,
 * fee), one approval; the buy stays open.
 */
async function runClaim(rc) {
  return scenario(rc, 'claim', async (r) => {
    const R = rc.curveRun;
    if (!R?.bidId) throw new Skip('needs the curve scenario');
    if (!R.filled) throw new Skip(`no executor filled the buy against the curve within 300 s (${JSON.stringify(R.fillViews ?? {}).slice(0, 300)})`);
    const { h } = rc;
    const tokenId = R.token.tokenId;
    await openOrders(rc, [], { tab: 'balances' });
    const before = await waitBalance(rc, tokenId, (b) => true, 'the row');
    const btn = tid(h, `balance-claim-${tokenId}`);
    await pollBalances(rc, () => btn.count(), (n) => n > 0, 180_000);
    if (!(await btn.count())) throw new Error(`Balances offers no Claim for ${R.token.ticker}: ${JSON.stringify(await balanceRow(rc, tokenId))}`);
    const bought = collapse(await h.page.locator(`[data-testid="balance-row-${tokenId}"] [data-testid="balance-bought"]`).innerText().catch(() => ''));
    await btn.click();
    const dialog = tid(h, 'claim-dialog');
    await dialog.waitFor({ timeout: 30_000 });
    await tid(h, 'claim-fee').waitFor({ timeout: 30_000 }).catch(() => {});
    const view = await h.page.evaluate(() => {
      const t = (id) => (document.querySelector(`[data-testid="${id}"]`)?.textContent ?? '').replace(/\s+/g, ' ').trim();
      return { amount: t('claim-amount'), fee: t('claim-fee'), carrier: t('claim-carrier'), error: t('claim-error') };
    });
    const dialogShot = await shot(h.page, rc.name, 'claim-dialog');
    if (await tid(h, 'claim-kind-claim').count()) await tid(h, 'claim-kind-claim').click();
    if (!(await tid(h, 'claim-confirm').isEnabled())) throw new Error(`the Claim dialog's confirm is disabled: ${JSON.stringify(view)}`);
    await tid(h, 'claim-confirm').click();
    const { steps, done } = await runFlow(h, rc.ctx, rc.drv, 'claim', rc.name);
    const s = steps.at(-1);
    if (!s?.submitted) throw new Error('the claim transaction was not captured');
    const step = { ...stepOf('claim', s), balancesBefore: before, bought, dialog: view, done };
    step.screenshots.unshift(dialogShot);
    r.steps.push(step);
    // node: the claim's covenant outputs (the buy's continuation, the tokens to the wallet) are
    // unspent
    step.verify = await verifyCreated(rc, s.submitted, s.txid, 'claim');
    const got = await trackedFrom(h, s.txid, tokenId);
    step.tracked = got.items;
    if (got.total !== R.level * TOKEN) throw new Error(`the claim should send ${R.level * TOKEN} base units to the wallet, the app tracks ${got.total} (${JSON.stringify(got.items)})`);
    R.claimTxid = s.txid;
    R.claimTx = s.submitted;
    // the buy stays open with nothing bought
    await openOrders(rc, [R.bidId]);
    const t0 = Date.now();
    let row = await curveRow(h, R.bidId);
    while ((row.bought || !row.live) && Date.now() - t0 < 90_000) {
      await sleep(3000);
      await clickRefresh(h);
      row = await curveRow(h, R.bidId);
    }
    step.rowAfter = row;
    if (!row.live || row.bought) throw new Error(`after the claim the buy should be open with nothing bought: ${JSON.stringify(row)}`);
    log(`[${rc.name}] claim: ${short(s.txid)}, ${got.total / TOKEN} ${R.token.ticker} to the wallet`);
  });
}

/**
 * The `claim` result of the run (made inside `curve`); SKIP when the curve scenario did not run.
 */
export async function claimResult(rc) {
  if (rc.claimResult) return rc.claimResult;
  const why = rc.curveRun ? 'not reached: the curve scenario stopped before the claim' : 'runs inside the curve scenario (--only curve,claim)';
  return { name: 'claim', ok: !rc.curveRun, skipped: rc.curveRun ? null : why, error: rc.curveRun ? why : null, steps: [] };
}

/**
 * Chromatic curve sale in the genesis (10% kept, 1 octave = 13 levels, the top 2x the first, 20 KAS
 * if sold out), then its cancel.
 */
export async function runChromatic(rc) {
  return scenario(rc, 'chromatic', async (r) => {
    const sale = { keepPct: 10, shape: 'chromatic', octaves: 1, totalKas: 20 };
    const c = await createCurve(rc, r, 'chromatic', sale, 13);
    if (!/13, top 2× first/.test(c.form.octavesRange)) throw new Error(`the octaves line says "${c.form.octavesRange}", expected 13 levels, top 2x`);
    const ratio = parseKas(c.form.last) / parseKas(c.form.first);
    c.step.lastOverFirst = ratio;
    if (!(ratio > 1.98 && ratio < 2.02)) throw new Error(`the last level is ${ratio}x the first, expected 2x (one octave)`);
    const cc = await cancelStep(withToken(rc, c.token), r, 'chromatic', { placed: new Map([[c.genesisTxid, c.genesisTx]]), covenantIds: [c.curveId], tokensBack: { token: c.token.tokenId, amount: c.sell * TOKEN } });
    log(`[${rc.name}] chromatic: genesis ${short(c.genesisTxid)}, cancelled ${short(cc.txid)}`);
  });
}
