// The whole stack over the REAL kob-wasm: the paywall builds its offers through the Rust builders, the client builds,
// signs and preflights through the Rust SDK, and the stub facilitator verifies every payment with the Rust verifier
// (the same code the real facilitator runs) before it answers. Fixtures (keys, UTXOs, tokens, KOB orders) come from the
// Rust-generated golden file. No network.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createNodeServer, createPaywall } from '../src/server.ts';
import { KobX402Client } from '../src/client.ts';
import { MemoryArtifactStore } from '../src/artifact-store.ts';
import { decodePaymentResponse } from '../src/headers.ts';
import { KobX402Error } from '../src/errors.ts';
import type { FacilitatorRequest, OfferSpec } from '../src/types.ts';
import type { AddressInfo } from 'node:net';
import { loadGolden, realWasm, skipUnlessWasm } from './helpers/real-wasm.ts';
import { failure, startStubFacilitator } from './helpers/stub-facilitator.ts';

const NOW_MS = 1_800_000_000_000;
const opts = { skip: skipUnlessWasm };

async function rig(spec: OfferSpec, capabilities: Record<string, unknown>, verifyAtMs = NOW_MS + 500, chain: { tokenUtxos?: unknown[]; quote?: unknown } = {}) {
  const wasm = realWasm()!;
  const cases = loadGolden();
  const by = (n: string) => cases.find((c) => c.name === n)!;
  const kas = by('pay.native').request;
  const payTo: string = by('offer.native').request.payTo;
  const context = {
    async load() {
      return {
        utxos: kas.utxos,
        tokenUtxos: chain.tokenUtxos ?? by('pay.kcc20').request.tokenUtxos,
        quote: chain.quote ?? by('pay.swap.sw1').request.quote,
        virtualDaaScore: '1000000',
      };
    },
  };
  const verified: { ok: boolean; diagnostic?: string; transactionId?: string; payerSpent?: string | null }[] = [];
  const facilitator = await startStubFacilitator({
    settle: (_n, body: FacilitatorRequest) => {
      // the merchant-side verifier opts into issuer-controlled pay assets exactly when the payer does (KRON: mint authority)
      const r = wasm.preflight({ requirements: body.paymentRequirements, paymentPayload: body.paymentPayload, requestHash: body.requestHash, nowMs: verifyAtMs, ...(capabilities.allowIssuerControlled === true ? { allowIssuerControlled: true } : {}) });
      verified.push(r);
      if (!r.ok) return { body: failure(r.diagnostic ?? 'internal', r.retryable === true, r.message ?? '') };
      return {
        body: {
          success: true,
          transaction: r.transactionId,
          network: body.paymentRequirements.network,
          amount: body.paymentRequirements.amount,
          payer: body.paymentPayload.payload.payerAddress,
          // the finality the settlement reached, as the facilitator reports it
          extensions: { kaspa: { finality: (body.paymentRequirements.extra as { finality?: string } | undefined)?.finality ?? 'accepted' } },
        },
      };
    },
  });
  const paywall = createPaywall({
    wasm,
    network: 'kaspa:testnet-10',
    payTo,
    offers: [spec],
    facilitator: { url: facilitator.url },
    handler: () => new Response('the report'),
  });
  const server = createNodeServer(paywall);
  await new Promise<void>((r) => server.listen(0, '127.0.0.1', r));
  const base = `http://127.0.0.1:${(server.address() as AddressInfo).port}`;
  const client = new KobX402Client({
    wasm,
    network: 'kaspa:testnet-10',
    payerAddress: 'kaspatest:qpayer',
    privateKeys: [kas.secretKey],
    context,
    store: new MemoryArtifactStore(),
    now: () => NOW_MS,
    capabilities,
    // the e2e payer authorises every payment through the policy hook (spend ceilings are covered by regress-payer.test.ts);
    // a swap always needs a bound for the asset it pays with
    maxPay: Object.fromEntries(['KAS', ...Object.keys((capabilities.tokens ?? {}) as Record<string, string>)].map((a) => [a, '18446744073709551615'])),
    approve: () => true,
  });
  return {
    client,
    base,
    verified,
    facilitator,
    by,
    close: async () => {
      server.closeAllConnections();
      await new Promise<void>((r) => server.close(() => r()));
      await facilitator.close();
    },
  };
}

test('KAS: paywall offer (Rust builder) -> client payment (Rust SDK) -> Rust verifier -> resource', opts, async () => {
  const r = await rig({ kind: 'native', amount: '50000000' }, {});
  try {
    const { response, payment } = await r.client.paidFetch(`${r.base}/report`);
    assert.equal(await response.text(), 'the report');
    assert.equal(payment?.kind, 'native');
    assert.equal(r.verified.length, 1);
    assert.equal(r.verified[0]!.ok, true);
    assert.equal(r.verified[0]!.transactionId, payment!.transactionId, 'the client and the verifier recompute the same transaction id');
    assert.equal(decodePaymentResponse(response.headers.get('payment-response')!).transaction, payment!.transactionId);
  } finally {
    await r.close();
  }
});

test('KCC-20: token offer, local-key payment, verified by the Rust verifier', opts, async () => {
  const cases = loadGolden();
  const k = cases.find((c) => c.name === 'offer.kcc20')!.request;
  const spec: OfferSpec = {
    kind: 'kcc20',
    asset: k.asset,
    amount: k.amount,
    token: { custody: 'unconditional', carrier: k.carrier, templateHash: k.templateHash, extensionCommitment: k.extensionCommitment, ticker: k.ticker, decimals: k.decimals },
  };
  const r = await rig(spec, { tokens: { [k.asset]: '3000' } });
  try {
    const { response, payment } = await r.client.paidFetch(`${r.base}/report`);
    assert.equal(response.status, 200);
    assert.equal(payment?.kind, 'kcc20');
    assert.equal(r.verified[0]!.ok, true, JSON.stringify(r.verified[0]));
    assert.equal(r.verified[0]!.transactionId, payment!.transactionId);
    // a payer that does not hold enough of the token never gets an offer it can pay
    const poor = new KobX402Client({
      wasm: realWasm()!,
      network: 'kaspa:testnet-10',
      payerAddress: 'x',
      privateKeys: ['01'.repeat(32)],
      context: { load: async () => ({ utxos: [] }) },
      capabilities: { tokens: { [k.asset]: '1999' } },
    });
    await assert.rejects(poor.fetch(`${r.base}/report`), (e: unknown) => e instanceof KobX402Error && e.code === 'no_acceptable_offer');
  } finally {
    await r.close();
  }
});

test('swap-and-pay: token A sold into a KOB bid pays the merchant in KAS, verified by the Rust verifier', opts, async () => {
  const cases = loadGolden();
  const s = cases.find((c) => c.name === 'offer.swap.kas')!.request;
  const spec: OfferSpec = {
    kind: 'swap',
    receive: 'kas',
    amount: s.amount,
    payAssets: s.payAssets.map((t: any) => ({ asset: t.covenantId, templateHash: t.templateHash, extensionCommitment: t.extensionCommitment })),
  };
  const asset: string = s.payAssets[0].covenantId;
  const r = await rig(spec, { tokens: { [asset]: '3000' } });
  try {
    const { response, payment } = await r.client.paidFetch(`${r.base}/report`);
    assert.equal(response.status, 200);
    assert.equal(payment?.kind, 'swap');
    assert.equal(r.verified[0]!.ok, true, JSON.stringify(r.verified[0]));
    assert.equal(r.verified[0]!.transactionId, payment!.transactionId);
    assert.equal(r.verified[0]!.payerSpent, '3000', 'the payer gives up 3 whole tokens (3000 base units) of token A');
  } finally {
    await r.close();
  }
});

test('a verifier rejection surfaces with its diagnostic, and nothing is served', opts, async () => {
  // the facilitator verifies one hour after the authorization expired
  const r = await rig({ kind: 'native', amount: '50000000' }, {}, NOW_MS + 3_600_000);
  try {
    await assert.rejects(
      r.client.fetch(`${r.base}/report`),
      (e: unknown) =>
        e instanceof KobX402Error && e.code === 'payment_failed' && e.diagnostic === 'expired_authorization' && e.retryable === false && e.attempts?.length === 3,
    );
    // an expired authorization is rebuilt (a new authorization), up to the 3 attempts of the default retry; the real builders
    // spend the first attempt's anchor input every time, so at most one of them could ever be accepted
    assert.equal(r.verified.length, 3);
    assert.ok(r.verified.every((v) => !v.ok));
    const recs = await r.client.store.list();
    assert.deepEqual(recs.map((x) => x.status), ['rejected', 'rejected', 'rejected']);
    const anchor = recs[0]!.anchor!;
    assert.ok(anchor && recs.every((x) => x.anchor === anchor && x.consumed.some((c) => `${c.txid.toLowerCase()}:${c.index}` === anchor)));
  } finally {
    await r.close();
  }
});

test('swap-and-pay with a KRON pay asset: a KRON token sold into a KobBidKron bid pays the merchant in KAS, verified by the Rust verifier', opts, async () => {
  const cases = loadGolden();
  const g = cases.find((c) => c.name === 'pay.swap.kron.sw1')!.request;
  const pay = g.offer.extra.route.payAssets[0];
  const spec: OfferSpec = {
    kind: 'swap',
    receive: 'kas',
    amount: g.offer.amount,
    payAssets: [{ asset: pay.asset, templateHash: pay.templateHash, extensionCommitment: pay.extensionCommitment }],
  };
  const r = await rig(spec, { tokens: { [pay.asset]: '3000' }, allowIssuerControlled: true }, NOW_MS + 500, { tokenUtxos: g.tokenUtxos, quote: g.quote });
  try {
    const { response, payment } = await r.client.paidFetch(`${r.base}/report`);
    assert.equal(response.status, 200);
    assert.equal(payment?.kind, 'swap');
    assert.equal(r.verified[0]!.ok, true, JSON.stringify(r.verified[0]));
    assert.equal(r.verified[0]!.transactionId, payment!.transactionId);
    assert.equal(r.verified[0]!.payerSpent, '3000', 'the payer gives up 3 whole tokens (3000 base units) of the KRON token');
  } finally {
    await r.close();
  }
});

test('swap-and-pay across families: a KRON pay asset buys a KCC-20 merchant token from a KCC-20 ask in one transaction', opts, async () => {
  const cases = loadGolden();
  const g = cases.find((c) => c.name === 'pay.swap.kron.sw3')!.request;
  const pay = g.offer.extra.route.payAssets[0];
  const t = g.offer.extra.token;
  const spec: OfferSpec = {
    kind: 'swap',
    receive: 'kcc20',
    amount: g.offer.amount,
    asset: g.offer.asset,
    token: { custody: t.custody, carrier: t.carrier, templateHash: t.templateHash, extensionCommitment: t.extensionCommitment, ticker: t.ticker, decimals: t.decimals },
    payAssets: [{ asset: pay.asset, templateHash: pay.templateHash, extensionCommitment: pay.extensionCommitment }],
  };
  const r = await rig(spec, { tokens: { [pay.asset]: '3000' }, allowIssuerControlled: true }, NOW_MS + 500, { tokenUtxos: g.tokenUtxos, quote: g.quote });
  try {
    const { response, payment } = await r.client.paidFetch(`${r.base}/report`);
    assert.equal(response.status, 200);
    assert.equal(payment?.kind, 'swap');
    assert.equal(r.verified[0]!.ok, true, JSON.stringify(r.verified[0]));
    assert.equal(r.verified[0]!.transactionId, payment!.transactionId);
  } finally {
    await r.close();
  }
});
