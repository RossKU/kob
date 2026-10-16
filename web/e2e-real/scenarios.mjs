// Scenarios of the real-wallet run. Every placement / cancel / amend / merge goes through the app's pre-sign confirmation screen and the wallet
// (the REAL extension's window on TN10, the in-page mock wallet with `--mock`); every accepted transaction is then checked on the node
// independently of the app: the covenant outputs of a placement are unspent, the ones a cancel / amend spends are gone and its own outputs exist.
// The UI is driven only through its test ids (web/e2e/testids.ts, the components under web/src/ui). Nothing runs on import.
//
// Prices are typed as the ticket shows them: KAS per whole token on a token's KAS market, B per whole A on the pair A/B. Amounts are whole tokens
// (the issued tokens have 8 decimals: 1 token = 1e8 base units).
import { readFileSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { REPO_ROOT, collapse, log, shot, sleep } from './common.mjs';
import { goto, outputsOf, runFlow, signFromTicket, signOnConfirmScreen } from './driver.mjs';

const tid = (h, id) => h.page.getByTestId(id);
const short = (s) => (s ? `${s.slice(0, 10)}...` : '-');
export const TOKEN = 100_000_000n; // base units per whole token (8 decimals)
export const SUPPLY = 1_000_000n; // whole tokens issued per token

/** A scenario that cannot run in this configuration (e.g. market without an indexer): reported as SKIP with the reason, not as a failure. */
export class Skip extends Error {}

// ------------------------------------------------------------------------------------------------ registry

/**
 * A TEST registry: the templates of registry/tokens.example.json (marked reviewed, as the mock e2e does) plus the issued tokens as `listed`.
 * The entries are the ones the issuance screen produced (`issue-registry-entry`), so template id, extension commitment and decimals are the app's.
 */
export function buildTestRegistry(entries) {
  const reg = JSON.parse(readFileSync(join(REPO_ROOT, 'registry', 'tokens.example.json'), 'utf8'));
  for (const t of reg.templates) t.review_status = 'reviewed';
  reg.tokens = entries.filter(Boolean).map((e) => ({ ...e, status: 'listed', verified: true }));
  return reg;
}

// ------------------------------------------------------------------------------------------------ step / scenario plumbing

/** Runs `fn(rec)` as a scenario: collects steps, turns a throw into a failed scenario with screenshots (a `Skip` into a skipped one). */
async function scenario(rc, name, fn) {
  const rec = { name, ok: false, skipped: null, error: null, steps: [], startedAt: Date.now() };
  try {
    await fn(rec);
    rec.ok = true;
  } catch (e) {
    if (e instanceof Skip) {
      rec.ok = true;
      rec.skipped = e.message;
      log(`[${rc.name}] scenario ${name} SKIPPED: ${e.message}`);
    } else {
      rec.error = String(e?.message ?? e).slice(0, 1500);
      log(`[${rc.name}] scenario ${name} FAILED: ${rec.error.slice(0, 400)}`);
      try {
        rec.failScreenshot = await shot(rc.h.page, rc.name, `${name}-failure`);
        rec.failText = collapse(await rc.h.page.innerText('body')).slice(0, 2500);
        rec.lastSubmits = rc.h.submits.slice(-2);
      } catch {
        /* ignore */
      }
      // a failure must not leave a dialog (an amend form, the issuance confirmation) over the next scenario
      await closeDialogs(rc.h).catch(() => {});
    }
  }
  rec.durationMs = Date.now() - rec.startedAt;
  return rec;
}

async function closeDialogs(h) {
  for (const id of ['confirm-close', 'confirm-cancel', 'amend-cancel', 'replace-cancel']) {
    const b = tid(h, id);
    if (await b.count()) await b.first().click({ timeout: 3000 }).catch(() => {});
  }
  await h.page.keyboard.press('Escape').catch(() => {});
}

/** Kaspire's raw JSON lists the `signatureScriptMode` it applied per input (null for plain P2PK inputs): kept as a compact list. */
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

/** The record of one signed transaction for the result file. */
const stepOf = (name, s) => ({ name, txid: s.txid, kind: s.kind, summary: s.summary, shownAs: s.shownAs || undefined, acks: s.acks.length ? s.acks : undefined, popups: popupView(s.popups), screenshots: s.screenshots, appStatus: s.appStatus, ms: s.totalMs });

/** Node check of one accepted tx: every P2SH output is unspent. */
async function verifyCreated(rc, tx, txid, label) {
  const outs = outputsOf(rc.node, tx).filter((o) => o.p2sh && o.address);
  const checks = [];
  for (const o of outs) {
    const w = await rc.node.waitOutpoint(o.address, txid, o.index, true);
    checks.push({ what: `${label}: covenant output ${short(txid)}:${o.index}`, unspent: w.ok, ms: w.ms });
    if (!w.ok) throw new Error(`node check failed: covenant output ${txid}:${o.index} is not unspent after ${w.ms} ms`);
  }
  if (!outs.length) throw new Error(`node check: the transaction ${txid} created no P2SH (covenant) output`);
  return checks;
}

/** Node check of a cancel / amend tx: the covenant outpoints it spent (created by `createdBy`) are gone, its own P2SH outputs exist. */
async function verifySpent(rc, tx, txid, createdBy, what = 'cancel') {
  const checks = [];
  const created = new Map();
  for (const [ptxid, ptx] of createdBy) for (const o of outputsOf(rc.node, ptx)) if (o.p2sh && o.address) created.set(`${ptxid}:${o.index}`, o.address);
  for (const inp of tx.inputs ?? []) {
    const op = inp.previousOutpoint ?? inp.previous_outpoint ?? inp; // wRPC frame: previousOutpoint; kob-wasm JSON (mock node): flat
    const key = `${op.transactionId}:${op.index}`;
    const address = created.get(key);
    if (!address) continue;
    const w = await rc.node.waitOutpoint(address, op.transactionId, op.index, false);
    checks.push({ what: `spent by ${what}: ${short(op.transactionId)}:${op.index}`, gone: w.ok, ms: w.ms });
    if (!w.ok) throw new Error(`node check failed: ${key} is still unspent after the ${what} was accepted`);
  }
  if (!checks.length) throw new Error(`node check: the ${what} transaction spends none of the covenant outputs placed earlier`);
  for (const o of outputsOf(rc.node, tx).filter((x) => x.p2sh && x.address)) {
    const w = await rc.node.waitOutpoint(o.address, txid, o.index, true);
    checks.push({ what: `created by ${what}: ${short(txid)}:${o.index}`, unspent: w.ok, ms: w.ms });
    if (!w.ok) throw new Error(`node check failed: ${what} output ${txid}:${o.index} is not unspent`);
  }
  return checks;
}

const bodyText = async (h) => collapse(await h.page.innerText('body').catch(() => ''));

/** localStorage placement records of the connected key: `{ covenantId: record }`. */
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

/** The token UTXOs the app's tracker remembers for the wallet (localStorage `kob.tokens.v1.testnet-10.<pubkey>`). */
async function trackedTokens(h) {
  return h.page.evaluate(() => {
    const out = [];
    for (let i = 0; i < localStorage.length; i++) {
      const k = localStorage.key(i);
      if (k && k.startsWith('kob.tokens.v1.testnet-10.')) out.push(...(JSON.parse(localStorage.getItem(k)).items ?? []));
    }
    return out.map((x) => ({ txid: x.transactionId, index: x.index, amount: x.state?.amount, token: x.tokenCovId, program: x.program }));
  });
}

/** Saves the token file (registry entries + tracker / records of the app) so a later `--skip-issue` run can continue with the same tokens. */
export async function snapshotTracker(rc, tokenFile) {
  if (!tokenFile) return;
  const ls = await rc.h.page.evaluate(() => {
    const out = {};
    for (let i = 0; i < localStorage.length; i++) {
      const k = localStorage.key(i);
      if (k && (k.startsWith('kob.tokens.v1.') || k.startsWith('kob.records.v1:') || k.startsWith('kob.covenantSign.v1:'))) out[k] = localStorage.getItem(k);
    }
    return out;
  });
  writeFileSync(tokenFile, JSON.stringify({ token: rc.token, tokenB: rc.tokenB ?? null, trackerLS: ls }, null, 2));
}

// ------------------------------------------------------------------------------------------------ issuance

/** Issues a fixed-supply token of KOB's standard 3 / 3 program (the issuance screen's default) to the wallet. Returns its description. */
async function issueOne(rc, r, which) {
  const { h } = rc;
  const t0 = Date.now();
  const ticker = `R${which}${{ kasware: 'KW', kaspire: 'KP', kastle: 'KS' }[rc.name] ?? 'XX'}${Math.random().toString(36).slice(2, 6).toUpperCase().replace(/[OIL0-9]/g, 'X')}`.slice(0, 8);
  await goto(h, '#/issue');
  if (await tid(h, 'issue-another').count()) await tid(h, 'issue-another').click();
  await tid(h, 'issue-form').waitFor({ timeout: 60_000 });
  // the program: KOB's standard 3 / 3 reference (KCC20Ref) is the default choice
  if ((await tid(h, 'issue-program-standard').getAttribute('aria-checked')) !== 'true') await tid(h, 'issue-program-standard').click();
  const program = collapse(await tid(h, 'issue-fixed-program').innerText().catch(() => ''));
  if (!/KCC20Ref\b/.test(program) || /PublicMint/.test(program)) throw new Error(`the issuance screen does not plan the standard 3 / 3 program: "${program}"`);
  await tid(h, 'issue-name').fill(`KOB real ${rc.name} ${which}`);
  await tid(h, 'issue-ticker').fill(ticker);
  await tid(h, 'issue-decimals').fill('8');
  await tid(h, 'issue-supply').fill(String(SUPPLY));
  const review = tid(h, 'issue-review');
  for (let i = 0; i < 60 && !(await review.isEnabled()); i++) await sleep(1000);
  if (!(await review.isEnabled())) throw new Error(`issue review stays disabled: ${collapse(await tid(h, 'issue-view').innerText().catch(() => bodyText(h))).slice(0, 700)}`);
  const summary = collapse(await tid(h, 'issue-summary').innerText().catch(() => ''));
  await review.click();
  const s = await signOnConfirmScreen(h, rc.ctx, rc.drv, `issue-${which}`, rc.name);
  const step = { ...stepOf(`issue ${which} (${ticker})`, s), issueSummary: summary, program };
  r.steps.push(step);
  if (!s.submitted) throw new Error('the submitted transaction was not captured from the node connection');
  step.verify = await verifyCreated(rc, s.submitted, s.txid, `genesis ${which}`);
  await tid(h, 'issue-result').waitFor({ timeout: 60_000 });
  const tokenId = await tid(h, 'issue-result-tokenId').getAttribute('data-value');
  const shownTxid = await tid(h, 'issue-result-txid').getAttribute('data-value');
  if (shownTxid !== s.txid) throw new Error(`the result panel shows txid ${shownTxid}, the confirmation screen ${s.txid}`);
  const registryEntry = JSON.parse(await tid(h, 'issue-registry-entry').innerText());
  if (registryEntry.covenant_id !== tokenId) throw new Error('registry entry covenant id differs from the shown token id');
  if (registryEntry.template_id !== 'kcc20-ref-3x3' || registryEntry.max_token_inputs !== 3) throw new Error(`the registry entry is not the standard 3 / 3 program: ${JSON.stringify(registryEntry).slice(0, 300)}`);
  step.screenshots.push(await shot(h.page, rc.name, `issue-${which}-result`));
  // the app tracks the wallet's token UTXO from the genesis (independent of the node check above)
  const tracked = await trackedTokens(h);
  const mine = tracked.filter((x) => x.txid === s.txid && x.token === tokenId);
  if (!mine.length || mine[0].program !== 'KCC20Ref' || BigInt(mine[0].amount) !== SUPPLY * TOKEN) throw new Error(`the app did not track the issued token UTXO (tracked: ${JSON.stringify(tracked).slice(0, 400)})`);
  step.tracked = mine;
  log(`[${rc.name}] issued ${ticker} (3/3) token ${short(tokenId)} in ${Date.now() - t0} ms, tx ${short(s.txid)}`);
  return { ticker, tokenId, registryEntry, issueTxid: s.txid, supply: String(SUPPLY) };
}

/** Issues token A (and token B, the quote of the pair scenario, when `withPair`). */
export async function issueTokens(rc, { withPair }) {
  let token = null;
  let tokenB = null;
  const rec = await scenario(rc, 'issue', async (r) => {
    token = await issueOne(rc, r, 'A');
    if (withPair) tokenB = await issueOne(rc, r, 'B');
  });
  return { token, tokenB, scenario: rec };
}

// ------------------------------------------------------------------------------------------------ the order ticket

/** The `data-testid` of a ticket field (form-state.ts `FIELDS`): amount / price / tip have the short order-* ids, the rest `field-<intent path>`. */
const fieldTestId = (id) => (id === 'amount' ? 'order-amount' : id === 'price' ? 'order-price' : id === 'tip' ? 'order-tip' : `field-${id}`);

async function fillFields(h, values) {
  for (const [id, value] of Object.entries(values)) {
    const loc = tid(h, fieldTestId(id));
    try {
      await loc.waitFor({ state: 'attached', timeout: 10_000 });
    } catch {
      throw new Error(`the ticket has no field "${id}" (${fieldTestId(id)}) for this order type`);
    }
    if (!(await loc.isVisible())) {
      const box = tid(h, 'order-advanced');
      if ((await box.getAttribute('open')) === null) await box.locator('summary').click();
    }
    const tag = await loc.evaluate((el) => `${el.tagName}:${el.type ?? ''}`);
    if (tag.startsWith('SELECT')) await loc.selectOption(String(value));
    else if (tag === 'INPUT:checkbox') {
      if (value) await loc.check();
      else await loc.uncheck();
    } else await loc.fill(String(value));
  }
}

/**
 * The covenant-signing gate (C5-10): a wallet not known to sign covenant inputs (Kastle) is asked once for a TEST signature before its first
 * order. Runs it through the wallet; a wallet that cannot sign a cancel is refused by the app for good: that is a finding, thrown.
 */
async function passCovenantGate(rc) {
  const { h } = rc;
  if (!(await tid(h, 'order-covenant-check').count())) return null;
  const { popups } = await rc.drv.approve(rc.ctx, async () => {
    await tid(h, 'order-covenant-check-run').click();
    const t0 = Date.now();
    while (Date.now() - t0 < 180_000) {
      if (await tid(h, 'order-covenant-unsupported').count()) return;
      if (!(await tid(h, 'order-covenant-check').count())) return;
      await sleep(500);
    }
  });
  if (await tid(h, 'order-covenant-unsupported').count()) throw new Error(`the app refuses ${rc.drv.label} for orders: ${collapse(await tid(h, 'order-covenant-unsupported').innerText())}`);
  if (await tid(h, 'order-covenant-check').count()) throw new Error(`the covenant-signing check of ${rc.drv.label} did not pass: ${collapse(await tid(h, 'order-covenant-check').innerText())}`);
  return { popups: popupView(popups) };
}

/** Opens the KAS market of token A (or the pair A/B with `pair`) and waits for the ticket. */
async function openTicket(rc, { pair = false } = {}) {
  const { h } = rc;
  const route = pair ? `#/market/${rc.token.tokenId}/${rc.tokenB.tokenId}` : `#/market/${rc.token.tokenId}`;
  await goto(h, route);
  await tid(h, 'order-ticket').waitFor({ timeout: 60_000 });
  if ((await tid(h, 'order-ticket').getAttribute('data-state')) === 'untradable') throw new Error(`the ticket says the token is untradable: ${collapse(await tid(h, 'order-untradable').innerText())}`);
  if (pair) {
    const shown = await tid(h, 'market-pair').getAttribute('data-pair').catch(() => null);
    if (shown !== `${rc.token.ticker}/${rc.tokenB.ticker}`) throw new Error(`the pair page shows "${shown}", expected ${rc.token.ticker}/${rc.tokenB.ticker}`);
  }
}

/**
 * Fills the order ticket until its order button is enabled. spec: { type, side, fields: {intent path: value}, pair?, expectSummary?: RegExp }.
 * Passes the covenant check when shown. Returns how to press the button.
 */
async function fillTicket(rc, spec) {
  const { h } = rc;
  await openTicket(rc, { pair: !!spec.pair });
  const ticket = tid(h, 'order-ticket');
  await tid(h, 'order-type').selectOption(spec.type);
  if ((await ticket.getAttribute('data-type')) !== spec.type) throw new Error(`the ticket did not switch to ${spec.type}`);
  await tid(h, spec.side === 'buy' ? 'order-side-buy' : 'order-side-sell').click();
  if ((await ticket.getAttribute('data-side')) !== spec.side) throw new Error(`the ticket did not switch to the ${spec.side} side`);
  await fillFields(h, spec.fields);
  const gate = await passCovenantGate(rc);
  const review = tid(h, 'order-place');
  for (let i = 0; i < 90; i++) {
    if ((await ticket.getAttribute('aria-busy')) === 'false' && (await review.isEnabled())) break;
    await sleep(1000);
  }
  if (!(await review.isEnabled())) {
    const issues = await tid(h, 'order-issues').innerText().catch(() => '');
    const needs = await tid(h, 'order-needs').innerText().catch(() => '');
    throw new Error(`the order button stays disabled. issues: ${collapse(issues)} needs: ${collapse(needs)}`);
  }
  const issues = await h.page.evaluate(() => [...document.querySelectorAll('[data-testid="order-issues"] li')].map((li) => ({ code: li.dataset.code, severity: li.dataset.severity })));
  // what the ticket says it orders: the side, the type and the figures of its disclosure
  const typeText = collapse(await tid(h, 'order-type').locator('option:checked').innerText().catch(() => ''));
  const disclosure = `${spec.side} ${typeText}: ${collapse(await tid(h, 'order-disclosure').innerText().catch(() => ''))}`;
  const balancesBefore = collapse(await tid(h, 'order-balances').innerText().catch(() => ''));
  return { disclosure, balancesBefore, issues, gate, click: () => review.click() };
}

/** Place through the ticket, sign, verify on the node. Returns the step record plus what the cancel needs. */
async function place(rc, r, label, spec) {
  const before = new Set(Object.keys(await readRecords(rc.h)));
  const t = await fillTicket(rc, spec);
  const s = await signFromTicket(rc.h, rc.ctx, rc.drv, label, rc.name, t.click);
  if (!s.submitted) throw new Error('the submitted transaction was not captured from the node connection');
  // what was ordered is said by the ticket's own figures (there is no confirmation screen)
  if (spec.expectSummary && !spec.expectSummary.test(t.disclosure)) throw new Error(`the ticket does not say what is ordered (${spec.expectSummary}): ${t.disclosure.slice(0, 400)}`);
  const step = { ...stepOf(`place ${label}`, s), disclosure: t.disclosure, ticketIssues: t.issues, covenantGate: t.gate ?? undefined, balancesBefore: t.balancesBefore };
  r.steps.push(step);
  step.verify = await verifyCreated(rc, s.submitted, s.txid, label);
  await sleep(1500);
  const after = await readRecords(rc.h);
  const created = Object.entries(after).filter(([id]) => !before.has(id));
  step.records = created.map(([id, rec]) => ({ covenantId: id, kind: rec.kind, txid: rec.txid, output: rec.output }));
  if (!created.length) throw new Error('the app stored no placement record for the accepted transaction');
  // the record the app keeps must point at a covenant outpoint that the node lists as unspent (verified above)
  const p2sh = new Set(outputsOf(rc.node, s.submitted).filter((o) => o.p2sh).map((o) => o.index));
  for (const [id, rec] of created) if (rec.txid !== s.txid || !p2sh.has(rec.output)) throw new Error(`placement record of ${short(id)} points at ${short(rec.txid)}:${rec.output}, not at a covenant output of the accepted transaction`);
  return { step, tx: s.submitted, txid: s.txid, covenantIds: created.map(([id]) => id) };
}

// ------------------------------------------------------------------------------------------------ My orders

/** Opens My orders afresh (the view is re-mounted, no flow of an earlier action stays on it) and waits for the rows of `wantIds`. */
async function openOrders(rc, wantIds = [], { timeoutMs = 90_000 } = {}) {
  const { h } = rc;
  await goto(h, '#/settings');
  await goto(h, '#/orders');
  await tid(h, 'orders-view').waitFor({ timeout: 60_000 });
  await tid(h, 'orders-list').waitFor({ timeout: 60_000 });
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

/** What My orders shows of an order row (type, side, status, live, prices, amount). */
async function rowView(h, id) {
  const row = tid(h, `order-row-${id}`);
  if (!(await row.count())) return { id, listed: false };
  const txt = async (t) => collapse(await row.getByTestId(t).first().innerText().catch(() => ''));
  return {
    id, listed: true, type: await row.getAttribute('data-type'), side: await row.getAttribute('data-side'), status: await row.getAttribute('data-status'), live: (await row.getAttribute('data-live')) === '1',
    price: await txt('order-price'), stop: await txt('order-stop'), amount: await txt('order-amount'), source: await txt('order-source'),
  };
}

/** Waits until none of `ids` is live in My orders (refreshing), returns the final row views. */
async function waitNotLive(rc, ids, timeoutMs = 90_000) {
  const { h } = rc;
  const t0 = Date.now();
  for (;;) {
    const views = [];
    for (const id of ids) views.push(await rowView(h, id));
    if (views.every((v) => !v.listed || !v.live)) return views;
    if (Date.now() - t0 > timeoutMs) throw new Error(`the app still lists the order(s) as live: ${JSON.stringify(views)}`);
    const refresh = tid(h, 'orders-refresh');
    if (await refresh.count()) await refresh.click().catch(() => {});
    await sleep(3000);
  }
}

/** Token amounts the app's tracker holds from `txid` (a cancel / merge output), base units, for `token`. */
async function trackedFrom(h, txid, token) {
  await sleep(1000);
  const items = (await trackedTokens(h)).filter((x) => x.txid === txid && x.token === token);
  return { items, total: items.reduce((a, x) => a + BigInt(x.amount ?? 0), 0n) };
}

/**
 * Cancels through My orders: the row's cancel (or the position card's, when the indexer groups an IFD), the flow's confirmation screen, the
 * node check, the tokens released to the wallet (tracked by the app), the order leaving the live list.
 */
async function cancelStep(rc, r, label, { placed, covenantIds, position = false, tokensBack = null }) {
  const { h } = rc;
  await openOrders(rc, covenantIds);
  const rows = [];
  for (const id of covenantIds) rows.push(await rowView(h, id));
  const shot1 = await shot(h.page, rc.name, `${label}-orders-listed`);
  if (rows.some((x) => !x.live)) throw new Error(`My orders does not list the placed order(s) as live: ${JSON.stringify(rows)}`);
  const balancesPanel = collapse(await tid(h, 'balances-panel').innerText().catch(() => ''));
  if (position && (await h.page.locator('[data-testid^="position-cancel-"]').count())) await h.page.locator('[data-testid^="position-cancel-"]').first().click();
  else {
    const btn = tid(h, `order-cancel-${covenantIds[0]}`);
    if (!(await btn.isEnabled())) throw new Error(`the cancel button of ${short(covenantIds[0])} is disabled: ${await btn.getAttribute('title')}`);
    await btn.click();
  }
  const { steps } = await runFlow(h, rc.ctx, rc.drv, `${label}-cancel`, rc.name);
  const s = steps.at(-1);
  if (!s.submitted) throw new Error('the cancel transaction was not captured from the node connection');
  const step = { ...stepOf(`cancel ${label}`, s), ordersBefore: rows, balancesPanelBeforeCancel: balancesPanel };
  step.screenshots.unshift(shot1);
  r.steps.push(step);
  step.verify = await verifySpent(rc, s.submitted, s.txid, placed);
  if (tokensBack) {
    // tokens released from custody come back as a wallet-owned token UTXO of the cancel tx: the app's tracker (each entry re-verified on the node) must hold them
    const got = await trackedFrom(h, s.txid, tokensBack.token);
    step.tokensBack = { expectedAtLeast: tokensBack.amount.toString(), trackedFromCancel: got.items };
    if (got.total < tokensBack.amount) throw new Error(`the cancel should release ${tokensBack.amount} base units to the wallet, the app tracks ${got.total} (${JSON.stringify(got.items)})`);
  }
  await openOrders(rc);
  step.ordersAfter = await waitNotLive(rc, covenantIds);
  step.screenshots.push(await shot(h.page, rc.name, `${label}-orders-after`));
  return step;
}

/** Token balance text of the ticket (available / in orders), for the record. */
async function ticketBalances(rc, pair = false) {
  await openTicket(rc, { pair });
  await sleep(2500);
  return collapse(await tid(rc.h, 'order-balances').innerText().catch(() => ''));
}

async function placeAndCancel(rc, r, label, spec, { position = false, tokensBack = null, whileOpen = null } = {}) {
  const p = await place(rc, r, label, spec);
  if (whileOpen) await whileOpen(p);
  const c = await cancelStep(rc, r, label, { placed: new Map([[p.txid, p.tx]]), covenantIds: p.covenantIds, position, tokensBack });
  c.balancesAfter = await ticketBalances(rc, !!spec.pair);
  log(`[${rc.name}] ${label}: placed ${short(p.txid)}, cancelled ${short(c.txid)}`);
  return { p, c };
}

/** The Balances panel of My orders: free / in orders / strays of a token (whole tokens, exact `data-value`s) and the KAS lines. */
async function balanceRow(rc, token, { timeoutMs = 60_000 } = {}) {
  const { h } = rc;
  const row = tid(h, `balance-row-${token}`);
  const t0 = Date.now();
  while (!(await row.count())) {
    if (Date.now() - t0 > timeoutMs) throw new Error(`the Balances panel has no row for ${short(token)}: ${collapse(await tid(h, 'balances-panel').innerText().catch(() => '')).slice(0, 500)}`);
    await sleep(1000);
  }
  const val = async (loc) => (await loc.locator('[data-value]').first().getAttribute('data-value').catch(() => null)) ?? collapse(await loc.innerText().catch(() => ''));
  return {
    free: await val(row.locator('td').nth(1)),
    escrowed: await val(row.getByTestId('balance-escrowed')),
    strays: await val(row.getByTestId('balance-strays')),
    kasLocked: await val(row.locator('td').nth(4)),
    mergeButton: (await tid(h, `orders-merge-token-${token}`).count()) ? collapse(await tid(h, `orders-merge-token-${token}`).innerText()) : null,
  };
}

const num = (s) => Number(String(s ?? '').replace(/,/g, ''));

/** Polls the Balances panel until `pred(row)` holds (the free / escrowed figures follow the node with a delay). */
async function waitBalance(rc, token, pred, what, timeoutMs = 90_000) {
  const t0 = Date.now();
  let b = null;
  for (;;) {
    b = await balanceRow(rc, token);
    if (pred(b)) return b;
    if (Date.now() - t0 > timeoutMs) throw new Error(`Balances panel: ${what} not reached after ${timeoutMs} ms (${JSON.stringify(b)})`);
    await openOrders(rc);
    await sleep(2000);
  }
}

/** Best-effort sweep at the end of a run: cancels any order still live (a failed scenario must not leave the test wallet's KAS locked). */
export async function sweepOpenOrders(rc) {
  const { h } = rc;
  const done = [];
  try {
    for (let round = 0; round < 8; round++) {
      await openOrders(rc);
      await sleep(5000);
      const ids = await h.page.evaluate(() => [...document.querySelectorAll('[data-testid^="order-row-"][data-live="1"]')].map((e) => e.getAttribute('data-testid').slice('order-row-'.length)));
      const id = ids.find(Boolean);
      if (!id) break;
      const btn = tid(h, `order-cancel-${id}`);
      if (!(await btn.count()) || !(await btn.isEnabled())) {
        done.push({ orderId: id, skipped: 'no enabled cancel button' });
        break;
      }
      await btn.click();
      const { steps } = await runFlow(h, rc.ctx, rc.drv, 'sweep-cancel', rc.name);
      done.push({ orderId: id, txids: steps.map((s) => s.txid) });
    }
  } catch (e) {
    done.push({ error: String(e?.message ?? e).slice(0, 300) });
  }
  return done;
}

// ------------------------------------------------------------------------------------------------ scenarios

const A = (rc, n) => ({ token: rc.token.tokenId, amount: BigInt(n) * TOKEN });

/** Limit GTC sell (rests far above any market; the Balances panel shows the tokens in the order) and limit GTC buy, each placed and cancelled. */
export async function runLimit(rc) {
  return scenario(rc, 'limit', async (r) => {
    await placeAndCancel(rc, r, 'limit-sell', { type: 'limit', side: 'sell', fields: { amount: 3, price: 50 }, expectSummary: /sell/i }, {
      tokensBack: A(rc, 3),
      whileOpen: async () => {
        await openOrders(rc);
        const b = await waitBalance(rc, rc.token.tokenId, (x) => num(x.escrowed) === 3, '3 tokens in orders');
        r.steps.push({ name: 'balances while the sell is open', balances: b });
      },
    });
    await placeAndCancel(rc, r, 'limit-buy', { type: 'limit', side: 'buy', fields: { amount: 2, price: 0.5 }, expectSummary: /buy/i });
  });
}

/** Amend in place (AMEND record): a limit sell gets a new price; the same order (covenant id) lives on at a new outpoint; then cancelled. */
export async function runAmend(rc) {
  return scenario(rc, 'amend', async (r) => {
    const { h } = rc;
    const p = await place(rc, r, 'amend-sell', { type: 'limit', side: 'sell', fields: { amount: 2, price: 50 }, expectSummary: /sell/i });
    const id = p.covenantIds[0];
    await openOrders(rc, [id]);
    const before = await rowView(h, id);
    const btn = tid(h, `order-replace-${id}`);
    if (!(await btn.count())) throw new Error(`My orders offers no amend for the limit sell ${short(id)}: ${JSON.stringify(before)}`);
    await btn.click();
    await tid(h, 'amend-dialog').waitFor({ timeout: 30_000 });
    await tid(h, 'amend-price').fill('55');
    await tid(h, 'amend-preview').waitFor({ timeout: 30_000 });
    const preview = collapse(await tid(h, 'amend-preview').innerText());
    if (!/55/.test(preview)) throw new Error(`the amend preview does not show the new price: ${preview}`);
    await tid(h, 'amend-review').click();
    const { steps } = await runFlow(h, rc.ctx, rc.drv, 'amend', rc.name);
    const s = steps.at(-1);
    if (!s.submitted) throw new Error('the amend transaction was not captured');
    const step = { ...stepOf('amend price 50 -> 55', s), rowBefore: before, preview };
    r.steps.push(step);
    step.verify = await verifySpent(rc, s.submitted, s.txid, new Map([[p.txid, p.tx]]), 'amend');
    // in place: the same covenant id, still live, at the new price
    await openOrders(rc, [id]);
    const t0 = Date.now();
    let after = await rowView(h, id);
    while (!(after.live && /55/.test(after.price)) && Date.now() - t0 < 90_000) {
      await sleep(3000);
      await openOrders(rc, [id]);
      after = await rowView(h, id);
    }
    step.rowAfter = after;
    if (!after.live || !/55/.test(after.price)) throw new Error(`after the amend My orders shows ${JSON.stringify(after)}, expected the same order live at 55`);
    const c = await cancelStep(rc, r, 'amend-sell', { placed: new Map([[s.txid, s.submitted]]), covenantIds: [id], tokensBack: A(rc, 2) });
    log(`[${rc.name}] amend: placed ${short(p.txid)}, amended ${short(s.txid)}, cancelled ${short(c.txid)}`);
  });
}

/** IOC buy below any ask (nothing fills): lives for its 60 s life; cancelled at once. */
export async function runIoc(rc) {
  return scenario(rc, 'ioc', async (r) => {
    await placeAndCancel(rc, r, 'ioc-buy', { type: 'ioc', side: 'buy', fields: { amount: 2, price: 0.4, life: 60 }, expectSummary: /buy/i });
  });
}

/** Stop (market) sell far below: waits unarmed; cancelled, the tokens come back. */
export async function runStop(rc) {
  return scenario(rc, 'stop', async (r) => {
    await placeAndCancel(rc, r, 'stop-sell', { type: 'stopMarket', side: 'sell', fields: { amount: 2, stop: 0.2 }, expectSummary: /stop/i }, { tokensBack: A(rc, 2) });
  });
}

/** OCO sell: take-profit far above, stop far below; cancelled, the tokens come back. */
export async function runOco(rc) {
  return scenario(rc, 'oco', async (r) => {
    await placeAndCancel(rc, r, 'oco-sell', { type: 'oco', side: 'sell', fields: { amount: 2, takeProfit: 60, stop: 0.2 }, expectSummary: /OCO/i }, { tokensBack: A(rc, 2) });
  });
}

/** IFD buy-first (buy 2 at 0.5, exit take-profit 1): one entry, cancelled from its row (node only) or its position card (indexer). */
export async function runIfd(rc) {
  return scenario(rc, 'ifd', async (r) => {
    await placeAndCancel(rc, r, 'ifd-buy-first', { type: 'ifd', side: 'buy', fields: { amount: 2, price: 0.5, 'exit.takeProfit': 1 }, expectSummary: /IFD/i }, { position: true });
  });
}

/** Pair A/B limit orders: a sell of A for B (escrows A) and a buy of A with B (escrows B), each placed from the pair page and cancelled. */
export async function runPair(rc) {
  return scenario(rc, 'pair', async (r) => {
    if (!rc.tokenB) throw new Error('the pair scenario needs token B (issued with --only including pair)');
    const B = (n) => ({ token: rc.tokenB.tokenId, amount: n });
    await placeAndCancel(rc, r, 'pair-limit-sell', { pair: true, type: 'limit', side: 'sell', fields: { amount: 2, price: 5 }, expectSummary: new RegExp(`${rc.tokenB.ticker}`) }, { tokensBack: A(rc, 2) });
    // a buy of 1 A at 0.5 B: at least 0.5 B escrowed
    await placeAndCancel(rc, r, 'pair-limit-buy', { pair: true, type: 'limit', side: 'buy', fields: { amount: 1, price: 0.5 }, expectSummary: new RegExp(`buys ${rc.token.ticker} with ${rc.tokenB.ticker}`) }, { tokensBack: B(TOKEN / 2n) });
  });
}

/**
 * Market sell against a counterparty: the market needs a book (an indexer) with a bid that is not the wallet's own (self-trade prevention).
 * The counterparty is the DEV key (the `maker` test key with `--mock`), driven through the same app in a separate browser with an in-page
 * signer: it rests a limit buy of 1 token at 0.6, the wallet market-sells 1 token into it. A matcher on the network may fill the market order
 * (then it is reported filled); otherwise it is cancelled. The counterparty's bid is cancelled at the end if it is still live.
 */
export async function runMarket(rc) {
  return scenario(rc, 'market', async (r) => {
    if (!rc.indexer) throw new Skip('a market order needs an indexer with a book (set INDEXER_URL in e2e-real/.env or pass --indexer <url>; --mock --indexer uses the mock indexer)');
    if (!rc.openCounterparty) throw new Skip('no counterparty key (DEV_PRIVATE_KEY) for the resting bid');
    const cp = await rc.openCounterparty();
    try {
      // the counterparty's resting bid
      const cpRc = { ...rc, ...cp, name: `${rc.name}-counterparty` };
      const bid = await place(cpRc, r, 'counterparty-bid', { type: 'limit', side: 'buy', fields: { amount: 1, price: 0.6 }, expectSummary: /buy/i });
      // the wallet's market sell, once the indexer's book shows the bid (NO_LIQUIDITY until then)
      const t0 = Date.now();
      for (;;) {
        await openTicket(rc);
        await tid(rc.h, 'order-type').selectOption('market');
        await tid(rc.h, 'order-side-sell').click();
        await fillFields(rc.h, { amount: 1 });
        await sleep(3000);
        if (!(await tid(rc.h, 'order-issue-NO_LIQUIDITY').count())) break;
        if (Date.now() - t0 > 120_000) throw new Error('the indexer book never showed the counterparty bid (NO_LIQUIDITY after 120 s)');
      }
      const p = await place(rc, r, 'market-sell', { type: 'market', side: 'sell', fields: { amount: 1, life: 60 }, expectSummary: /market/i });
      const placedStep = r.steps.at(-1);
      if (!/0\.6\b/.test(placedStep.disclosure)) throw new Error(`the market disclosure does not start from the counterparty bid 0.6: ${placedStep.disclosure.slice(0, 400)}`);
      await openOrders(rc, p.covenantIds);
      const view = await rowView(rc.h, p.covenantIds[0]);
      r.steps.push({ name: 'market order in My orders', row: view });
      if (view.live) await cancelStep(rc, r, 'market-sell', { placed: new Map([[p.txid, p.tx]]), covenantIds: p.covenantIds, tokensBack: A(rc, 1) });
      else log(`[${rc.name}] market sell ${short(p.covenantIds[0])} is ${view.status} (filled by a matcher?): not cancelled`);
      // the counterparty's bid: cancelled when still live
      await openOrders(cpRc, bid.covenantIds);
      if ((await rowView(cp.h, bid.covenantIds[0])).live) await cancelStep(cpRc, r, 'counterparty-bid', { placed: new Map([[bid.txid, bid.tx]]), covenantIds: bid.covenantIds });
    } finally {
      await cp.close().catch(() => {});
    }
  });
}

/**
 * Merge (My orders > Balances): after the cancels the wallet holds several plain UTXOs of token A; "Merge (n)" joins them in a chain of
 * transfers to itself (at most 3 token inputs each, the 3 / 3 program), each on its own confirmation screen. The last link's token output is on
 * the node, every merged input is spent, the button disappears, the free balance is the whole supply.
 */
export async function runMerge(rc) {
  return scenario(rc, 'merge', async (r) => {
    const { h } = rc;
    const token = rc.token.tokenId;
    await openOrders(rc);
    const before = await waitBalance(rc, token, (b) => !!b.mergeButton, 'a "Merge" button (two or more plain UTXOs)', 60_000);
    const n = Number(/\((\d+)\)/.exec(before.mergeButton)?.[1] ?? NaN);
    if (!(n >= 2)) throw new Error(`the merge button does not state the UTXO count: "${before.mergeButton}"`);
    const trackedBefore = (await trackedTokens(h)).filter((x) => x.token === token);
    await tid(h, `orders-merge-token-${token}`).click();
    const { steps, done } = await runFlow(h, rc.ctx, rc.drv, 'merge', rc.name);
    const expectLinks = n <= 3 ? 1 : 1 + Math.ceil((n - 3) / 2);
    const step = { name: `merge ${n} UTXOs`, balancesBefore: before, trackedBefore, links: steps.map((s) => stepOf('merge link', s)), done };
    r.steps.push(step);
    if (steps.length !== expectLinks) throw new Error(`a merge of ${n} UTXOs should take ${expectLinks} transaction(s), the flow had ${steps.length}`);
    if (!new RegExp(`Merge ${n} ${rc.token.ticker} UTXOs into 1`).test(steps[0].shownAs)) throw new Error(`the merge flow does not state the merge: "${steps[0].shownAs}"`);
    // node: every merged input is spent, the last link's single token output is unspent
    const last = steps.at(-1);
    const outs = outputsOf(rc.node, last.submitted).filter((o) => o.p2sh && o.address);
    if (outs.length !== 1) throw new Error(`the last merge link has ${outs.length} covenant outputs, expected 1`);
    const w = await rc.node.waitOutpoint(outs[0].address, last.txid, outs[0].index, true);
    step.verify = [{ what: `merged output ${short(last.txid)}:${outs[0].index}`, unspent: w.ok, ms: w.ms }];
    if (!w.ok) throw new Error(`node check failed: the merged output ${last.txid}:${outs[0].index} is not unspent`);
    const got = await trackedFrom(h, last.txid, token);
    step.trackedAfter = got.items;
    if (got.total !== SUPPLY * TOKEN) throw new Error(`the merged UTXO should hold the whole supply (${SUPPLY * TOKEN}), the app tracks ${got.total}`);
    await openOrders(rc);
    step.balancesAfter = await waitBalance(rc, token, (b) => !b.mergeButton && num(b.free) === Number(SUPPLY), 'one UTXO with the whole supply');
    log(`[${rc.name}] merge: ${n} UTXOs in ${steps.length} transaction(s), last ${short(last.txid)}`);
  });
}

/** Balances at the end: every token back in the wallet (free = supply, nothing in orders, no strays), no KAS locked in orders. */
export async function runBalances(rc) {
  return scenario(rc, 'balances', async (r) => {
    await openOrders(rc);
    const rows = {};
    for (const t of [rc.token, rc.tokenB].filter(Boolean)) {
      rows[t.ticker] = await waitBalance(rc, t.tokenId, (b) => num(b.free) === Number(SUPPLY) && num(b.escrowed) === 0 && num(b.strays) === 0, `${t.ticker}: free ${SUPPLY}, nothing in orders`);
    }
    const kasLocked = collapse(await tid(rc.h, 'kas-locked').innerText().catch(() => ''));
    const live = await rc.h.page.locator('[data-testid^="order-row-"][data-live="1"]').count();
    r.steps.push({ name: 'balances', rows, kasLocked, liveOrders: live });
    if (live) throw new Error(`${live} order(s) still live in My orders`);
  });
}

/** Scenario ids in run order, with the function and whether it needs token B. */
export const SCENARIOS = [
  ['limit', runLimit],
  ['amend', runAmend],
  ['ioc', runIoc],
  ['stop', runStop],
  ['oco', runOco],
  ['ifd', runIfd],
  ['pair', runPair],
  ['market', runMarket],
  ['merge', runMerge],
  ['balances', runBalances],
];
