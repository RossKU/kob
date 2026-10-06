// Building blocks of the real-wallet run: the independent node checker, the wallet drivers (KasWare / Kaspire) and the app driver
// (connect, issue, place, cancel through the pre-sign confirmation screen). Nothing runs on import. Secrets (mnemonics, keys) are never printed.
import { mkdirSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import {
  OUT_DIR, collapse, extensionId, kaspaNode, kaswareAddress, kaswareApprover, kaspireApprover, log, onboardKasware, onboardKaspire, shot, sleep,
  switchKaswareToTn10, withPopupApproval,
} from './common.mjs';
import * as T from '../../tools/wallet-gate/lib/txbuild.mjs';
import { TN10_NODE } from './fund.mjs';

export const KASPIRE_PASSWORD = 'kob-real-kaspire-throwaway-2026'; // throwaway vault password of a throwaway profile
export const KAS = 100_000_000n;
export const POPUP_TIMEOUT_MS = 180_000;
export const ACCEPT_TIMEOUT_MS = 90_000;

const P2SH_RE = /^aa20[0-9a-f]{64}87$/;

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

  /** Every output of a submitted tx (JSON from the wRPC frame) with its address and whether it is a P2SH (covenant) output. */
  outputsOf(tx) {
    return (tx.outputs ?? []).map((o, i) => {
      const spk = o.scriptPublicKey;
      const script = typeof spk === 'string' ? spk.slice(4) : spk?.script ?? spk?.scriptPublicKey ?? '';
      let address = null;
      try {
        address = this.addressOfSpk(spk);
      } catch {
        /* non standard */
      }
      return { index: i, value: BigInt(o.value ?? o.amount ?? 0), address, p2sh: P2SH_RE.test(script), covenant: o.covenant ?? null };
    });
  }
}

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
 * A wallet under test: `setup(ctx)` onboards it in the browser context and returns what the run needs.
 * `approver()` is the popup handler (reads the popup text, clicks approve).
 */
export function walletDriver(name, env) {
  if (name === 'kasware') {
    const mnemonic = env.WALLET_MNEMONIC_KASWARE;
    return {
      name,
      label: 'KasWare',
      mnemonicKey: 'WALLET_MNEMONIC_KASWARE',
      hasMnemonic: !!mnemonic,
      expectedAddress: mnemonic ? kaswareAddress(mnemonic) : null,
      approver: () => kaswareApproverWithUnlock({ screenshotDir: 'kasware' }),
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
      label: 'Kaspire',
      mnemonicKey: 'WALLET_MNEMONIC_KASPIRE',
      hasMnemonic: !!mnemonic,
      expectedAddress: null, // derived by the wallet; read after connecting
      approver: () => kaspireApprover({ password: KASPIRE_PASSWORD, screenshotDir: 'kaspire' }),
      async setup(ctx) {
        const id = await extensionId(ctx);
        await onboardKaspire(ctx, id, mnemonic, { password: KASPIRE_PASSWORD });
        return { extId: id };
      },
    };
  }
  throw new Error(`unknown wallet ${name}`);
}

// ------------------------------------------------------------------------------------------------ app driver

/** Opens the app in a new page of `ctx` with the injected TN10 config; records console errors and the node's submitTransaction frames. */
export async function openApp(ctx, baseUrl, { registryUrl, nodeUrl = TN10_NODE } = {}) {
  const page = await ctx.newPage();
  const h = { page, consoleErrors: [], pageErrors: [], badResponses: [], submits: [], baseUrl };
  page.on('console', (m) => {
    if (m.type() === 'error') h.consoleErrors.push(m.text().slice(0, 400));
  });
  page.on('response', (r) => {
    if (r.status() >= 400) h.badResponses.push(`${r.status()} ${r.url().replace(baseUrl, '')}`.slice(0, 200));
  });
  page.on('pageerror', (e) => h.pageErrors.push(String(e.message).slice(0, 400)));
  page.on('websocket', (ws) => {
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
  if (process.env.KOB_DEBUG_TX) {
    // debugging aid: remember every serialized transaction JSON the app produced
    await page.addInitScript(() => {
      const orig = JSON.stringify;
      window.__txs = [];
      JSON.stringify = function (...a) {
        const r = orig.apply(this, a);
        if (typeof r === 'string' && r.includes('covenantId') && r.includes('"outputs"') && r.includes('signatureScript')) window.__txs.push(r);
        return r;
      };
    });
  }
  await page.addInitScript(
    (c) => {
      window.__KOB_CONFIG__ = c;
    },
    { network: 'testnet-10', indexerUrl: '', nodeUrl, registryUrl, features: { kastle: false, test: false } },
  );
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

/** Connects the wallet through the app (approves the wallet's connect popup) and reads what the header shows. */
export async function connectWallet(h, ctx, wallet) {
  const btn = tid(h, `wallet-connect-${wallet.name}`);
  await btn.waitFor({ timeout: 60_000 });
  const { popups } = await withPopupApproval(
    ctx,
    wallet.name,
    async () => {
      await btn.click();
      await tid(h, 'wallet-address').waitFor({ timeout: POPUP_TIMEOUT_MS });
    },
    wallet.approver(),
  );
  // the shortened address is display only: read the full one from the provider (already approved, no popup)
  const address = await h.page.evaluate(async (name) => {
    if (name === 'kasware') return (await window.kasware.getAccounts())[0];
    return (await window.kaspire.request({ method: 'getAccounts' }))[0];
  }, wallet.name);
  const network = (await tid(h, 'wallet-network').innerText()).trim();
  return { address, network, popups, shownAddress: (await tid(h, 'wallet-address').innerText()).trim() };
}

/** The wallet's own addresses' KAS balance on the node, sompi. */
export async function kasBalance(node, address) {
  const us = await node.utxos(address);
  return us.filter((u) => !u.covenantId).reduce((a, u) => a + u.amount, 0n);
}

const CONFIRM_ERR = ['confirm-error', 'confirm-rejected'];

/**
 * The pre-sign screen -> wallet popup -> node acceptance round trip. Precondition: `confirm-screen` is (about to be) open.
 * Returns what a human would have seen and the node's answer. Throws with the app's own error text on any failure.
 */
export async function signOnConfirmScreen(h, ctx, wallet, label, shotDir) {
  const t0 = Date.now();
  const screen = tid(h, 'confirm-screen');
  await screen.waitFor({ timeout: 90_000 });
  await sleep(500);
  const blocking = await tid(h, 'confirm-blocking').count();
  const summary = collapse(await tid(h, 'confirm-summary').innerText().catch(() => ''));
  const fullText = await screen.innerText().catch(() => '');
  const screenshots = [await shot(h.page, shotDir, `${label}-confirm`)];
  if (blocking) throw new Error(`the confirmation screen shows BLOCKING findings: ${collapse(await tid(h, 'confirm-blocking').innerText())}`);
  const ack = tid(h, 'confirm-ack');
  if (await ack.count()) await ack.check();
  const submitsBefore = h.submits.length;
  const tSign = Date.now();
  const { popups } = await withPopupApproval(
    ctx,
    wallet.name,
    async () => {
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
    },
    wallet.approver(),
  );
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
  return { txid, appStatus: status, summary, confirmText: fullText, popups, screenshots, signMs, totalMs: Date.now() - t0, submitted };
}

export function writeJson(name, obj) {
  mkdirSync(OUT_DIR, { recursive: true });
  const file = join(OUT_DIR, name);
  writeFileSync(file, JSON.stringify(obj, (_, v) => (typeof v === 'bigint' ? v.toString() : v), 2));
  return file;
}
