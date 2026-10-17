// The order-type matrix (mock KasWare wallet, default mock seed, EXKCC listed as tradable): every order type of docs/spec/order-types.md, on
// each side where it exists, is placed end to end through the UI:
//   ticket fields -> ONE click on the order button -> the final confirmation (the signer's table: summary, fee and the order's terms, read before it is confirmed) -> its
//   Confirm -> the wallet signs exactly the inputs of the transaction -> mock node accepts (the covenants run) -> the indexer holds the right contract with the right state fields
//   (and kob-wasm `recoverOrders` on the accepted tx agrees) -> My orders shows it with the right label and status.
// The expectations live in cases.ts; this file is the generic driver plus the timing rules (GTC 90 days, GTD date, day order until 00:00 UTC
// against `kob.dayOrder`, timed activation, IOC / auction lifetimes).
import { test, expect } from '../fixtures';
import { p2pkSpk } from '../../mock/address.mjs';
import { loadKobNode } from '../../src/kob/wasm.node';
import ticketText from '../../src/i18n/en/ticket';
import { formatKas } from '../../src/kob/units';
import { browserTzOffset, fund, mockClock, openMarket } from '../helpers/env';
import { clickPlaceAndRead, expectWalletSigned, fillFields, pickType, placedBy, waitReviewable, waitSubmitted } from '../helpers/ticket';
import { CASES, type Ctx, type Timing } from './cases';

test.use({ autoOpen: false });

const GTC_DAA = 77_760_000; // 90 days at 10 DAA/s (docs/spec/order-types.md)
/** the 8-byte little-endian integer push of the state codec: 08 <u64 le> */
const pushInt = (v: bigint): string => '08' + Array.from({ length: 8 }, (_, i) => Number((v >> BigInt(8 * i)) & 0xffn).toString(16).padStart(2, '0')).join('');

interface Placement {
  state: Record<string, string>;
  view: any;
  daa: number;
  unix: number;
}

/** The timing rules: what the planner computed for the on-chain expiry / activation against the node clock and the wallet defaults. */
async function checkTiming(t: Timing, p: Placement, ctx: Ctx): Promise<void> {
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
      break;
    }
    case 'timed': {
      const target = Math.floor((ctx.unix + t.afterSeconds) / 60) * 60;
      expect(Math.abs(activeFrom - (p.daa + (target - p.unix) * 10))).toBeLessThan(slack(t.afterSeconds));
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

    // ---- one click: the final confirmation, read before it is confirmed; then the wallet is asked once and signs exactly the inputs of the transaction
    const receipt = await clickPlaceAndRead(page);
    expect(receipt.from).toBe('app');
    expect(receipt.title).toBe('Place order');
    expect(receipt.updated).toBe(false);
    expect(receipt.problems).toBe(false);
    // the one order the transaction creates, with the terms of the case (and the exit an if-done entry commits to)
    expect(receipt.orders, 'one order in the table').toHaveLength(1);
    const order = receipt.orders[0]!;
    expect(order.title).toBe(c.receipt.orderTitle);
    expect(order.token, 'the token line names the traded token').toMatch(/^EXKCC [(][0-9a-f]{8}…[0-9a-f]{8}[)]$/);
    expect(order.rows).toMatchObject(c.receipt.card);
    if (c.receipt.exitCard) {
      expect(order.exit?.title).toBe(c.receipt.exitTitle);
      expect(order.exit?.rows).toMatchObject(c.receipt.exitCard);
    } else expect(order.exit).toBeNull();
    // the summary: the token lines of the case, what is locked in orders (the value the accepted transaction locks), and the net KAS = locked + fee
    for (const [label, value] of Object.entries(c.receipt.lines ?? {})) expect(receipt.lines[label], `summary line ${label}`).toBe(value);
    expect(receipt.lines['Locked in orders'], 'locked in orders').toBe(c.locked);
    const kasAmount = (text: string | undefined): bigint => {
      const m = /^([−-]?)(\d+)(?:[.](\d+))?\sKAS$/.exec(text ?? '');
      expect(m, `a KAS amount in "${text}"`).toBeTruthy();
      return (m![1] ? -1n : 1n) * (BigInt(m![2]!) * 100_000_000n + BigInt((m![3] ?? '').padEnd(8, '0')));
    };
    expect(kasAmount(receipt.lines.KAS), 'net KAS = -(locked + fee)').toBe(-(kasAmount(c.locked) + kasAmount(receipt.fee)));
    const placed = await placedBy(mock, await waitSubmitted(page));
    expect(receipt.fee, 'the fee shown is the fee of the accepted transaction').toBe(`${formatKas(BigInt(placed.submission.fee))} KAS`);
    // the DAA times the table shows are the ones the accepted order carries
    const stateOf = placed.views[0].state.state as Record<string, string>;
    if (c.timing.kind === 'day') expect(order.rows['Day order ends'], 'the day order says its end').toContain('00:00 UTC');
    else expect(order.rows.Expires, 'expiry').toBe(`DAA score ${stateOf.expiryDaa}`);
    if (Number(stateOf.activeFrom) > 0 && 'Starts at' in order.rows) expect(order.rows['Starts at'], 'start').toBe(`DAA score ${stateOf.activeFrom}`);
    await expectWalletSigned(wallet!, placed, { inputs: Array.from({ length: placed.submission.inputs }, (_, i) => i) });

    // ---- the transaction itself: one order created, the value it locks, nothing paid to another key
    expect(placed.submission.created).toHaveLength(1);
    const tx = placed.submission.tx;
    const alice = (await mock.balance('alice')).pubkey;
    const lockedOutputs = new Set([...placed.submission.created.map((o) => o.output), ...placed.submission.token_outputs.filter((o) => o.role === 'custody').map((o) => o.output)]);
    const locked = [...lockedOutputs].reduce((sum, i) => sum + BigInt(tx.outputs[i].value), 0n);
    expect(locked, 'KAS locked in contracts').toBe(BigInt(Math.round(Number(c.locked.replace(' KAS', '')) * 1e8)));
    // everything else goes back to the wallet key: its KAS change, or the token change carrier it owns
    const ownCarriers = new Set(placed.submission.token_outputs.filter((o) => o.role === 'owned' && o.owner === alice).map((o) => o.output));
    tx.outputs.forEach((o: any, i: number) => {
      if (!lockedOutputs.has(i) && !ownCarriers.has(i)) expect(o.scriptPublicKey, `output ${i} is change to the wallet key`).toBe(p2pkSpk(alice));
    });

    // ---- what the indexer holds
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
    await checkTiming(c.timing, { state, view, daa: placed.submission.daa, unix: at }, ctx);

    // ---- My orders
    await page.getByTestId('nav-orders').click();
    const row = page.getByTestId(`order-row-${view.covenant_id}`);
    await expect(row).toBeVisible();
    await expect(row).toHaveAttribute('data-type', c.listType);
    await expect(row).toHaveAttribute('data-side', c.side);
    await expect(row).toHaveAttribute('data-status', 'open');
    await expect(page.getByTestId(`order-type-${view.covenant_id}`)).toHaveText(c.listLabel);
    await expect(row.getByTestId('order-status')).toHaveText(/^open$/i);
    await expect(row.getByTestId(`order-tx-${view.covenant_id}`)).toBeVisible();
    // the placement note the app stored with the record, in the ticket's words ("Sell · Limit"), never the form's ids
    const typeName = (ticketText as Record<string, string>)[`ticket.type.${c.type}.name`]!.replace('{name}', 'EXKCC');
    await expect(row).toContainText(`${c.side === 'buy' ? 'Buy' : 'Sell'} · ${typeName}`);
  });
}

