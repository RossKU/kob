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
import { pendingSpends } from '../../app/pending-spends';
import { checkBeforeSigning, type Blocked, type SendJob } from './send';

/** The finding that stopped the job, or null when it may be signed. */
const blockedBy = (r: { blocked: Blocked } | { job: SendJob }): Blocked | null => ('blocked' in r ? r.blocked : null);

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
    const r = await checkBeforeSigning(servicesFor(built), wallet(), { built, expected: expectedFromPlan(plan) });
    expect(blockedBy(r)).toBeNull();
    expect('job' in r && r.job.built).toBe(built);
  });

  it('stops a transaction that disagrees with the plan (the decoded transaction is the authority, never the form)', async () => {
    const plan = planned();
    const built = plan.built!;
    const claim = expectedFromPlan(plan);
    // the planner claims one sompi more locked than the transaction locks
    const locked = blockedBy(await checkBeforeSigning(servicesFor(built), wallet(), { built, expected: { ...claim, kasLocked: claim.kasLocked! + 1n } }));
    expect(locked?.code).toBe('expected-kas-locked');
    expect(locked?.text).toBeTruthy();
    // the planned order has another price than the order the transaction creates
    const order = claim.orders![0]! as { kind: string; state: { price: string } };
    const repriced = { ...order, state: { ...order.state, price: (BigInt(order.state.price) + 1n).toString() } };
    const price = blockedBy(await checkBeforeSigning(servicesFor(built), wallet(), { built, expected: { ...claim, orders: [repriced as never] } }));
    expect(price?.code).toBe('expected-orders');
  });

  it('stops without a wallet, after an account switch and on another network', async () => {
    const plan = planned();
    const built = plan.built!;
    const job = { built, expected: expectedFromPlan(plan) };
    expect(blockedBy(await checkBeforeSigning(servicesFor(built), wallet({ none: true }), job))?.code).toBe('no-wallet');
    expect(blockedBy(await checkBeforeSigning(servicesFor(built), wallet({ pubkey: `02${'7'.repeat(64)}` }), job))?.code).toBe('account-changed');
    expect(blockedBy(await checkBeforeSigning(servicesFor(built), wallet({ mismatch: true, network: 'mainnet' }), job))?.code).toBe('network');
  });

  it("stops on the caller's re-check, and when the node cannot confirm the coins", async () => {
    const plan = planned();
    const built = plan.built!;
    const job = { built, expected: expectedFromPlan(plan) };
    const stop = { code: 'terms', text: 'The market moved past your price.' };
    expect(blockedBy(await checkBeforeSigning(servicesFor(built), wallet(), { ...job, gate: Promise.resolve({ stop }) }))).toEqual(stop);
    expect(blockedBy(await checkBeforeSigning(servicesFor(built), wallet(), { ...job, gate: Promise.resolve(null) }))).toBeNull();
    const down = { getUtxosByAddresses: () => Promise.reject(new Error('node down')) };
    expect(blockedBy(await checkBeforeSigning(servicesFor(built, down), wallet(), job))?.code).toBe('inputs-unavailable');
    // a node that does not hold the inputs: the decoder flags them (the KAS of covenant inputs is committed by no signature)
    expect(blockedBy(await checkBeforeSigning(servicesFor(null), wallet(), job))?.code).toBe('input-unconfirmed');
  });

  it("puts the re-check's plan in place and checks it as strictly: its own inputs read on the node, decoded against its own claims", async () => {
    const first = planned();
    // the state at the click planned the same order into another transaction (here: another price, so another order state and escrow)
    const other = planOrder(ticketEnv(), buildIntent(form('limit', 'sell', { amount: '5', price: '2.7' }), CTX).intent!);
    const node = new MockNode({ network: NET, addressOf: (spk) => spkStringToAddress(sdk, spk, NET) });
    node.seedFromInputs(first.built!.tx);
    node.seedFromInputs(other.built!.tx);
    const services = servicesFor(null, node);
    const replace = { built: other.built!, expected: expectedFromPlan(other) };
    const r = await checkBeforeSigning(services, wallet(), { built: first.built!, expected: expectedFromPlan(first), gate: Promise.resolve({ replace }) });
    expect(blockedBy(r)).toBeNull();
    expect('job' in r && r.job.built).toBe(other.built);
    // the replacement's own claims are held to the transaction exactly as the first plan's would be
    const wrong = { ...replace, expected: { ...replace.expected, kasLocked: replace.expected.kasLocked! + 1n } };
    const bad = await checkBeforeSigning(services, wallet(), { built: first.built!, expected: expectedFromPlan(first), gate: Promise.resolve({ replace: wrong }) });
    expect(blockedBy(bad)?.code).toBe('expected-kas-locked');
  });

  it('lets the next order spend the change of a transaction this page submitted, which the node lists only once a block holds it', async () => {
    const plan = planOrder(ticketEnv(), buildIntent(form('limit', 'buy', { amount: '2', price: '2.3' }), CTX).intent!);
    const built = plan.built!;
    const job = { built, expected: expectedFromPlan(plan) };
    // the node does not list the coins yet
    expect(blockedBy(await checkBeforeSigning(servicesFor(null), wallet(), job))?.code).toBe('input-unconfirmed');
    // they are the change of transactions this page submitted: their facts are the ones it built
    for (const i of built.tx.inputs) {
      const outputs = Array.from({ length: i.index + 1 }, () => ({ value: '0', scriptPublicKey: '00', covenant: null }));
      outputs[i.index] = { value: i.utxo.amount, scriptPublicKey: i.utxo.scriptPublicKey, covenant: null };
      pendingSpends.addChange({ outputs }, i.transactionId, MAKER_PK);
    }
    expect(blockedBy(await checkBeforeSigning(servicesFor(null), wallet(), job))).toBeNull();
  });
});
