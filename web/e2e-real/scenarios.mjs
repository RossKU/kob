// Scenarios of the real-wallet run: issue, limit (sell + buy), IFD, OCO. Every placement / cancel goes through the app's pre-sign confirmation
// screen and the REAL wallet popup; every accepted tx is then checked on the node independently of the app (the covenant outpoints are unspent
// after a placement and spent after a cancel). Nothing runs on import.
import { readFileSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { REPO_ROOT, collapse, log, shot, sleep } from './common.mjs';
import { ACCEPT_TIMEOUT_MS, goto, signOnConfirmScreen } from './driver.mjs';

const tid = (h, id) => h.page.getByTestId(id);
const short = (s) => (s ? `${s.slice(0, 10)}...` : '-');

// ------------------------------------------------------------------------------------------------ registry

/** A TEST registry: the reviewed templates of registry/tokens.example.json plus the issued token, listed and verified (lot = 1 token, tick 1000 sompi). */
export function buildTestRegistry(entry) {
  const reg = JSON.parse(readFileSync(join(REPO_ROOT, 'registry', 'tokens.example.json'), 'utf8'));
  for (const t of reg.templates) t.review_status = 'reviewed';
  reg.tokens = [{ ...entry, status: 'listed', verified: true }];
  return reg;
}

// ------------------------------------------------------------------------------------------------ step / scenario plumbing

/** Runs `fn(rec)` as a scenario: collects steps, turns a throw into a failed scenario with screenshots. */
async function scenario(rc, name, fn) {
  const rec = { name, ok: false, error: null, steps: [], startedAt: Date.now() };
  try {
    await fn(rec);
    rec.ok = true;
  } catch (e) {
    rec.error = String(e?.message ?? e).slice(0, 1200);
    log(`[${rc.name}] scenario ${name} FAILED: ${rec.error.slice(0, 300)}`);
    try {
      rec.failScreenshot = await shot(rc.h.page, rc.name, `${name}-failure`);
      rec.failText = collapse(await rc.h.page.innerText('body')).slice(0, 2500);
      rec.lastSubmits = rc.h.submits.slice(-2);
      if (process.env.KOB_DEBUG_TX) rec.debugTxs = await rc.h.page.evaluate(() => (window.__txs ?? []).slice(-2));
    } catch {
      /* ignore */
    }
  }
  rec.durationMs = Date.now() - rec.startedAt;
  return rec;
}

/** Kaspire's raw JSON lists the `signatureScriptMode` it applied per input (null for plain P2PK inputs): kept as a compact list, the raw JSON itself is truncated. */
const kaspireModes = (raw) => [...new Set([...String(raw ?? '').matchAll(/"(?:signatureScriptMode|mode)":\s*("[^"]*"|null)/g)].map((m) => m[1].replace(/"/g, '')))];
/** The raw JSON of Kaspire's approval window without the (huge, already shown) transaction string. */
const compactRaw = (raw) => {
  try {
    const j = JSON.parse(raw);
    if (j?.request?.txJsonString) j.request.txJsonString = `<${String(j.request.txJsonString).length} chars>`;
    return JSON.stringify(j, null, 1).slice(0, 6000);
  } catch {
    return String(raw).slice(0, 4000);
  }
};
const popupView = (popups) =>
  popups.map((p) => ({
    url: p.url,
    clicked: p.result?.clicked ?? null,
    text: p.result?.text ?? p.result?.summary ?? null,
    title: p.result?.title ?? null,
    signatureScriptModes: p.result?.rawJson ? kaspireModes(p.result.rawJson) : undefined,
    rawJson: p.result?.rawJson ? compactRaw(p.result.rawJson) : undefined,
    screenshot: p.result?.screenshot ?? null,
    error: p.error,
  }));

/** Node check of one accepted tx: every P2SH output is unspent (`phase: 'placed'`). */
async function verifyCreated(rc, tx, txid, label) {
  const outs = rc.node.outputsOf(tx).filter((o) => o.p2sh && o.address);
  const checks = [];
  for (const o of outs) {
    const w = await rc.node.waitOutpoint(o.address, txid, o.index, true);
    checks.push({ what: `${label}: covenant output ${short(txid)}:${o.index}`, unspent: w.ok, ms: w.ms });
    if (!w.ok) throw new Error(`node check failed: covenant output ${txid}:${o.index} is not unspent after ${w.ms} ms`);
  }
  if (!outs.length) throw new Error(`node check: the transaction ${txid} created no P2SH (covenant) output`);
  return checks;
}

/** Node check of a cancel tx: the covenant outpoints it spent are gone, its own P2SH outputs (returned tokens / KAS carriers) exist. */
async function verifyCancelled(rc, cancelTx, cancelTxid, createdBy) {
  const checks = [];
  const created = new Map();
  for (const [txid, tx] of createdBy) for (const o of rc.node.outputsOf(tx)) if (o.p2sh && o.address) created.set(`${txid}:${o.index}`, o.address);
  for (const inp of cancelTx.inputs ?? []) {
    const op = inp.previousOutpoint ?? inp.previous_outpoint ?? {};
    const key = `${op.transactionId}:${op.index}`;
    const address = created.get(key);
    if (!address) continue;
    const w = await rc.node.waitOutpoint(address, op.transactionId, op.index, false);
    checks.push({ what: `spent by cancel: ${short(op.transactionId)}:${op.index}`, gone: w.ok, ms: w.ms });
    if (!w.ok) throw new Error(`node check failed: ${key} is still unspent after the cancel was accepted`);
  }
  if (!checks.length) throw new Error('node check: the cancel transaction spends none of the covenant outputs placed earlier');
  for (const o of rc.node.outputsOf(cancelTx).filter((x) => x.p2sh && x.address)) {
    const w = await rc.node.waitOutpoint(o.address, cancelTxid, o.index, true);
    checks.push({ what: `created by cancel: ${short(cancelTxid)}:${o.index}`, unspent: w.ok, ms: w.ms });
    if (!w.ok) throw new Error(`node check failed: cancel output ${cancelTxid}:${o.index} is not unspent`);
  }
  return checks;
}

const bodyText = async (h) => collapse(await h.page.innerText('body').catch(() => ''));

/** localStorage records (placements) of the connected key: `{ covenantId: record }`. */
async function readRecords(h) {
  return h.page.evaluate(() => {
    const out = {};
    for (let i = 0; i < localStorage.length; i++) {
      const k = localStorage.key(i);
      if (k && k.startsWith('kob.records.v1:testnet-10:')) Object.assign(out, JSON.parse(localStorage.getItem(k) || '{}'));
    }
    return out;
  });
}

/** Saves the token file (registry entry + the tracker entries of the app) so a later `--skip-issue` run can continue with the same token. */
export async function snapshotTracker(rc, tokenFile) {
  const trackerLS = await rc.h.page.evaluate(() => {
    const out = {};
    for (let i = 0; i < localStorage.length; i++) {
      const k = localStorage.key(i);
      if (k && (k.startsWith('kob.tokens.v1.') || k.startsWith('kob.records.v1:'))) out[k] = localStorage.getItem(k);
    }
    return out;
  });
  rc.token.trackerLS = trackerLS;
  writeFileSync(tokenFile, JSON.stringify(rc.token, null, 2));
}

// ------------------------------------------------------------------------------------------------ issuance

export async function issueToken(rc) {
  const { h } = rc;
  let token = null;
  const rec = await scenario(rc, 'issue', async (r) => {
    const t0 = Date.now();
    const ticker = `RT${rc.name === 'kasware' ? 'KW' : 'KP'}${Math.random().toString(36).slice(2, 6).toUpperCase().replace(/[OIL]/g, 'X')}`;
    await goto(h, '#/issue');
    await tid(h, 'issue-form').waitFor({ timeout: 60_000 });
    await tid(h, 'issue-name').fill(`KOB real ${rc.name} test`);
    await tid(h, 'issue-ticker').fill(ticker);
    await tid(h, 'issue-decimals').fill('8');
    await tid(h, 'issue-supply').fill('1000000');
    const review = tid(h, 'issue-review');
    for (let i = 0; i < 60 && !(await review.isEnabled()); i++) await sleep(1000);
    if (!(await review.isEnabled())) throw new Error(`issue review stays disabled: ${(await bodyText(h)).slice(0, 600)}`);
    await review.click();
    const s = await signOnConfirmScreen(h, rc.ctx, rc.drv, 'issue', rc.name);
    r.steps.push({ name: 'issue genesis', txid: s.txid, summary: s.summary, popups: popupView(s.popups), screenshots: s.screenshots, appStatus: s.appStatus, ms: s.totalMs });
    if (!s.submitted) throw new Error('the submitted transaction was not captured from the node connection');
    r.steps.at(-1).verify = await verifyCreated(rc, s.submitted, s.txid, 'genesis');
    await tid(h, 'issue-result').waitFor({ timeout: 60_000 });
    const tokenId = await tid(h, 'issue-result-tokenId').getAttribute('data-value');
    const registryEntry = JSON.parse(await tid(h, 'issue-registry-entry').innerText());
    if (registryEntry.covenant_id !== tokenId) throw new Error('registry entry covenant id differs from the shown token id');
    r.steps.at(-1).screenshots.push(await shot(h.page, rc.name, 'issue-result'));
    // the app tracks the wallet's token UTXOs from the genesis (independent of the node check above)
    const tracked = await h.page.evaluate(() => {
      const out = [];
      for (let i = 0; i < localStorage.length; i++) {
        const k = localStorage.key(i);
        if (k && k.startsWith('kob.tokens.v1.testnet-10.')) out.push(...(JSON.parse(localStorage.getItem(k)).items ?? []));
      }
      return out.map((x) => ({ txid: x.transactionId, index: x.index, amount: x.state?.amount, token: x.tokenCovId }));
    });
    if (!tracked.some((x) => x.txid === s.txid && x.token === tokenId)) throw new Error(`the app did not track the issued token UTXO (tracked: ${JSON.stringify(tracked).slice(0, 300)})`);
    r.steps.at(-1).tracked = tracked;
    token = { ticker, tokenId, registryEntry, issueTxid: s.txid, supply: '1000000', tracked };
    log(`[${rc.name}] issued ${ticker} token ${short(tokenId)} in ${Date.now() - t0} ms, tx ${short(s.txid)}`);
  });
  return { token, scenario: rec };
}

// ------------------------------------------------------------------------------------------------ placing

/**
 * Fills the order ticket and reviews it. spec: { side, type, lots, price?, fields?: {name: value}, expectSummary?: RegExp }.
 * `price` is KAS per token (the ticket's unit); `fields` are `field-<name>` inputs (KAS per token for prices).
 */
async function fillTicket(rc, spec) {
  const { h } = rc;
  await goto(h, `#/market/${rc.token.tokenId}`);
  await tid(h, 'order-ticket').waitFor({ timeout: 60_000 });
  if ((await tid(h, 'order-ticket').getAttribute('data-state')) === 'untradable') throw new Error(`the ticket says the token is untradable: ${(await tid(h, 'order-untradable').innerText()).slice(0, 300)}`);
  await tid(h, 'order-type').selectOption(spec.type);
  await tid(h, spec.side === 'buy' ? 'order-side-buy' : 'order-side-sell').click();
  await tid(h, 'order-lots').fill(String(spec.lots));
  if (spec.price !== undefined) await tid(h, 'order-price').fill(String(spec.price));
  for (const [k, v] of Object.entries(spec.fields ?? {})) await tid(h, `field-${k}`).fill(String(v));
  const review = tid(h, 'order-review');
  for (let i = 0; i < 90 && !(await review.isEnabled()); i++) await sleep(1000);
  if (!(await review.isEnabled())) {
    const issues = await tid(h, 'order-issues').innerText().catch(() => '');
    const needs = await tid(h, 'order-needs').innerText().catch(() => '');
    throw new Error(`order review stays disabled. issues: ${collapse(issues)} needs: ${collapse(needs)}`);
  }
  const disclosure = collapse(await tid(h, 'order-disclosure').innerText().catch(() => ''));
  const balancesBefore = collapse(await tid(h, 'order-balances').innerText().catch(() => ''));
  await review.click();
  return { disclosure, balancesBefore };
}

/** Place through the ticket, sign in the real wallet, verify on the node. Returns the step record plus what the cancel needs. */
async function place(rc, r, label, spec) {
  const before = new Set(Object.keys(await readRecords(rc.h)));
  const { disclosure, balancesBefore } = await fillTicket(rc, spec);
  const s = await signOnConfirmScreen(rc.h, rc.ctx, rc.drv, label, rc.name);
  if (!s.submitted) throw new Error('the submitted transaction was not captured from the node connection');
  if (spec.expectSummary && !spec.expectSummary.test(s.summary)) throw new Error(`the confirmation summary does not say what was ordered: ${s.summary.slice(0, 300)}`);
  const step = { name: `place ${label}`, txid: s.txid, summary: s.summary, disclosure, balancesBefore, popups: popupView(s.popups), screenshots: s.screenshots, appStatus: s.appStatus, ms: s.totalMs };
  r.steps.push(step);
  step.verify = await verifyCreated(rc, s.submitted, s.txid, label);
  await sleep(1500);
  const after = await readRecords(rc.h);
  const created = Object.entries(after).filter(([id]) => !before.has(id));
  step.records = created.map(([id, rec]) => ({ covenantId: id, kind: rec.kind, txid: rec.txid, output: rec.output }));
  if (!created.length) throw new Error('the app stored no placement record for the accepted transaction');
  // the record the app keeps must point at a covenant outpoint that the node lists as unspent (verified above)
  const p2sh = new Set(rc.node.outputsOf(s.submitted).filter((o) => o.p2sh).map((o) => o.index));
  for (const [id, rec] of created) if (rec.txid !== s.txid || !p2sh.has(rec.output)) throw new Error(`placement record of ${short(id)} points at ${short(rec.txid)}:${rec.output}, not at a covenant output of the accepted transaction`);
  return { step, tx: s.submitted, txid: s.txid, covenantIds: created.map(([id]) => id) };
}

// ------------------------------------------------------------------------------------------------ cancelling

/** Opens My orders (node-only mode: the rows come from the placement records resolved on the node). */
async function openOrders(rc, wantIds, { timeoutMs = 90_000 } = {}) {
  const { h } = rc;
  await goto(h, '#/orders');
  await tid(h, 'orders-view').waitFor({ timeout: 60_000 });
  const t0 = Date.now();
  for (;;) {
    const missing = [];
    for (const id of wantIds) if (!(await tid(h, `order-row-${id}`).count())) missing.push(id);
    if (!missing.length) return;
    if (Date.now() - t0 > timeoutMs) throw new Error(`My orders does not show ${missing.map(short).join(', ')} after ${timeoutMs} ms. page: ${(await bodyText(h)).slice(0, 700)}`);
    const refresh = tid(h, 'orders-refresh');
    if (Date.now() - t0 > 10_000 && (await refresh.count())) await refresh.click().catch(() => {});
    await sleep(2500);
  }
}

async function cancelStep(rc, r, label, { placed, covenantIds, position = false, tokensBack = 0n }) {
  const { h } = rc;
  await openOrders(rc, covenantIds);
  const rows = [];
  for (const id of covenantIds) rows.push({ id, status: await tid(h, `order-row-${id}`).getAttribute('data-status'), type: await tid(h, `order-row-${id}`).getAttribute('data-type') });
  const shot1 = await shot(h.page, rc.name, `${label}-orders-listed`);
  const balancesPanel = collapse(await tid(h, 'balances-panel').innerText().catch(() => ''));
  if (position) {
    // with an indexer an IFD / OCO-entry is a position card with one cancel for all its orders; from the node alone it is the entry's own row
    const btn = h.page.locator('[data-testid^="position-cancel-"]').first();
    if (await btn.count()) await btn.click();
    else await tid(h, `order-cancel-${covenantIds[0]}`).click();
  } else {
    await tid(h, `order-cancel-${covenantIds[0]}`).click();
  }
  const s = await signOnConfirmScreen(h, rc.ctx, rc.drv, `${label}-cancel`, rc.name);
  if (!s.submitted) throw new Error('the cancel transaction was not captured from the node connection');
  const step = { name: `cancel ${label}`, txid: s.txid, ordersBefore: rows, balancesPanelBeforeCancel: balancesPanel, summary: s.summary, popups: popupView(s.popups), screenshots: [shot1, ...s.screenshots], appStatus: s.appStatus, ms: s.totalMs };
  r.steps.push(step);
  step.verify = await verifyCancelled(rc, s.submitted, s.txid, placed);
  if (tokensBack > 0n) {
    // tokens released from custody come back as a wallet-owned token UTXO of the cancel tx: the app's tracker (each entry re-verified on the node) must hold them
    await sleep(1000);
    const tracked = await h.page.evaluate((cancelTxid) => {
      const out = [];
      for (let i = 0; i < localStorage.length; i++) {
        const k = localStorage.key(i);
        if (k && k.startsWith('kob.tokens.v1.testnet-10.')) out.push(...(JSON.parse(localStorage.getItem(k)).items ?? []));
      }
      return out.filter((x) => x.transactionId === cancelTxid).map((x) => ({ index: x.index, amount: x.state?.amount }));
    }, s.txid);
    const got = tracked.reduce((a, x) => a + BigInt(x.amount ?? 0), 0n);
    step.tokensBack = { expectedAtLeast: tokensBack.toString(), trackedFromCancel: tracked };
    if (got < tokensBack) throw new Error(`the cancel should release ${tokensBack} base units of the token to the wallet, the app tracks ${got} (${JSON.stringify(tracked)})`);
  }
  // the order leaves the active list
  await sleep(1000);
  await openOrders(rc, [], {}).catch(() => {});
  const t0 = Date.now();
  let gone = false;
  while (Date.now() - t0 < 60_000) {
    const still = [];
    for (const id of covenantIds) {
      const row = tid(h, `order-row-${id}`);
      if ((await row.count()) && (await row.getAttribute('data-status')) === 'open') still.push(id);
    }
    if (!still.length) {
      gone = true;
      break;
    }
    const refresh = tid(h, 'orders-refresh');
    if (await refresh.count()) await refresh.click().catch(() => {});
    await sleep(3000);
  }
  step.ordersAfter = [];
  for (const id of covenantIds) {
    const row = tid(h, `order-row-${id}`);
    step.ordersAfter.push({ id, status: (await row.count()) ? await row.getAttribute('data-status') : 'not listed' });
  }
  step.screenshots.push(await shot(h.page, rc.name, `${label}-orders-after`));
  if (!gone) throw new Error(`the app still lists the cancelled order(s) as open: ${JSON.stringify(step.ordersAfter)}`);
  return step;
}

/** Token balance text of the ticket (available / in orders), for the record. */
async function ticketBalances(rc) {
  const { h } = rc;
  await goto(h, `#/market/${rc.token.tokenId}`);
  await tid(h, 'order-ticket').waitFor({ timeout: 60_000 });
  await sleep(2500);
  return collapse(await tid(h, 'order-balances').innerText().catch(() => ''));
}

async function placeAndCancel(rc, r, label, spec, { position = false, tokensBack = 0n } = {}) {
  const p = await place(rc, r, label, spec);
  const c = await cancelStep(rc, r, label, { placed: new Map([[p.txid, p.tx]]), covenantIds: p.covenantIds, position, tokensBack });
  c.balancesAfter = await ticketBalances(rc);
  log(`[${rc.name}] ${label}: placed ${short(p.txid)}, cancelled ${short(c.txid)}`);
  return { p, c };
}

/** Best-effort sweep at the end of a run: cancels any order still open (a failed scenario must not leave the test wallet's KAS locked). */
export async function sweepOpenOrders(rc) {
  const { h } = rc;
  const done = [];
  try {
    await goto(h, '#/orders');
    await tid(h, 'orders-view').waitFor({ timeout: 60_000 });
    await sleep(8000);
    for (let round = 0; round < 6; round++) {
      const ids = await h.page.evaluate(() => [...document.querySelectorAll('[data-testid^="order-cancel-"]')].map((e) => e.getAttribute('data-testid').slice('order-cancel-'.length)));
      if (!ids.length) break;
      await tid(h, `order-cancel-${ids[0]}`).click();
      const s = await signOnConfirmScreen(h, rc.ctx, rc.drv, 'sweep-cancel', rc.name);
      done.push({ orderId: ids[0], txid: s.txid });
      await sleep(4000);
      await goto(h, '#/orders');
      await sleep(4000);
    }
  } catch (e) {
    done.push({ error: String(e?.message ?? e).slice(0, 300) });
  }
  return done;
}

// ------------------------------------------------------------------------------------------------ scenarios

export async function runLimit(rc) {
  return scenario(rc, 'limit', async (r) => {
    // a limit SELL far above any market: it rests; its tokens go into one custody UTXO
    await placeAndCancel(rc, r, 'limit-sell', { side: 'sell', type: 'limit', lots: 3, price: 50, expectSummary: /sell/i }, { tokensBack: 3n * 100_000_000n });
    // a limit BUY (KAS escrow)
    await placeAndCancel(rc, r, 'limit-buy', { side: 'buy', type: 'limit', lots: 2, price: 0.5, expectSummary: /buy/i });
  });
}

export async function runIfd(rc) {
  return scenario(rc, 'ifd', async (r) => {
    // buy first at 0.5, sell 1.0 when filled (take-profit): one position with its exit
    await placeAndCancel(rc, r, 'ifd-buy-first', { side: 'buy', type: 'ifd', lots: 2, price: 0.5, fields: { 'exit.takeProfit': 1 } }, { position: true });
  });
}

export async function runOco(rc) {
  return scenario(rc, 'oco', async (r) => {
    // sell OCO: take-profit far above, stop far below
    await placeAndCancel(rc, r, 'oco-sell', { side: 'sell', type: 'oco', lots: 2, fields: { takeProfit: 60, stop: 0.2 } }, { tokensBack: 2n * 100_000_000n });
  });
}
