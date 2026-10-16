// The silent pre-sign checks of an order action (send.ts): no confirmation screen shows the decoded transaction any more, so a transaction that
// disagrees with what the planner claims, a stale account, a wrong network, a node that cannot confirm the coins or a failed re-check must stop
// it before the wallet is asked, with one sentence.
import { describe, expect, it } from 'vitest';
import type { Services } from '../../app/services';
import type { WalletSession } from '../../app/context';
import { loadKaspaSdkNode } from '../../data/kaspa-sdk.node';
import { spkStringToAddress } from '../../data/kaspa-sdk';
import { planOrder } from '../../kob/plan';
import type { OrderPlan } from '../../kob/plan-types';
import type { BuiltTx } from '../../kob/types';
import { MAKER_PK, kob } from '../../testing/fixtures';
import { MockNode } from '../../testing/mock-node';
import type { WalletAdapter } from '../../wallet/types';
import { buildIntent } from '../ticket/form-state';
import { expectedFromPlan } from '../ticket/plan-expected';
import { CTX, form, ticketEnv } from '../ticket/ticket-fixtures';
import { checkBeforeSigning } from './send';

const NET = 'testnet-10';
const K = kob();
const sdk = loadKaspaSdkNode();

const planned = (): OrderPlan => planOrder(ticketEnv(), buildIntent(form('limit', 'sell', { amount: '5', price: '2.6' }), CTX).intent!);

/** The services the checks read: the node holds exactly the inputs of `built` unless another node is given. */
function servicesFor(built: BuiltTx | null, node?: Pick<MockNode, 'getUtxosByAddresses'>): Services {
  const n = node ?? new MockNode({ network: NET, addressOf: (spk) => spkStringToAddress(sdk, spk, NET) });
  if (built && n instanceof MockNode) n.seedFromInputs(built.tx);
  return { kob: K, sdk, node: n, registry: null, config: { network: NET } } as unknown as Services;
}

const wallet = (o: Partial<{ pubkey: string; network: string; mismatch: boolean; none: boolean }> = {}): Pick<WalletSession, 'info' | 'adapter' | 'networkMismatch'> => ({
  info: o.none ? null : ({ pubkey: o.pubkey ?? MAKER_PK, network: o.network ?? NET, id: 'kasware' } as WalletSession['info']),
  adapter: o.none ? null : ({} as WalletAdapter),
  networkMismatch: !!o.mismatch,
});

describe('checkBeforeSigning', () => {
  it('lets a transaction through that the decoder reads exactly as planned', async () => {
    const plan = planned();
    const built = plan.built!;
    expect(await checkBeforeSigning(servicesFor(built), wallet(), { built, expected: expectedFromPlan(plan) })).toBeNull();
  });

  it('stops a transaction that disagrees with the plan (the decoded transaction is the authority, never the form)', async () => {
    const plan = planned();
    const built = plan.built!;
    const claim = expectedFromPlan(plan);
    // the planner claims one sompi more locked than the transaction locks
    const locked = await checkBeforeSigning(servicesFor(built), wallet(), { built, expected: { ...claim, kasLocked: claim.kasLocked! + 1n } });
    expect(locked?.code).toBe('expected-kas-locked');
    expect(locked?.text).toBeTruthy();
    // the planned order has another price than the order the transaction creates
    const order = claim.orders![0]! as { kind: string; state: { price: string } };
    const repriced = { ...order, state: { ...order.state, price: (BigInt(order.state.price) + 1n).toString() } };
    const price = await checkBeforeSigning(servicesFor(built), wallet(), { built, expected: { ...claim, orders: [repriced as never] } });
    expect(price?.code).toBe('expected-orders');
  });

  it('stops without a wallet, after an account switch and on another network', async () => {
    const plan = planned();
    const built = plan.built!;
    const job = { built, expected: expectedFromPlan(plan) };
    expect((await checkBeforeSigning(servicesFor(built), wallet({ none: true }), job))?.code).toBe('no-wallet');
    expect((await checkBeforeSigning(servicesFor(built), wallet({ pubkey: `02${'7'.repeat(64)}` }), job))?.code).toBe('account-changed');
    expect((await checkBeforeSigning(servicesFor(built), wallet({ mismatch: true, network: 'mainnet' }), job))?.code).toBe('network');
  });

  it("stops on the caller's re-check, and when the node cannot confirm the coins", async () => {
    const plan = planned();
    const built = plan.built!;
    const job = { built, expected: expectedFromPlan(plan) };
    expect(await checkBeforeSigning(servicesFor(built), wallet(), { ...job, gate: Promise.resolve('The book moved.') })).toEqual({ code: 'changed', text: 'The book moved.' });
    expect(await checkBeforeSigning(servicesFor(built), wallet(), { ...job, gate: Promise.resolve(null) })).toBeNull();
    const down = { getUtxosByAddresses: () => Promise.reject(new Error('node down')) };
    expect((await checkBeforeSigning(servicesFor(built, down), wallet(), job))?.code).toBe('inputs-unavailable');
    // a node that does not hold the inputs: the decoder flags them (the KAS of covenant inputs is committed by no signature)
    expect((await checkBeforeSigning(servicesFor(null), wallet(), job))?.code).toBe('input-unconfirmed');
  });
});
