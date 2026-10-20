// Building blocks of the real-wallet run: the independent node checker, the wallet drivers (KasWare / Kaspire / Kastle, real extensions or the
// in-page mock wallets of `--mock`) and the app driver (connect, the pre-sign confirmation screen, multi-step flows). Nothing runs on import.
// Secrets (mnemonics, keys) are never printed.
import { mkdirSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import {
  OUT_DIR, collapse, extensionId, kaspaNode, kaswareAddress, kaswareApprover, kaspireApprover, kastleApprover, log, onboardKasware, onboardKaspire,
  onboardKastle, shot, sleep, switchKaswareToTn10, withPopupApproval,
} from './common.mjs';
import * as T from '../../tools/wallet-gate/lib/txbuild.mjs';
import { TN10_NODE } from './fund.mjs';
import { installMockWallet } from './mock.mjs';

export const KASPIRE_PASSWORD = 'kob-real-kaspire-throwaway-2026'; // throwaway vault password of a throwaway profile
export const KASTLE_PASSWORD = 'kob-real-kastle-throwaway-2026'; // throwaway wallet password of a throwaway profile
export const KAS = 100_000_000n;
export const POPUP_TIMEOUT_MS = 180_000;
export const ACCEPT_TIMEOUT_MS = 90_000;

const P2SH_RE = /^aa20[0-9a-f]{64}87$/;
export const WALLET_LABELS = { kasware: 'KasWare', kaspire: 'Kaspire', kastle: 'Kastle' };

/** Retries `fn` once (a dropped node socket is the known transient failure); never loops. */
export async function retryOnce(label, fn) {
  try {
    return await fn();
  } catch (e) {
    log(`retry: ${label} failed once (${String(e?.message ?? e).slice(0, 160)}), trying again`);
    await sleep(2000);
    return await fn();
  }
}

// ------------------------------------------------------------------------------------------------ independent node checker

/** Reads the TN10 node directly through the official SDK, independently of the app under test. */
export class NodeChecker {
  constructor(nodeWs = TN10_NODE) {
    this.k = kaspaNode();
    this.nodeWs = nodeWs;
  }

  async #rpc() {
    return T.connectRpc(this.k, this.nodeWs, 'testnet-10');
  }

  async #with(fn) {
    return retryOnce('node read', async () => {
      const rpc = await this.#rpc();
      try {
        return await fn(rpc);
      } finally {
        try {
          await rpc.disconnect();
        } catch {
          /* ignore */
        }
      }
    });
  }

  async init() {
    return this;
  }

  async ping() {
    return this.#with(async (rpc) => (await rpc.getBlockDagInfo()).virtualDaaScore);
  }

  /** `[{txid, index, amount: bigint, covenantId}]` of an address. */
  async utxos(address) {
    return this.#with(async (rpc) => {
      const es = await T.utxosOf(rpc, address);
      return es.map((e) => {
        const ent = e.utxoEntry ?? e.entry ?? e;
        const op = e.outpoint;
        return { txid: op.transactionId, index: op.index, amount: BigInt(ent.amount), covenantId: ent.covenantId ?? null };
      });
    });
  }

  addressOfSpk(spk) {
    const version = typeof spk === 'string' ? parseInt(spk.slice(0, 4), 16) : Number(spk.version ?? 0);
    const script = typeof spk === 'string' ? spk.slice(4) : spk.scriptPublicKey ?? spk.script;
    const a = this.k.addressFromScriptPublicKey(new this.k.ScriptPublicKey(version, script), 'testnet-10');
    return a ? a.toString() : null;
  }

  /** Is `txid:index` (an output paying to `address`) unspent on the node right now? */
  async isUnspent(address, txid, index) {
    const us = await this.utxos(address);
    return us.some((u) => u.txid === txid && u.index === index);
  }

  /** Polls until `txid:index` is unspent (`want` true) or gone (false); returns {ok, ms}. */
  async waitOutpoint(address, txid, index, want, timeoutMs = ACCEPT_TIMEOUT_MS) {
    const t0 = Date.now();
    for (;;) {
      let now = null;
      try {
        now = await this.isUnspent(address, txid, index);
      } catch {
        /* transient: keep polling */
      }
      if (now === want) return { ok: true, ms: Date.now() - t0 };
      if (Date.now() - t0 > timeoutMs) return { ok: false, ms: Date.now() - t0 };
      await sleep(1500);
    }
  }
}

/**
 * Every output of a submitted tx (JSON from the wRPC frame on TN10, from the HTTP submit body in `--mock`) with its address and whether it is a
 * P2SH (covenant) output. `node` is a NodeChecker or a MockNodeChecker (address derivation).
 */
export function outputsOf(node, tx) {
  return (tx?.outputs ?? []).map((o, i) => {
    const spk = o.scriptPublicKey;
    const script = typeof spk === 'string' ? spk.slice(4) : spk?.script ?? spk?.scriptPublicKey ?? '';
    let address = null;
    try {
      address = node.addressOfSpk(spk);
    } catch {
      /* non standard */
    }
    return { index: i, value: BigInt(o.value ?? o.amount ?? 0), address, p2sh: P2SH_RE.test(script), covenant: o.covenant ?? null };
  });
}

/** The outpoints a submitted tx spends: `txid:index`. */
export const spentBy = (tx) => (tx?.inputs ?? []).map((inp) => {
  const op = inp.previousOutpoint ?? inp.previous_outpoint ?? inp; // wRPC frame: previousOutpoint; kob-wasm JSON (mock node): flat
  return `${op.transactionId ?? op.transaction_id}:${op.index}`;
});

// ------------------------------------------------------------------------------------------------ wallet drivers

const KASWARE_PASSWORD = 'Kob-Test-12345'; // throwaway UI password of the throwaway KasWare profile (same as common.mjs onboardKasware)

/** KasWare approver that first unlocks the wallet when the approval window opens on its lock screen (a re-opened profile / auto-lock). */
export const kaswareApproverWithUnlock = (opts = {}) => {
  const inner = kaswareApprover(opts);
  return async (pop, info) => {
    await pop.waitForLoadState('domcontentloaded').catch(() => {});
    await sleep(1200);
    const pw = pop.locator('input[type=password]');
    if ((await pw.count().catch(() => 0)) > 0 && /Unlock/i.test(await pop.innerText('body').catch(() => ''))) {
      await pw.first().fill(KASWARE_PASSWORD);
      const unlock = pop.getByText('Unlock', { exact: true });
      await unlock.last().click({ timeout: 5000 }).catch(() => pw.first().press('Enter'));
      await sleep(2500);
    }
    return inner(pop, info);
  };
};

/**
 * A wallet under test. `setup(ctx)` prepares it in the browser context (onboarding through the extension UI, or the mock wallet injection);
 * `approve(ctx, start)` runs `start()` (an app action that makes the wallet ask) while approving the wallet's windows, resolving with
 * `{result, popups}`. The mock wallets approve by themselves (no window): `popups` is empty.
 *
 *   real:  walletDriver('kaspire', env)
 *   mock:  walletDriver('kaspire', env, { mock: { secretKey } })
 */
export function walletDriver(name, env, { mock = null } = {}) {
  if (!WALLET_LABELS[name]) throw new Error(`unknown wallet ${name}`);
  const label = WALLET_LABELS[name];
  if (mock) {
    return {
      name,
      label: `${label} (mock)`,
      mock: true,
      expectedAddress: null,
      async setup(ctx) {
        const cfg = await installMockWallet(ctx, name, mock.secretKey);
        this.expectedAddress = cfg.addresses['testnet-10'];
        this.pubkey = cfg.pubkey;
        return {};
      },
      async approve(_ctx, start) {
        return { result: await start(), popups: [] };
      },
    };
  }
  const withApprover = (approver) => async (ctx, start) => withPopupApproval(ctx, name, start, approver());
  if (name === 'kasware') {
    const mnemonic = env.WALLET_MNEMONIC_KASWARE;
    return {
      name,
      label,
      mnemonicKey: 'WALLET_MNEMONIC_KASWARE',
      expectedAddress: mnemonic ? kaswareAddress(mnemonic) : null,
      approve: withApprover(() => kaswareApproverWithUnlock({ screenshotDir: 'kasware' })),
      async setup(ctx) {
        const wp = await onboardKasware(ctx, mnemonic.trim().split(/\s+/));
        const text = await switchKaswareToTn10(wp);
        if (!/kaspatest:/.test(text)) throw new Error('KasWare did not switch to Testnet 10 (no kaspatest address on the wallet screen)');
        return { walletPage: wp };
      },
    };
  }
  if (name === 'kaspire') {
    const mnemonic = env.WALLET_MNEMONIC_KASPIRE;
    return {
      name,
      label,
      mnemonicKey: 'WALLET_MNEMONIC_KASPIRE',
      expectedAddress: null, // derived by the wallet; read after connecting
      approve: withApprover(() => kaspireApprover({ password: KASPIRE_PASSWORD, screenshotDir: 'kaspire' })),
      async setup(ctx) {
        const id = await extensionId(ctx);
        await onboardKaspire(ctx, id, mnemonic, { password: KASPIRE_PASSWORD });
        return { extId: id };
      },
    };
  }
  // kastle: optional in the app (`features.kastle`); the run switches it on. Its network switch to TN10 is a popup of the connect step.
  const mnemonic = env.WALLET_MNEMONIC_KASTLE;
  return {
    name,
    label,
    mnemonicKey: 'WALLET_MNEMONIC_KASTLE',
    expectedAddress: null,
    approve: withApprover(() => kastleApprover({ password: KASTLE_PASSWORD, screenshotDir: 'kastle' })),
    async setup(ctx) {
      const id = await extensionId(ctx);
      await onboardKastle(ctx, id, { mnemonic, password: KASTLE_PASSWORD });
      return { extId: id };
    },
  };
}

// ------------------------------------------------------------------------------------------------ app driver

/**
 * Opens the app in a new page of `ctx` with the injected config (`window.__KOB_CONFIG__`); records console errors and every transaction the
 * app submits: wRPC `submitTransaction` frames (TN10) and `POST /node/submit` bodies (the mock node).
 */
export async function openApp(ctx, baseUrl, config) {
  const page = await ctx.newPage();
  const h = { page, consoleErrors: [], pageErrors: [], badResponses: [], submits: [], baseUrl };
  page.on('console', (m) => {
    if (m.type() === 'error') h.consoleErrors.push(m.text().slice(0, 400));
  });
  // a submit request carries no transaction id: the node's answer does (`transactionId`); it goes to the oldest submit still
  // without one (the app submits one transaction at a time and waits for the answer)
  const answer = (id) => {
    const s = h.submits.find((x) => x.tx && !x.txid);
    if (s && typeof id === 'string') s.txid = id;
  };
  page.on('response', (r) => {
    if (r.status() >= 400) h.badResponses.push(`${r.status()} ${r.url().replace(baseUrl, '')}`.slice(0, 200));
    if (r.request().method() === 'POST' && /\/node\/submit$/.test(new URL(r.url()).pathname) && r.ok()) {
      r.json().then((j) => answer(j.transactionId ?? j.txid)).catch(() => {});
    }
  });
  page.on('pageerror', (e) => h.pageErrors.push(String(e.message).slice(0, 400)));
  page.on('request', (req) => {
    if (req.method() !== 'POST' || !/\/node\/submit$/.test(new URL(req.url()).pathname)) return;
    try {
      h.submits.push({ at: Date.now(), tx: JSON.parse(req.postData() ?? '{}').transaction });
    } catch {
      /* not JSON */
    }
  });
  page.on('websocket', (ws) => {
    ws.on('framereceived', (f) => {
      const p = typeof f.payload === 'string' ? f.payload : '';
      if (!/"transactionId"/.test(p) || /"outputs"/.test(p)) return;
      try {
        // only a submit's answer: `{transactionId}` (a replacement also names the replaced one); a UTXO list names
        // transaction ids inside its entries, never at the top of its params
        const j = JSON.parse(p);
        const r = j.params ?? j;
        if (r && typeof r.transactionId === 'string' && Object.keys(r).every((k) => k === 'transactionId' || k === 'replacedTransaction')) answer(r.transactionId);
      } catch {
        /* not JSON */
      }
    });
    ws.on('framesent', (f) => {
      const p = typeof f.payload === 'string' ? f.payload : '';
      if (process.env.KOB_DEBUG_WS) log('ws-sent', typeof f.payload, String(f.payload).length, p.slice(0, 160));
      // wRPC uses numeric op codes: recognise the submit request by its payload (a transaction with inputs / outputs)
      if (!/"transaction"/.test(p) || !/"outputs"/.test(p)) return;
      try {
        const j = JSON.parse(p);
        const params = j.params ?? j;
        h.submits.push({ at: Date.now(), tx: params.transaction ?? params.tx ?? params });
      } catch {
        h.submits.push({ at: Date.now(), raw: p.slice(0, 200) });
      }
    });
  });
  await page.addInitScript((c) => {
    window.__KOB_CONFIG__ = c;
  }, config);
  await page.goto(baseUrl + '/');
  await page.getByTestId('main').waitFor({ timeout: 60_000 }).catch(() => {});
  return h;
}

export const goto = async (h, hash) => {
  await h.page.evaluate((x) => {
    location.hash = x;
  }, hash);
  await sleep(600);
};

const tid = (h, id) => h.page.getByTestId(id);

/** Connects the wallet through the app's header (approves the wallet's connect / network windows) and reads what the header shows. */
export async function connectWallet(h, ctx, wallet) {
  const btn = tid(h, `wallet-connect-${wallet.name}`);
  try {
    await btn.waitFor({ timeout: 60_000 });
  } catch {
    // say why: what the header shows, whether the page has the wallet's provider at all, and the page's errors (a provider script that threw)
    const seen = await h.page.evaluate((n) => ({ provider: typeof window[n], hint: document.querySelector('[data-testid="wallet-install-hint"]')?.textContent ?? null }), wallet.name).catch(() => ({}));
    throw new Error(`the app offers no ${wallet.label} connect button after 60 s: window.${wallet.name} is ${seen.provider}, header "${collapse(seen.hint ?? '')}", page errors ${JSON.stringify(h.pageErrors.slice(0, 3))}`);
  }
  const { popups } = await wallet.approve(ctx, async () => {
    await btn.click();
    await tid(h, 'wallet-address').waitFor({ timeout: POPUP_TIMEOUT_MS });
  });
  const address = await tid(h, 'wallet-address').getAttribute('data-value');
  // the header's wallet chip carries the wallet's network; a network badge (`wallet-network`) appears only when it differs from the app's
  const network = ((await tid(h, 'wallet-connected').getAttribute('data-network')) ?? '').trim();
  if ((await tid(h, 'wallet-network').count()) > 0) throw new Error(`the header shows a network mismatch (wallet ${network})`);
  return { address, network, popups, shownAddress: (await tid(h, 'wallet-address').innerText()).trim() };
}

/** The wallet's plain (non-covenant) KAS on the node, sompi. */
export async function kasBalance(node, address) {
  const us = await node.utxos(address);
  return us.filter((u) => !u.covenantId).reduce((a, u) => a + u.amount, 0n);
}

const CONFIRM_ERR = ['confirm-error', 'confirm-rejected'];

/**
 * The pre-sign screen of a token issuance -> wallet -> node acceptance round trip (orders have no such screen: `signFromTicket`, `runFlow`).
 * Precondition: `confirm-screen` is (about to be) open.
 * Ticks the "I checked" box, refuses BLOCKING findings, signs, waits for the transaction id and the app's acceptance status, closes the
 * screen. Throws with the app's own error text on any failure. `expectKind`: the decoded kind the screen must show (`issue`).
 */
export async function signOnConfirmScreen(h, ctx, wallet, label, shotDir, { expectKind } = {}) {
  const t0 = Date.now();
  const screen = tid(h, 'confirm-screen');
  await screen.waitFor({ timeout: 90_000 });
  await tid(h, 'confirm-summary').waitFor({ timeout: 60_000 });
  await sleep(300);
  const kind = await h.page.locator('[data-testid="confirm-screen"] .cf').getAttribute('data-kind').catch(() => null);
  const summary = collapse(await tid(h, 'confirm-summary').innerText().catch(() => ''));
  const fullText = await screen.innerText().catch(() => '');
  const screenshots = [await shot(h.page, shotDir, `${label}-confirm`)];
  if (await tid(h, 'confirm-blocking').count()) throw new Error(`the confirmation screen shows BLOCKING findings: ${collapse(await tid(h, 'confirm-blocking').innerText())}`);
  if (expectKind && kind !== expectKind) throw new Error(`the confirmation screen decodes the transaction as "${kind}", expected "${expectKind}": ${summary.slice(0, 300)}`);
  await tid(h, 'confirm-ack').check();
  const submitsBefore = h.submits.length;
  const tSign = Date.now();
  const { popups } = await wallet.approve(ctx, async () => {
    await tid(h, 'confirm-sign').click();
    const t1 = Date.now();
    for (;;) {
      if (await tid(h, 'tx-id').count()) return;
      for (const e of CONFIRM_ERR) {
        if (await tid(h, e).count()) throw new Error(`${e}: ${collapse(await tid(h, e).innerText())}`);
      }
      if (Date.now() - t1 > POPUP_TIMEOUT_MS + 60_000) throw new Error('timed out waiting for the transaction id');
      await sleep(400);
    }
  });
  const txid = await tid(h, 'tx-id').getAttribute('data-value');
  const signMs = Date.now() - tSign;
  // the app's own acceptance status (node UTXO polling), bounded
  let status = null;
  const t2 = Date.now();
  while (Date.now() - t2 < ACCEPT_TIMEOUT_MS) {
    status = await tid(h, 'tx-status').getAttribute('data-status').catch(() => null);
    if (status === 'confirmed') break;
    await sleep(1000);
  }
  screenshots.push(await shot(h.page, shotDir, `${label}-accepted`));
  const submitted = h.submits.slice(submitsBefore).at(-1)?.tx ?? null;
  await tid(h, 'confirm-close').click();
  await sleep(500);
  return { txid, kind, appStatus: status, summary, shownAs: '', acks: [], confirmText: fullText, popups, screenshots, signMs, totalMs: Date.now() - t0, submitted };
}

/** The phases of an order action's status line (`order-status` / `flow-status`) that end it without a transaction. */
const STOPPED = ['blocked', 'rejected', 'failed'];

/**
 * The app's final confirmation of a transaction for an external wallet (`confirm-order`, the order's details from the decoded transaction):
 * read and confirmed when it is open, its title, summary lines and fee returned; null when none is open (the private-key signer confirms in its own frame).
 */
async function confirmInApp(h, expect = null) {
  const popup = tid(h, 'confirm-order');
  if (!(await popup.count())) return null;
  // read in one pass (an absent part is '' at once, not a 30 s locator wait); `orders`: each order's title, token and terms
  const read = await popup.evaluate((el) => {
    const txt = (n) => (n?.textContent ?? '').replace(/\s+/g, ' ').trim();
    const q = (id) => el.querySelector(`[data-testid="${id}"]`);
    return {
      title: txt(q('confirm-order-title')), rows: txt(q('confirm-order-summary')), fee: txt(q('confirm-order-fee')),
      orders: [...el.querySelectorAll('[data-testid^="confirm-order-order-"]')].filter((o) => !/-exit$/.test(o.dataset.testid)).map((o) => (o.innerText ?? '').replace(/\s+/g, ' ').trim()),
    };
  });
  read.text = [read.title, read.rows, ...read.orders].join(' | ');
  // what the confirmation says is not what the scenario orders: cancelled here, nothing reaches the wallet or the chain
  if (expect && !expect.re.test(read.text)) {
    await tid(h, 'confirm-order-cancel').click().catch(() => {});
    throw new Error(`the confirmation does not say what is ordered (${expect.re}${expect.what ? `: ${expect.what}` : ''}): ${read.text.slice(0, 600)}`);
  }
  await tid(h, 'confirm-order-ok').click();
  return read;
}

/** The phase of a status line (`order-status` / `flow-status`) now, null when there is none (no locator wait). */
const phaseOf = (h, id) => h.page.evaluate((id) => document.querySelector(`[data-testid="${id}"]`)?.getAttribute('data-phase') ?? null, id).catch(() => null);

/** The status line's sentence when it stopped (`send-error`), with its code. */
async function statusError(h, scope) {
  const line = tid(h, scope);
  const err = line.locator('[data-testid="send-error"]');
  const code = await err.getAttribute('data-code').catch(() => null);
  return `${scope} ${await line.getAttribute('data-phase').catch(() => '?')} (${code ?? '-'}): ${collapse(await err.innerText().catch(() => ''))}`;
}

/**
 * An order of the ticket: `click()` presses the order button, which checks the order silently and shows the final confirmation (the app's
 * popup for an external wallet, confirmed here; the private-key signer's frame), then asks the wallet. Approves the wallet's popup(s), waits for the transaction id on the ticket's status line (`order-status`) and the app's acceptance
 * status. Throws with the app's own sentence when it stops (a blocking check, a rejection, a failure).
 */
export async function signFromTicket(h, ctx, wallet, label, shotDir, click, { expect = null } = {}) {
  const t0 = Date.now();
  const line = tid(h, 'order-status');
  const submitsBefore = h.submits.length;
  const receipts = [];
  const { popups } = await wallet.approve(ctx, async () => {
    await click();
    const t1 = Date.now();
    for (;;) {
      const phase = await phaseOf(h, 'order-status');
      // `expect` is checked on the first confirmation only (a re-timed auction shows the same terms with later times)
      if (phase === 'confirming') receipts.push(await confirmInApp(h, receipts.some(Boolean) ? null : expect));
      if (phase === 'submitted') return;
      if (STOPPED.includes(phase)) throw new Error(await statusError(h, 'order-status'));
      if (Date.now() - t1 > POPUP_TIMEOUT_MS + 60_000) throw new Error('timed out waiting for the transaction id');
      await sleep(400);
    }
  });
  const txid = await line.locator('[data-testid="tx-id"]').getAttribute('data-value');
  const signMs = Date.now() - t0;
  // the app's own acceptance status (node UTXO polling), bounded
  let status = null;
  const t2 = Date.now();
  while (Date.now() - t2 < ACCEPT_TIMEOUT_MS) {
    status = await line.locator('[data-testid="tx-status"]').getAttribute('data-status').catch(() => null);
    if (status === 'confirmed') break;
    await sleep(1000);
  }
  const screenshots = [await shot(h.page, shotDir, `${label}-accepted`)];
  const submitted = h.submits.slice(submitsBefore).at(-1)?.tx ?? null;
  return { txid, kind: null, appStatus: status, summary: collapse(await line.innerText().catch(() => '')), receipts: receipts.filter(Boolean), shownAs: '', acks: [], popups, screenshots, signMs, totalMs: Date.now() - t0, submitted };
}

/**
 * Runs a My orders transaction flow (cancel, cancel of a position, merge ...) that the action's button has just started: every step shows its
 * final confirmation (confirmed here) and asks the wallet, the next one after each submission. Approves the popups until the flow shows its result and returns one step
 * per submitted transaction. Throws when the flow stops or ends with an error. Open My orders fresh before the action (scenarios.mjs
 * `openOrders`): an error banner of an earlier flow on the same page would be taken for this one's.
 */
export async function runFlow(h, ctx, wallet, label, shotDir, { maxSteps = 12 } = {}) {
  const t0 = Date.now();
  const submitsBefore = h.submits.length;
  let shownAs = '';
  let done = '';
  // what the flow says it does (`flow-shown-as`, shown only while a step runs): read whenever it is there, and always before a step is confirmed
  const readShownAs = async () => {
    if (shownAs) return;
    shownAs = await h.page.evaluate(() => (document.querySelector('[data-testid="flow-shown-as"]')?.textContent ?? '').replace(/\s+/g, ' ').trim());
  };
  const { popups } = await wallet.approve(ctx, async () => {
    for (;;) {
      await readShownAs();
      if (await tid(h, 'flow-done').count()) {
        done = collapse(await tid(h, 'flow-done').innerText());
        return;
      }
      const phase = await phaseOf(h, 'flow-status');
      if (phase === 'confirming') {
        await readShownAs();
        await confirmInApp(h);
      }
      if (STOPPED.includes(phase)) throw new Error(await statusError(h, 'flow-status'));
      for (const e of ['flow-error', 'flow-stopped', 'flow-plan-errors', 'orders-action-error']) {
        if (await tid(h, e).count()) throw new Error(`${e}: ${collapse(await tid(h, e).innerText()).slice(0, 600)}`);
      }
      if (Date.now() - t0 > 90_000 + maxSteps * POPUP_TIMEOUT_MS) throw new Error(`${label}: the flow did not end`);
      await sleep(400);
    }
  });
  const screenshots = [await shot(h.page, shotDir, `${label}-done`)];
  const subs = h.submits.slice(submitsBefore).filter((x) => x.tx);
  if (subs.length > maxSteps) throw new Error(`${label}: more than ${maxSteps} transactions`);
  const steps = subs.map((x, i) => ({
    txid: x.txid ?? x.tx.id ?? null, kind: null, appStatus: 'submitted', summary: '', shownAs, acks: [], popups: i === 0 ? popups : [], screenshots, signMs: null, totalMs: Date.now() - t0, submitted: x.tx,
  }));
  return { steps, done };
}

export function writeJson(name, obj) {
  mkdirSync(OUT_DIR, { recursive: true });
  const file = join(OUT_DIR, name);
  writeFileSync(file, JSON.stringify(obj, (_, v) => (typeof v === 'bigint' ? v.toString() : v), 2));
  return file;
}
