// The order-type matrix (mock KasWare wallet, default mock seed, EXKCC listed as tradable): every order type of docs/spec/order-types.md, on
// each side where it exists, is placed end to end through the UI:
//   ticket fields -> live disclosure -> review -> decoded confirmation screen (no blocking finding) -> acknowledge -> wallet signs exactly the
//   inputs of the transaction -> mock node accepts (the covenants run) -> the indexer holds the right contract with the right state fields
//   (and kob-wasm `recoverOrders` on the accepted tx agrees) -> My orders shows it with the right label and status.
// The expectations live in cases.ts; this file is the generic driver plus the timing rules (GTC 90 days, GTD date, day order until 00:00 UTC
// against `kob.dayOrder`, timed activation, IOC / auction lifetimes).
import { test, expect } from '../fixtures';
import { loadKobNode } from '../../src/kob/wasm.node';
import { browserTzOffset, fund, mockClock, openMarket } from '../helpers/env';
import { fillFields, openReview, pickType, readDisclosure, acknowledgeAndSign, placedBy, expectWalletSigned, waitReviewable } from '../helpers/ticket';
import { CASES, type Ctx, type Timing } from './cases';

test.use({ autoOpen: false });

const GTC_DAA = 77_760_000; // 90 days at 10 DAA/s (docs/spec/order-types.md)
/** the 8-byte little-endian integer push of the state codec: 08 <u64 le> */
const pushInt = (v: bigint): string => '08' + Array.from({ length: 8 }, (_, i) => Number((v >> BigInt(8 * i)) & 0xffn).toString(16).padStart(2, '0')).join('');
const utcText = (unix: number): string => `${new Date(unix * 1000).toISOString().slice(0, 16).replace('T', ' ')} UTC`;

interface Placement {
  state: Record<string, string>;
  view: any;
  daa: number;
  unix: number;
}

/** The timing rules: what the planner computed for the on-chain expiry / activation against the node clock and the wallet defaults. */
async function checkTiming(t: Timing, p: Placement, ctx: Ctx, extra: { discRows: Record<string, string>; confirmRows: Record<string, string> }): Promise<void> {
  const expiry = Number(p.state.expiryDaa);
  const activeFrom = Number(p.state.activeFrom);
  const slack = (secs: number) => Math.round(secs * 10 * 0.01) + 600; // 1% (measured DAA rate) + planning lag
  switch (t.kind) {
    case 'gtc':
      expect(Math.abs(expiry - p.daa - GTC_DAA), 'GTC = 90 days of DAA').toBeLessThan(5_000);
      break;
    case 'gtd': {
      const target = Math.floor((ctx.unix + t.afterSeconds) / 60) * 60;
      expect(Math.abs(expiry - (p.daa + (target - p.unix) * 10))).toBeLessThan(slack(t.afterSeconds));
      expect(extra.discRows.expiry).toContain(utcText(target));
      break;
    }
    case 'timed': {
      const target = Math.floor((ctx.unix + t.afterSeconds) / 60) * 60;
      expect(Math.abs(activeFrom - (p.daa + (target - p.unix) * 10))).toBeLessThan(slack(t.afterSeconds));
      expect(extra.discRows.activates).toContain(utcText(target));
      expect(extra.confirmRows.activeFrom).toContain(utcText(target));
      expect(expiry).toBeGreaterThan(activeFrom);
      break;
    }
    case 'day': {
      // the placement record carries the wall-clock deadline: the next 00:00 UTC after the placement (either side of a midnight that passed while the test ran)
      const day = (u: number) => (Math.floor(u / 86_400) + 1) * 86_400;
      expect([day(p.unix - 5), day(p.unix + 5)]).toContain(p.view.deadline);
      const kob = await loadKobNode();
      // the wallet measures the DAA rate; dayOrder clamps it to 9.5 .. 10.5 DAA/s and adds a 1% margin
      const lo = Number(kob.dayOrder(BigInt(p.daa), BigInt(p.unix), 9_500n).expiryDaa);
      const hi = Number(kob.dayOrder(BigInt(p.daa), BigInt(p.unix), 10_500n).expiryDaa);
      expect(kob.dayOrder(BigInt(p.daa), BigInt(p.unix), 10_000n).deadline.toString()).toBe(String(p.view.deadline));
      expect(expiry).toBeGreaterThanOrEqual(lo - 300);
      expect(expiry).toBeLessThanOrEqual(hi + 300);
      expect(extra.discRows.expiry).toContain('00:00 UTC');
      expect(extra.confirmRows.deadline).toContain(utcText(p.view.deadline).replace(/ UTC$/, ''));
      break;
    }
    case 'ioc':
      // IOC / FOK: dead 30 s after placement (300 DAA) at the latest
      expect(expiry - p.daa).toBeGreaterThan(100);
      expect(expiry - p.daa).toBeLessThan(500);
      break;
    case 'auction':
      // market family: a short auction (20 s) plus the IOC life (30 s), never a resting order
      expect(BigInt(p.state.slope)).toBeGreaterThan(0n);
      expect(p.state.decayStep).toBe('1');
      expect(expiry - p.daa).toBeGreaterThan(100);
      expect(expiry - p.daa).toBeLessThan(900);
      break;
    case 'none':
      break;
  }
}

for (const c of CASES) {
  test(`order matrix: ${c.name}`, async ({ appPage: page, mock, wallet }) => {
    await fund(mock);
    await openMarket(page, mock);
    const clock = await mockClock(mock);
    const ctx: Ctx = { tz: await browserTzOffset(page), unix: clock.unix };
    const fields = c.fields(ctx);

    // ---- ticket
    await pickType(page, c.type, c.side);
    await fillFields(page, fields);
    await waitReviewable(page);
    if (!('tip' in fields)) await expect(page.getByTestId('order-tip')).toHaveValue(''); // the default tip is 0: an empty box
    const disc = await readDisclosure(page);
    for (const [id, want] of Object.entries(c.disc)) {
      expect(disc.rows[id], `disclosure row ${id}`).toBeDefined();
      expect(disc.rows[id], `disclosure row ${id}`).toContain(want);
    }
    for (const id of c.discAbsent ?? []) expect(disc.rows[id], `disclosure row ${id} must not exist`).toBeUndefined();
    for (const n of c.notes) expect(disc.notes, `disclosure note ${n}`).toContain(n);
    for (const [k, want] of Object.entries(c.carriers)) expect(disc.carriers[k], `carrier ${k}`).toContain(want);
    expect(disc.kasLocked).toBe(c.locked);
    if (c.escrowed === null) expect(disc.tokensEscrowed).toBeNull();
    else expect(disc.tokensEscrowed).toBe(c.escrowed);
    expect(disc.summary).toContain(`${c.side === 'sell' ? 'Sell' : 'Buy'} ${fields.amount} EXKCC`);

    // ---- confirmation screen: decoded from the transaction itself
    const confirm = await openReview(page);
    expect(confirm.blocking).toEqual([]);
    expect(confirm.kind).toBe('create');
    expect(confirm.created).toHaveLength(1);
    const card = confirm.created[0]!;
    expect(card.title).toBe(c.title);
    expect(card.badge).toMatch(/verified/);
    for (const [id, want] of Object.entries(c.rows)) {
      expect(card.rows[id], `confirmation row ${id}`).toBeDefined();
      expect(card.rows[id], `confirmation row ${id}`).toContain(want);
    }
    if (c.exit) {
      expect(card.children).toHaveLength(1);
      expect(card.children[0]!.title).toBe(c.exit.title);
      for (const [id, want] of Object.entries(c.exit.rows)) {
        expect(card.children[0]!.rows[id], `exit row ${id}`).toBeDefined();
        expect(card.children[0]!.rows[id], `exit row ${id}`).toContain(want);
      }
    } else expect(card.children).toEqual([]);
    expect(confirm.sections.locked?.rows['locked-total'], 'locked in contracts').toContain(c.locked);
    if (c.escrowed) {
      const netTok = Object.entries(confirm.sections.net!.rows).find(([id]) => id.startsWith('tok-net-'));
      expect(netTok?.[1], 'net token effect').toContain(`-${c.escrowed}`);
    }
    expect(confirm.sections.net!.rows['net-kas']).toMatch(/^-\d/); // KAS leaves the wallet (locked + fee)
    expect(confirm.sections.others, 'nothing goes to other keys').toBeUndefined();

    // ---- sign
    const txid = await acknowledgeAndSign(page);
    const placed = await placedBy(mock, txid);
    await expectWalletSigned(wallet!, placed, { inputs: Array.from({ length: placed.submission.inputs }, (_, i) => i) });

    // ---- what the indexer holds
    expect(placed.submission.created).toHaveLength(1);
    expect(placed.submission.created[0]!.kind).toBe(c.kind);
    const view = placed.views[0];
    expect(view.contract).toBe(c.kind);
    expect(view.status).toBe('open');
    expect(view.maker).toBe((await mock.balance('alice')).pubkey);
    const state: Record<string, string> = view.state.state;
    for (const [k, v] of Object.entries(c.state)) expect(state[k], `state.${k}`).toBe(v);
    if (!('tip' in fields)) expect(state.tip ?? '0').toBe('0');
    if (c.escrowed) {
      // the tokens sit in ONE custody UTXO owned by the order's covenant id
      expect(view.custody.ok).toBe(true);
      expect(view.custody.utxo.state.owner).toBe(view.covenant_id);
    }
    if (c.exit) {
      // the exit an if-done entry commits to sits inside the entry state as a template (the fields only known at the fill are left out):
      // it must belong to the maker, name the token, and carry every price of the exit as an 8-byte push
      const tpl = state.exitState!;
      expect(tpl.startsWith('20' + view.maker), 'exit template owner').toBe(true);
      expect(tpl).toContain(view.state.state.tokenCovId);
      for (const [k, v] of Object.entries(c.exit.state.fields)) if (v !== '0') expect(tpl, `exit state.${k}`).toContain(pushInt(BigInt(v)));
    }
    // kob-wasm reads the same order out of the accepted transaction
    const kob = await loadKobNode();
    const recovered = kob.recoverOrders(placed.submission.tx);
    expect(recovered).toHaveLength(1);
    expect(recovered[0]!.covenantId).toBe(view.covenant_id);
    expect(recovered[0]!.order.kind).toBe(c.kind);
    expect(String(recovered[0]!.value)).toBe(view.current.value);

    const at = Math.floor(Date.parse(placed.submission.at) / 1000);
    await checkTiming(c.timing, { state, view, daa: placed.submission.daa, unix: at }, ctx, { discRows: disc.rows, confirmRows: card.rows });

    // ---- My orders
    await page.getByTestId('confirm-close').click();
    await expect(page.getByTestId('confirm-screen')).toHaveCount(0);
    await page.getByTestId('nav-orders').click();
    const row = page.getByTestId(`order-row-${view.covenant_id}`);
    await expect(row).toBeVisible();
    await expect(row).toHaveAttribute('data-type', c.listType);
    await expect(row).toHaveAttribute('data-side', c.side);
    await expect(row).toHaveAttribute('data-status', 'open');
    await expect(page.getByTestId(`order-type-${view.covenant_id}`)).toHaveText(c.listLabel);
    await expect(row.getByTestId('order-status')).toHaveText(/^open$/i);
    await expect(row.getByTestId(`order-tx-${view.covenant_id}`)).toBeVisible();
    await expect(row).toContainText(`${c.type} ${c.side}`); // the placement note the app stored with the record
  });
}

