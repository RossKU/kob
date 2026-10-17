// The whole stack over the REAL kob-wasm: the paywall builds its offers through the Rust builders, the client builds,
// signs and preflights through the Rust SDK, and the stub facilitator verifies every payment with the Rust verifier
// (the same code the real facilitator runs) before it answers. Fixtures (keys, UTXOs, tokens, KOB orders) come from the
// Rust-generated golden file. No network.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createNodeServer, createPaywall } from '../src/server.ts';
import { KobX402Client, kcc20Custody } from '../src/client.ts';
import { MemoryArtifactStore } from '../src/artifact-store.ts';
import { decodePaymentResponse } from '../src/headers.ts';
import { KobX402Error } from '../src/errors.ts';
import type { FacilitatorRequest, OfferSpec, PaymentPayload } from '../src/types.ts';
import { buildOffer } from '../src/offers.ts';
import { InvoiceClient, invoiceId, newInvoice } from '../src/invoice.ts';
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
          // the finality the settlement reached, as the facilitator reports it, and a kcc20 offer's custody class (x402-kcc20-profile.md 4.3)
          extensions: {
            kaspa: {
              finality: (body.paymentRequirements.extra as { finality?: string } | undefined)?.finality ?? 'accepted',
              ...(kcc20Custody(body.paymentRequirements) ? { custody: kcc20Custody(body.paymentRequirements) } : {}),
            },
          },
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

test('invoices: native and kcc20 entries paid by KobX402Client.payInvoice through the Rust SDK, verified with the invoice id', opts, async () => {
  const wasm = realWasm()!;
  const cases = loadGolden();
  const by = (n: string) => cases.find((c) => c.name === n)!;
  const kas = by('pay.native').request;
  const payTo: string = by('offer.native').request.payTo;
  const k = by('offer.kcc20').request;
  const ctx = { network: 'kaspa:testnet-10' as const, payTo, maxTimeoutSeconds: 600, finality: 'accepted' as const };
  const native = buildOffer({ kind: 'native', amount: '50000000' }, { wasm, ...ctx });
  const token = buildOffer(
    { kind: 'kcc20', asset: k.asset, amount: k.amount, token: { custody: 'unconditional', carrier: k.carrier, templateHash: k.templateHash, extensionCommitment: k.extensionCommitment, ticker: k.ticker, decimals: k.decimals } },
    { wasm, ...ctx },
  );
  const invoice = newInvoice({ network: 'kaspa:testnet-10', reference: 'wasm-order-1', expiresAtMs: NOW_MS + 600_000, accepts: [token, native] });
  const id = invoiceId(invoice);
  const inv = { invoice, id, payUrl: `https://pay.example/invoices/${id}/pay`, statusUrl: `https://pay.example/invoices/${id}/status`, kaspaUri: null, expiresAtMs: NOW_MS + 600_000 };
  const verified: { ok: boolean; diagnostic?: string; transactionId?: string }[] = [];
  // the facilitator's invoice endpoint: the entry the payment accepts, verified with the invoice id as the request hash
  const fetch = async (_url: string, init?: RequestInit): Promise<Response> => {
    const p = JSON.parse(String(init?.body)) as PaymentPayload;
    const r = wasm.preflight({ requirements: p.accepted, paymentPayload: p, requestHash: id, nowMs: NOW_MS + 500 });
    verified.push(r);
    const body = r.ok
      ? { success: true, transaction: r.transactionId, network: p.accepted.network, amount: p.accepted.amount, extensions: { kaspa: { finality: 'accepted', ...(kcc20Custody(p.accepted) ? { custody: kcc20Custody(p.accepted) } : {}) } } }
      : failure(r.diagnostic ?? 'internal', r.retryable === true, r.message ?? '');
    return new Response(JSON.stringify(body), { status: 200, headers: { 'content-type': 'application/json' } });
  };
  const invoices = new InvoiceClient({ url: 'https://pay.example', fetch });
  const client = new KobX402Client({
    wasm,
    network: 'kaspa:testnet-10',
    payerAddress: 'kaspatest:qpayer',
    privateKeys: [kas.secretKey],
    context: { load: async () => ({ utxos: kas.utxos, tokenUtxos: by('pay.kcc20').request.tokenUtxos, virtualDaaScore: '1000000' }) },
    store: new MemoryArtifactStore(),
    now: () => NOW_MS,
    capabilities: { tokens: { [k.asset]: '3000' }, maxAmount: { KAS: '1000000000', [k.asset]: '3000' } },
    policy: { fundingSource: 'local-keys', allowedOrigins: ['https://pay.example'], allowedPayTo: [...new Set([token.payTo, native.payTo])], allowedKinds: ['native', 'kcc20'] },
  });
  const paid = await client.payInvoice(inv, invoices);
  assert.deepEqual([paid.kind, paid.acceptIndex, paid.invoiceId], ['native', 1, id]);
  assert.deepEqual(verified.map((v) => [v.ok, v.transactionId]), [[true, paid.transactionId]], 'the Rust verifier accepted it under the invoice id');
  const tok = await client.payInvoice(inv, invoices, { acceptIndex: 0 });
  assert.equal(tok.kind, 'kcc20');
  assert.equal(verified[1]!.ok, true, JSON.stringify(verified[1]));
  // the same payment presented for another invoice does not verify
  const other = wasm.preflight({ requirements: native, paymentPayload: (await client.store.load(paid.paymentId))!.paymentPayload, requestHash: 'ab'.repeat(32), nowMs: NOW_MS + 500 });
  assert.equal(other.ok, false);
});
