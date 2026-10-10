// The payer's retry (docs/spec/x402-retry.md) end to end: paywall + client over real HTTP against the stub facilitator and
// the stub wasm, with injected failures. No network beyond loopback, no chain.
//
// What must hold in every case: an unknown outcome re-sends the SAME artifact (never a second transaction); a rebuild happens
// only after a failure that left the payer's funds where they were, re-quotes, re-checks the limits and spends the payment's
// anchor; the merchant serves one payment once; the attempts are bounded.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { KobX402Error } from '../src/errors.ts';
import type { ArtifactRecord } from '../src/artifact-store.ts';
import type { ChainContext, ChainContextProvider, KobX402ClientOptions, PaidFetchResult, PaymentApproval } from '../src/client.ts';
import { backoffMs, classifyFailure, classifyStatus, outpointKey, retryDeadline, retryPolicy } from '../src/retry.ts';
import type { FacilitatorRequest, OfferSpec, PayerUtxo } from '../src/types.ts';
import { payInvoiceWithIntent } from '../src/intent.ts';
import type { FetchedInvoice, InvoiceClient } from '../src/invoice.ts';
import { ASSET_KAS, BINDING_EXACT, BINDING_INTENT, ROUTER_ARTIFACT_ID, TX_ENCODING } from '../src/types.ts';
import type { PaymentRequirements, SettlementResponse } from '../src/types.ts';
import { KobX402Client } from '../src/client.ts';
import { NETWORK, PAYER, SPEND_CAPS, SWAP_BOUNDS, TOKEN_A, kasUtxo, startRig } from './helpers/env.ts';
import type { Rig } from './helpers/env.ts';
import { defaultSettlement, failure } from './helpers/stub-facilitator.ts';

/** Retries without waiting (the backoff is computed, the sleep is skipped). */
const FAST: KobX402ClientOptions['retry'] = { sleep: async () => {}, random: () => 0.5 };
const SWAP_OFFER: OfferSpec = { kind: 'swap', receive: 'kas', amount: '50000000', payAssets: [{ asset: TOKEN_A }] };
/** The merchant asset (KAS) without a ceiling: every attempt asks `approve`. */
const SWAP_CAPS = { tokens: { [TOKEN_A]: '1000' }, maxAmount: {} };

const order = (n: number): Record<string, unknown> => ({ leg: { kind: 'bid', order: { transactionId: (0x90 + n).toString(16).repeat(32), index: 0, amount: '1', state: {} }, amount: '3000' } });
const tokenUtxo = { transactionId: '05'.repeat(32), index: 5, amount: '100000000', blockDaaScore: '100', covenantId: TOKEN_A, state: { amount: '100' } };

/** A chain context whose n-th load (1-based) is `ctx(n)`: a fresh quote every time the payment is (re)built. */
function contexts(ctx: (n: number) => Partial<ChainContext>): ChainContextProvider & { loads: number } {
  const p = {
    loads: 0,
    async load(): Promise<ChainContext> {
      p.loads++;
      return { utxos: [kasUtxo(0), kasUtxo(1)], tokenUtxos: [tokenUtxo], quote: { lockTime: '1000', orders: [order(1)] }, virtualDaaScore: '1000', ...ctx(p.loads) };
    },
  };
  return p;
}

const statuses = async (rig: Rig): Promise<string[]> => (await rig.store.list()).map((r) => r.status);
const txOf = (call: { body: unknown }): string => (JSON.parse((call.body as { paymentPayload: { payload: { transaction: string } } }).paymentPayload.payload.transaction) as { id: string }).id;
const consumedKeys = (r: ArtifactRecord): string[] => r.consumed.map(outpointKey);

test('the retry rules: classification, backoff with equal jitter, the deadline and the options', () => {
  assert.equal(classifyFailure('order_conflict', true), 'rebuild');
  assert.equal(classifyFailure('invalid_kaspa_exact_transaction', false), 'rebuild');
  assert.equal(classifyFailure('settlement_pending', true), 'resend');
  assert.equal(classifyFailure('internal', true), 'resend');
  assert.equal(classifyFailure('internal', false), 'stop');
  assert.equal(classifyFailure('kaspa_payment_identifier_conflict', false), 'stop');
  assert.equal(classifyFailure('underpayment', false), 'stop');
  assert.equal(classifyFailure(undefined, true), 'stop');
  assert.equal(classifyStatus(0, undefined, false), 'resend');
  assert.equal(classifyStatus(503, 'node_unavailable', true), 'resend');
  assert.equal(classifyStatus(502, undefined, false), 'resend');
  assert.equal(classifyStatus(502, 'unauthorized', false), 'stop');
  assert.equal(classifyStatus(409, 'settlement_pending', true), 'resend');
  assert.equal(classifyStatus(409, 'kaspa_payment_identifier_conflict', false), 'stop');
  assert.equal(classifyStatus(413, undefined, false), 'stop');
  const p = retryPolicy(undefined);
  assert.deepEqual([p.attempts, p.resends, p.baseDelayMs, p.maxDelayMs, p.budgetMs], [3, 4, 500, 8000, 120000]);
  assert.deepEqual([backoffMs(p, 1, 0), backoffMs(p, 1, 0.999999), backoffMs(p, 2, 0.5), backoffMs(p, 10, 1), backoffMs(p, 40, 0)], [250, 499, 750, 8000, 4000]);
  assert.equal(retryDeadline(p, 1000, 60), 61000);
  assert.equal(retryDeadline(p, 1000, 600), 121000);
  assert.deepEqual([retryPolicy(false).attempts, retryPolicy(false).resends], [1, 0]);
  assert.throws(() => retryPolicy({ attempts: 0 }), RangeError);
});

test('a lost order race on the first attempt: rebuilt from a fresh quote, anchored, paid and served once', async () => {
  const context = contexts((n) => ({ quote: { lockTime: '1000', orders: [order(n)] } }));
  const approvals: PaymentApproval[] = [];
  const rig = await startRig({
    offers: [SWAP_OFFER],
    capabilities: SWAP_CAPS,
    settle: (n) => (n === 1 ? { body: failure('order_conflict', true, 'an order was filled by another transaction', { orders: [{ txid: '91'.repeat(32), index: 0 }] }) } : undefined),
    client: { context, retry: FAST, approve: (a) => (approvals.push(a), true) },
  });
  try {
    const { response, payment } = await rig.client.paidFetch(`${rig.base}/report`);
    assert.equal(response.status, 200);
    assert.equal(payment?.attempts, 2);
    assert.equal(payment?.resends, 0);
    const calls = rig.facilitator.settleCalls();
    assert.equal(calls.length, 2);
    assert.notEqual(txOf(calls[0]!), txOf(calls[1]!), 'a new transaction');
    assert.equal(context.loads, 2, 'a fresh chain context and quote for the rebuild');
    const [first, second] = await rig.store.list();
    assert.deepEqual([first!.status, second!.status], ['superseded', 'settled']);
    assert.deepEqual(payment?.superseded, [first!.paymentId]);
    assert.notEqual(first!.paymentId, second!.paymentId, 'a fresh payment id');
    assert.equal(second!.replaces, first!.paymentId);
    assert.equal(second!.attempt, 2);
    // the anchor is the payer's token input (never the order), and both attempts spend it
    assert.equal(first!.anchor, outpointKey({ txid: '05'.repeat(32), index: 5 }));
    assert.equal(second!.anchor, first!.anchor);
    assert.ok(consumedKeys(first!).includes(first!.anchor!) && consumedKeys(second!).includes(first!.anchor!));
    assert.ok(consumedKeys(second!).includes(outpointKey({ txid: '92'.repeat(32), index: 0 })), 'the fresh quote');
    // approve was asked for each attempt, at its own cost
    assert.deepEqual(approvals.map((a) => [a.attempt, a.replaces]), [[1, undefined], [2, first!.paymentId]]);
    assert.deepEqual(rig.handled.map((h) => [h.paymentId, h.replayed]), [[second!.paymentId, false]], 'served once');
  } finally {
    await rig.close();
  }
});

test('a facilitator 5xx, a dropped connection and a client timeout: the SAME artifact is re-sent, one transaction, served once', async () => {
  for (const fault of ['5xx', 'drop', 'timeout'] as const) {
    const rig = await startRig({
      settle: (n) =>
        n === 1
          ? fault === '5xx'
            ? { status: 500, body: { error: 'boom' } }
            : fault === 'drop'
              ? { drop: true }
              : { delayMs: 400 }
          : undefined,
      // the timed-out request is still being settled: the re-sends back off (real waits) until the paywall has its outcome
      client: fault === 'timeout' ? { retry: { baseDelayMs: 200, random: () => 0.5 }, requestTimeoutMs: 150 } : { retry: FAST },
    });
    try {
      const { response, payment } = await rig.client.paidFetch(`${rig.base}/report`);
      assert.equal(response.status, 200, fault);
      assert.equal(payment?.attempts, 1, fault);
      assert.ok((payment?.resends ?? 0) >= 1, fault);
      assert.equal(rig.wasm.calls.filter((c) => c.method === 'payNative').length, 1, `${fault}: signed once`);
      const txs = new Set(rig.facilitator.settleCalls().map(txOf));
      assert.equal(txs.size, 1, `${fault}: one transaction`);
      assert.deepEqual(await statuses(rig), ['settled']);
      if (fault === 'timeout') {
        // the first request was settled and served after the client gave up on it; the re-send is its replay
        await new Promise((r) => setTimeout(r, 450));
        assert.deepEqual(rig.handled.map((h) => h.replayed).sort(), [false, true], fault);
      } else {
        assert.deepEqual(rig.handled.map((h) => h.replayed), [false], fault);
      }
    } finally {
      await rig.close();
    }
  }
});

test('a rejected broadcast is rebuilt once and paid once', async () => {
  const rig = await startRig({
    settle: (n) => (n === 1 ? { body: failure('invalid_kaspa_exact_transaction', false, 'the node rejected the transaction: mass too high', undefined, 'invalid_transaction_state') } : undefined),
    client: { retry: FAST },
  });
  try {
    const { response, payment } = await rig.client.paidFetch(`${rig.base}/report`);
    assert.equal(response.status, 200);
    assert.equal(payment?.attempts, 2);
    assert.deepEqual(await statuses(rig), ['superseded', 'settled']);
    const [a, b] = await rig.store.list();
    assert.ok(consumedKeys(b!).includes(a!.anchor!), 'the rebuilt attempt spends the anchor of the first');
    assert.equal(rig.handled.length, 1);
  } finally {
    await rig.close();
  }
});

test('the payment went through but its answer was lost: the re-send finds it, nothing is paid twice', async () => {
  let drops = 1;
  const lossy = async (u: string, init?: RequestInit): Promise<Response> => {
    const r = await fetch(u, init);
    if (drops > 0 && new Headers(init?.headers).has('payment-signature')) {
      drops--;
      throw new TypeError('connection reset after the merchant processed the payment');
    }
    return r;
  };
  const rig = await startRig({ client: { fetch: lossy, retry: FAST } });
  try {
    const { response, payment } = await rig.client.paidFetch(`${rig.base}/report`);
    assert.equal(response.status, 200);
    assert.equal(payment?.attempts, 1);
    assert.equal(payment?.resends, 1);
    assert.equal(rig.facilitator.settleCalls().length, 1, 'settled once');
    assert.equal(rig.wasm.calls.filter((c) => c.method === 'payNative').length, 1, 'signed once');
    assert.deepEqual(rig.handled.map((h) => h.replayed), [false, true], 'the re-send is served as a replay');
    assert.deepEqual(await statuses(rig), ['settled']);
  } finally {
    await rig.close();
  }
});

test('every attempt fails: gives up after 3 with the attempts on the error; all of them share the anchor, none is settled', async () => {
  const rig = await startRig({ settle: () => ({ body: failure('order_conflict', true, 'lost again') }), client: { retry: FAST } });
  try {
    let err: unknown;
    await rig.client.paidFetch(`${rig.base}/report`).catch((e) => (err = e));
    assert.ok(err instanceof KobX402Error);
    assert.equal(err.code, 'payment_failed');
    assert.equal(err.diagnostic, 'order_conflict');
    assert.match(err.message, /gave up after 3 attempt\(s\)/);
    assert.deepEqual(err.attempts?.map((a) => a.outcome), ['order_conflict', 'order_conflict', 'order_conflict']);
    assert.equal(rig.facilitator.settleCalls().length, 3);
    const recs = await rig.store.list();
    assert.deepEqual(recs.map((r) => r.status), ['rejected', 'rejected', 'rejected']);
    const anchor = recs[0]!.anchor!;
    assert.ok(recs.every((r) => r.anchor === anchor && consumedKeys(r).includes(anchor)), 'at most one of them can ever be accepted');
    assert.equal(rig.handled.length, 0);
    // a new fetch for the resource does not sign a fourth payment silently: the attempts are live (rejected, unexpired)
    await assert.rejects(rig.client.fetch(`${rig.base}/report`), (e: unknown) => e instanceof KobX402Error && e.code === 'payment_in_flight');
  } finally {
    await rig.close();
  }
});

test('refusals are not retried: a non-retryable failure stops at once, retry: false sends once', async () => {
  const rig = await startRig({ settle: () => ({ body: failure('underpayment', false, 'too little', undefined, 'invalid_payload') }), client: { retry: FAST } });
  try {
    await assert.rejects(rig.client.fetch(`${rig.base}/report`), (e: unknown) => e instanceof KobX402Error && e.diagnostic === 'underpayment' && e.attempts === undefined);
    assert.equal(rig.facilitator.settleCalls().length, 1);
  } finally {
    await rig.close();
  }
  const off = await startRig({ settle: () => ({ status: 500, body: { error: 'boom' } }), client: { retry: false } });
  try {
    await assert.rejects(off.client.fetch(`${off.base}/report`), (e: unknown) => e instanceof KobX402Error && e.code === 'payment_pending');
    assert.equal(off.facilitator.settleCalls().length, 1);
  } finally {
    await off.close();
  }
});

test('a worse re-quote is held to the limits again: approve refuses its new cost, nothing more is paid', async () => {
  const approvals: PaymentApproval[] = [];
  const rig = await startRig({
    offers: [SWAP_OFFER],
    capabilities: SWAP_CAPS,
    settle: (n) => (n === 1 ? { body: failure('order_conflict', true, 'lost the race') } : undefined),
    client: {
      context: contexts((n) => ({ quote: { lockTime: '1000', orders: [order(n)] } })),
      retry: FAST,
      approve: (a) => (approvals.push(a), BigInt(a.cost.payerSpent ?? '0') <= 500n),
    },
  });
  // the second quote costs more (the stub reports what the builder would)
  let builds = 0;
  rig.wasm.tamper = (r) => {
    r.payerSpent = ++builds === 1 ? '300' : '900';
  };
  try {
    let err: unknown;
    await rig.client.paidFetch(`${rig.base}/report`).catch((e) => (err = e));
    assert.ok(err instanceof KobX402Error);
    assert.equal(err.code, 'spend_not_authorized');
    assert.deepEqual(approvals.map((a) => [a.attempt, a.cost.payerSpent]), [[1, '300'], [2, '900']]);
    assert.equal(rig.facilitator.settleCalls().length, 1, 'the refused attempt was never sent');
    assert.deepEqual(await statuses(rig), ['rejected'], 'never stored either');
    assert.equal(err.attempts?.length, 1);
  } finally {
    await rig.close();
  }
});

test('a worse re-quote above the KAS ceiling (fee included) is refused without approve', async () => {
  const rig = await startRig({
    capabilities: { maxAmount: { KAS: (50_000_000 + 3_000).toString() } },
    settle: (n) => (n === 1 ? { body: failure('invalid_kaspa_exact_fee', false, 'the fee is below the node minimum') } : undefined),
    client: { retry: FAST, maxPay: {} },
  });
  let builds = 0;
  rig.wasm.tamper = (r) => {
    r.feeSompi = ++builds === 1 ? '2000' : '5000';
  };
  try {
    await assert.rejects(rig.client.fetch(`${rig.base}/report`), (e: unknown) => e instanceof KobX402Error && e.code === 'spend_not_authorized');
    assert.equal(rig.facilitator.settleCalls().length, 1);
  } finally {
    await rig.close();
  }
});

test('the anchor decides: a spent anchor stops the rebuild; a larger coin that arrived is set aside so the anchor is spent', async () => {
  // the anchor coin is gone on the second load (it may have been spent by the first attempt): nothing is rebuilt
  const gone = await startRig({
    settle: (n) => (n === 1 ? { body: failure('invalid_kaspa_exact_utxo', false, 'an input was spent by another transaction') } : undefined),
    client: { retry: FAST, context: contexts((n) => (n === 1 ? {} : { utxos: [kasUtxo(1)] })) },
  });
  try {
    await assert.rejects(gone.client.fetch(`${gone.base}/report`), (e: unknown) => e instanceof KobX402Error && e.code === 'payment_pending' && e.diagnostic === 'retry_anchor_spent');
    assert.equal(gone.facilitator.settleCalls().length, 1);
    assert.equal(gone.wasm.calls.filter((c) => c.method === 'payNative').length, 1);
  } finally {
    await gone.close();
  }
  // a larger coin arrived and the builder would take it first: the rebuild runs again without it, around the anchor
  const big: PayerUtxo = kasUtxo(7, '9000000000');
  const grew = await startRig({
    settle: (n) => (n === 1 ? { body: failure('order_conflict', true, 'lost') } : undefined),
    client: { retry: FAST, context: contexts((n) => (n === 1 ? {} : { utxos: [big, kasUtxo(0), kasUtxo(1)] })) },
  });
  try {
    const { payment } = await grew.client.paidFetch(`${grew.base}/report`);
    assert.equal(payment?.attempts, 2);
    const [a, b] = await grew.store.list();
    assert.ok(consumedKeys(b!).includes(a!.anchor!));
    assert.ok(!consumedKeys(b!).includes(outpointKey(big)));
  } finally {
    await grew.close();
  }
});

test('concurrency: paid requests for one resource run one at a time; a resume racing the retry does not send it again', async () => {
  let drops = 1;
  const lossy = async (u: string, init?: RequestInit): Promise<Response> => {
    const r = await fetch(u, init);
    if (drops > 0 && new Headers(init?.headers).has('payment-signature')) {
      drops--;
      throw new TypeError('connection reset');
    }
    return r;
  };
  // each payment's first attempt loses an order race
  const rig = await startRig({
    offers: [SWAP_OFFER],
    capabilities: SWAP_CAPS,
    settle: (n) => (n === 1 || n === 3 ? { body: failure('order_conflict', true, 'lost the race') } : undefined),
    client: { retry: FAST, approve: () => true },
  });
  try {
    const url = `${rig.base}/report`;
    const [a, b] = await Promise.all([rig.client.paidFetch(url), rig.client.paidFetch(url)]);
    assert.equal(a.payment?.attempts, 2);
    assert.equal(b.payment?.attempts, 2);
    const recs = await rig.store.list();
    // two payments, each settled exactly once; every other attempt superseded
    assert.deepEqual(recs.map((r) => r.status).sort(), ['settled', 'settled', 'superseded', 'superseded']);
    assert.equal(new Set([a.payment?.paymentId, b.payment?.paymentId]).size, 2);
    assert.equal(rig.handled.filter((h) => !h.replayed).length, 2);
  } finally {
    await rig.close();
  }
  // the answer of the paid request is lost; a recovery job resumes the stored artifact meanwhile: it waits for the running
  // retry, then finds the payment settled instead of sending it a second time
  const rig2 = await startRig({ client: { fetch: lossy, retry: FAST, newPaymentId: () => 'concurrent-payment-0001' } });
  try {
    const url = `${rig2.base}/report`;
    const p = rig2.client.paidFetch(url);
    while ((await rig2.store.list()).length === 0) await new Promise((r) => setTimeout(r, 1));
    const resumed = rig2.client.resume('concurrent-payment-0001').then(
      () => 'resent',
      (e: KobX402Error) => e.message,
    );
    const { payment } = await p;
    assert.equal(payment?.resends, 1);
    assert.match(await resumed, /already settled/);
    assert.equal(rig2.facilitator.settleCalls().length, 1);
    assert.deepEqual(rig2.handled.map((h) => h.replayed), [false, true]);
  } finally {
    await rig2.close();
  }
});

/**
 * A chain for the stub facilitator: an outpoint is spent by one transaction only (a second transaction spending it is refused
 * `replay`, as the KOB facilitator's ledger and the node refuse it), and the payer's chain context lists only its unspent
 * coins. `refuse(n)` scripts a failure of the n-th settle (nothing is spent).
 */
function exclusiveChain(refuse: (n: number) => SettlementResponse | undefined) {
  const spentBy = new Map<string, string>();
  const settled: string[] = [];
  const settle = (n: number, req: FacilitatorRequest) => {
    const scripted = refuse(n);
    if (scripted) return { body: scripted };
    const tx = JSON.parse(req.paymentPayload.payload.transaction) as { id: string; inputs: { txid: string; index: number }[] };
    const taken = tx.inputs.map(outpointKey).find((k) => spentBy.has(k) && spentBy.get(k) !== tx.id);
    if (taken) return { body: failure('replay', false, `outpoint ${taken} is already consumed by transaction ${spentBy.get(taken)}`) };
    if (!settled.includes(tx.id)) settled.push(tx.id);
    for (const i of tx.inputs) spentBy.set(outpointKey(i), tx.id);
    return { body: defaultSettlement(req) };
  };
  const view = (): ChainContext => ({ utxos: [kasUtxo(0), kasUtxo(1)].filter((u) => !spentBy.has(outpointKey(u))), tokenUtxos: [], virtualDaaScore: '1000' });
  const context: ChainContextProvider = { load: async () => view() };
  return { settle, context, view, settled };
}

test('two retries of one payment racing (another process resumes the attempt this one rebuilds): paid once', async () => {
  // `staleView`: the payer's node does not show the resumed attempt's spend yet, so the rebuilt attempt is sent and only the
  // anchor it shares with the resumed one keeps it out (the facilitator refuses it); otherwise the rebuild sees the anchor spent
  for (const staleView of [false, true]) {
    const chain = exclusiveChain((n) => (n === 1 ? failure('invalid_kaspa_exact_transaction', false, 'the node refused the transaction (mempool full)') : undefined));
    let ids = 0;
    let resumed: Promise<string> | undefined;
    const firstView = chain.view();
    const rig = await startRig({
      settle: chain.settle,
      client: {
        context: staleView ? { load: async () => firstView } : chain.context,
        newPaymentId: () => `racing-payment-a-${String(++ids).padStart(4, '0')}`,
        // the backoff before the rebuild: meanwhile another process holding the stored first attempt re-sends it, and it goes through
        retry: { random: () => 0.5, sleep: async () => void (await resumed) },
      },
    });
    try {
      // the other process: its own client over the same artifact store, resuming the first attempt once it was refused
      const other: KobX402Client = new KobX402Client({ wasm: rig.wasm, network: NETWORK, payerAddress: PAYER, privateKeys: ['01'.repeat(32)], context: chain.context, store: rig.store, capabilities: { maxAmount: SPEND_CAPS }, maxPay: SWAP_BOUNDS, retry: FAST });
      const resume = async (): Promise<string> => {
        while (!(await rig.store.list()).some((r) => r.status === 'rejected')) await new Promise((r) => setTimeout(r, 1));
        const done: PaidFetchResult = await other.resume('racing-payment-a-0001');
        return done.payment!.transactionId;
      };
      resumed = resume();
      const a = await rig.client.paidFetch(`${rig.base}/report`).then(
        () => 'paid',
        (e: KobX402Error) => e.diagnostic ?? e.code,
      );
      const winner: string = await resumed;
      assert.equal(a, staleView ? 'replay' : 'retry_anchor_spent', `stale view ${staleView}: the rebuilt attempt is refused`);
      const recs = await rig.store.list();
      const anchor = recs.find((r) => r.paymentId === 'racing-payment-a-0001')!.anchor!;
      assert.equal(anchor, outpointKey(kasUtxo(0)));
      assert.ok(recs.every((r) => consumedKeys(r).includes(anchor)), 'every attempt of the payment spends its anchor');
      assert.equal(recs.length, staleView ? 2 : 1, 'the rebuilt attempt is built only while the anchor looks unspent');
      // one transaction of the payment went through: the one the resuming process reported, served once by the merchant
      assert.deepEqual(chain.settled, [winner]);
      assert.equal(recs.filter((r) => r.status === 'settled').map((r) => r.transactionId).join(), winner);
      assert.equal(rig.handled.filter((h) => !h.replayed).length, 1);
    } finally {
      await rig.close();
    }
  }
});

// ----------------------------------------------------------------------------------------- intent payments (invoices)

const NOW = 1_800_000_000_000;
function intentOffer(): PaymentRequirements {
  return {
    scheme: 'exact',
    network: 'kaspa:testnet-10',
    amount: '500000000',
    asset: ASSET_KAS,
    payTo: 'kaspatest:qmerchant',
    maxTimeoutSeconds: 600,
    extra: {
      binding: BINDING_EXACT,
      profile: 'standard-native',
      finality: 'accepted',
      transactionEncoding: TX_ENCODING,
      payToScriptPublicKey: '0000',
      route: { binding: BINDING_INTENT, critical: true, router: ROUTER_ARTIFACT_ID, payAssets: [{ asset: '70'.repeat(32), templateHash: '40'.repeat(32), extensionCommitment: 'ee'.repeat(32) }] },
    },
  } as PaymentRequirements;
}

test('intent: the signed creation is re-sent while the keeper cannot execute it yet or the facilitator does not answer; never re-signed', async () => {
  const fetched = { invoice: { accepts: [intentOffer()] }, id: 'ab'.repeat(32), expiresAtMs: NOW + 600_000 } as unknown as FetchedInvoice;
  let signs = 0;
  const wasm = {
    payIntent(r: { requirements: PaymentRequirements }) {
      signs++;
      return { paymentPayload: { x402Version: 2, accepted: r.requirements }, transactionId: 'cd'.repeat(32), consumed: [], feeSompi: '1', expiresAtMs: NOW + 300_000, payerSpent: '3000', intent: { actor: 'TokenToKas_sell', state: {}, intent: {} } };
    },
  } as never;
  const answers: (() => Promise<SettlementResponse>)[] = [
    async () => failure('intent_not_executable', true, 'the book cannot execute this intent now'),
    async () => {
      throw new KobX402Error('facilitator', 'POST /invoices/x/pay: the facilitator is unreachable');
    },
    async () => failure('settlement_pending', true, 'executing'),
    async () => ({ success: true, transaction: 'ef'.repeat(32) }) as SettlementResponse,
  ];
  const payloads: unknown[] = [];
  const invoices = {
    async pay(_id: string, payload: unknown) {
      payloads.push(payload);
      return answers[payloads.length - 1]!();
    },
  } as unknown as InvoiceClient;
  const r = await payInvoiceWithIntent(wasm, invoices, fetched, { payAsset: '70'.repeat(32), utxos: [], nowMs: NOW, options: { maxSell: '3000' } }, { retry: FAST });
  assert.equal(r.settlement.success, true);
  assert.equal(signs, 1, 'signed once');
  assert.equal(payloads.length, 4);
  assert.ok(payloads.every((p) => p === payloads[0]), 'the same signed creation every time');
  assert.equal(r.sends, 4);

  // a refusal is returned as it is (no re-send), and the bound holds
  let n = 0;
  const refusing = { pay: async () => (n++, failure('invoice_paid', false, 'already paid')) } as unknown as InvoiceClient;
  const r2 = await payInvoiceWithIntent(wasm, refusing, fetched, { payAsset: '70'.repeat(32), utxos: [], nowMs: NOW, options: { maxSell: '3000' } }, { retry: FAST });
  assert.equal(r2.settlement.success, false);
  assert.equal(n, 1);
  let m = 0;
  const pending = { pay: async () => (m++, failure('settlement_pending', true, 'executing')) } as unknown as InvoiceClient;
  const r3 = await payInvoiceWithIntent(wasm, pending, fetched, { payAsset: '70'.repeat(32), utxos: [], nowMs: NOW, options: { maxSell: '3000' } }, { retry: { ...FAST, resends: 2 } });
  assert.equal(r3.settlement.errorReason, 'invalid_transaction_state');
  assert.equal(m, 3, 'sent once and re-sent twice');
});
