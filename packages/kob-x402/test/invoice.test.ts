// Invoices: the merchant registers an invoice and awaits it paid, the payer fetches it from its URL (the id is checked)
// and pays it; against a stub facilitator that speaks the /invoices endpoints. No wasm.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import type { AddressInfo } from 'node:net';
import { InvoiceClient, fetchInvoice, invoiceId, invoiceIdFromUrl, kaspaUri, newInvoice } from '../src/invoice.ts';
import { isIntentOffer, payInvoiceWithIntent } from '../src/intent.ts';
import { KobX402Error } from '../src/errors.ts';
import { ASSET_KAS, BINDING_EXACT, BINDING_INTENT, ROUTER_ARTIFACT_ID, TX_ENCODING } from '../src/types.ts';
import type { Invoice, InvoiceStatus, PaymentPayload, PaymentRequirements } from '../src/types.ts';

const NOW = 1_800_000_000_000;
const KEY = 'merchant-key';

function kasOffer(amount: string): PaymentRequirements {
  return {
    scheme: 'exact',
    network: 'kaspa:testnet-10',
    amount,
    asset: ASSET_KAS,
    payTo: 'kaspatest:qmerchant',
    maxTimeoutSeconds: 600,
    extra: { binding: BINDING_EXACT, profile: 'standard-native', finality: 'accepted', transactionEncoding: TX_ENCODING, payToScriptPublicKey: '0000' },
  };
}

function intentOffer(): PaymentRequirements {
  const o = kasOffer('500000000');
  o.extra.route = { binding: BINDING_INTENT, critical: true, router: ROUTER_ARTIFACT_ID, payAssets: [{ asset: '70'.repeat(32), templateHash: '40'.repeat(32), extensionCommitment: 'ee'.repeat(32) }] };
  return o;
}

async function stubFacilitator() {
  const invoices = new Map<string, Invoice>();
  const statuses = new Map<string, InvoiceStatus>();
  const paid: { id: string; payload: PaymentPayload }[] = [];
  const server = createServer((req, res) => {
    let body = '';
    req.on('data', (c) => (body += c));
    req.on('end', () => {
      const send = (status: number, v: unknown) => {
        res.writeHead(status, { 'content-type': 'application/json' });
        res.end(JSON.stringify(v));
      };
      const url = req.url ?? '';
      if (req.method === 'POST' && url === '/invoices') {
        if (req.headers.authorization !== `Bearer ${KEY}`) return send(401, { error: 'unauthorized' });
        const inv = JSON.parse(body) as Invoice;
        const id = invoiceId(inv);
        const created = !invoices.has(id);
        invoices.set(id, inv);
        statuses.set(id, { id, reference: inv.reference, status: 'unpaid', expiresAt: inv.expiresAt, attempts: [], extraPayments: [] });
        return send(200, { id, url: `/invoices/${id}`, invoice: inv, created });
      }
      const m = /^\/invoices\/([0-9a-f]{64})(\/status|\/pay)?$/.exec(url);
      if (!m) return send(404, { error: 'not_found' });
      const id = m[1]!;
      const inv = invoices.get(id);
      if (!inv) return send(404, { error: 'not_found', extensions: { kaspa: { diagnostic: 'invoice_unknown', retryable: false, message: 'unknown invoice' } } });
      if (!m[2]) return send(200, inv);
      if (m[2] === '/status') return send(200, statuses.get(id));
      const payload = JSON.parse(body) as PaymentPayload;
      paid.push({ id, payload });
      const s = statuses.get(id)!;
      if (s.status === 'paid') {
        return send(200, { success: false, errorReason: 'invalid_transaction_state', transaction: '', extensions: { kaspa: { diagnostic: 'invoice_paid', retryable: false, message: 'paid' } } });
      }
      // paid after a short "execution": pending first, then paid
      s.status = 'pending';
      setTimeout(() => {
        s.status = 'paid';
        s.payment = { transaction: 'ab'.repeat(32), acceptedIndex: 0, acceptedDaaScore: '123' };
      }, 50);
      return send(200, { success: false, errorReason: 'invalid_transaction_state', transaction: '', extensions: { kaspa: { diagnostic: 'settlement_pending', retryable: true, message: 'executing' } } });
    });
  });
  await new Promise<void>((r) => server.listen(0, '127.0.0.1', r));
  const url = `http://127.0.0.1:${(server.address() as AddressInfo).port}`;
  return { url, invoices, paid, close: () => new Promise<void>((r) => server.close(() => r())) };
}

test('an invoice is content-addressed: its id is the SHA-256 of its canonical JSON', () => {
  const a = newInvoice({ network: 'kaspa:testnet-10', reference: 'order-1', expiresAtMs: NOW + 600_000, memo: 'two coffees', accepts: [kasOffer('150000000')] });
  assert.equal(a.expiresAt, '2027-01-15T08:10:00.000Z');
  const id = invoiceId(a);
  assert.match(id, /^[0-9a-f]{64}$/);
  // key order does not matter, the reference does
  const reordered = JSON.parse(JSON.stringify({ accepts: a.accepts, reference: a.reference, x402Version: 2, memo: a.memo, network: a.network, expiresAt: a.expiresAt, invoiceVersion: a.invoiceVersion }));
  assert.equal(invoiceId(reordered), id);
  assert.notEqual(invoiceId({ ...a, reference: 'order-2' }), id);
  assert.equal(kaspaUri(a), 'kaspatest:qmerchant?amount=1.5');
  assert.equal(kaspaUri({ ...a, accepts: [intentOffer()] }), null, 'a route is not a plain KAS payment');
  assert.equal(invoiceIdFromUrl(`https://pay.example/invoices/${id}`), id);
  assert.throws(() => invoiceIdFromUrl('https://pay.example/invoices/zz'));
});

test('merchant registers and awaits the invoice paid; the payer fetches, checks and pays it', async () => {
  const f = await stubFacilitator();
  try {
    const merchant = new InvoiceClient({ url: f.url, apiKey: KEY });
    const inv = newInvoice({ network: 'kaspa:testnet-10', reference: 'order-9', expiresAtMs: Date.now() + 600_000, accepts: [kasOffer('100000000'), intentOffer()] });
    const reg = await merchant.create(inv);
    assert.equal(reg.id, invoiceId(inv));
    assert.equal(reg.created, true);
    assert.equal((await merchant.create(inv)).created, false, 'idempotent');
    // the payer side: no API key
    const payer = new InvoiceClient({ url: f.url });
    await assert.rejects(payer.create(inv), (e: unknown) => e instanceof KobX402Error && e.code === 'bad_request');
    const fetched = await fetchInvoice(merchant.urlOf(reg.id), { nowMs: Date.now() });
    assert.equal(fetched.id, reg.id);
    assert.equal(fetched.kaspaUri, 'kaspatest:qmerchant?amount=1');
    assert.equal(fetched.payUrl, `${merchant.urlOf(reg.id)}/pay`);
    const intentIdx = fetched.invoice.accepts.findIndex(isIntentOffer);
    assert.equal(intentIdx, 1);
    // the payer submits a payment; the facilitator answers settlement_pending while it executes
    const payload = { x402Version: 2, accepted: fetched.invoice.accepts[intentIdx]! } as unknown as PaymentPayload;
    const r = await payer.pay(reg.id, payload);
    assert.equal(r.success, false);
    assert.equal((r.extensions as any).kaspa.diagnostic, 'settlement_pending');
    const s = await merchant.awaitPaid(reg.id, { intervalMs: 10, timeoutMs: 5_000 });
    assert.equal(s.status, 'paid');
    assert.equal(s.payment?.transaction, 'ab'.repeat(32));
    // a second payment of the paid invoice is refused
    const dup = await payer.pay(reg.id, payload);
    assert.equal((dup.extensions as any).kaspa.diagnostic, 'invoice_paid');
    assert.equal(f.paid.length, 2);
    // unknown invoices are a 404 with the diagnostic
    await assert.rejects(payer.status('00'.repeat(32)), (e: unknown) => e instanceof KobX402Error && e.status === 404 && e.diagnostic === 'invoice_unknown');
  } finally {
    await f.close();
  }
});

test('fetchInvoice refuses an invoice that does not hash to its URL, or that expired', async () => {
  const f = await stubFacilitator();
  try {
    const merchant = new InvoiceClient({ url: f.url, apiKey: KEY });
    const inv = newInvoice({ network: 'kaspa:testnet-10', reference: 'order-3', expiresAtMs: Date.now() + 60_000, accepts: [kasOffer('100000000')] });
    const { id } = await merchant.create(inv);
    // the host swaps the content under the same id
    f.invoices.set(id, { ...inv, accepts: [kasOffer('1')] });
    await assert.rejects(fetchInvoice(merchant.urlOf(id), { nowMs: Date.now() }), /does not hash/);
    await assert.rejects(new InvoiceClient({ url: f.url }).get(id), /does not hash/);
    f.invoices.set(id, inv);
    await assert.rejects(fetchInvoice(merchant.urlOf(id), { nowMs: Date.now() + 120_000 }), (e: unknown) => e instanceof KobX402Error && e.diagnostic === 'invoice_expired');
    // plain http is only for loopback hosts
    await assert.rejects(fetchInvoice(`http://pay.example/invoices/${id}`, { nowMs: Date.now() }), /https/);
  } finally {
    await f.close();
  }
});

test('payInvoiceWithIntent pays the intent entry with the invoice id as the request hash', async () => {
  const f = await stubFacilitator();
  try {
    const merchant = new InvoiceClient({ url: f.url, apiKey: KEY });
    const inv = newInvoice({ network: 'kaspa:testnet-10', reference: 'order-5', expiresAtMs: Date.now() + 600_000, accepts: [kasOffer('100000000'), intentOffer()] });
    const { id } = await merchant.create(inv);
    const fetched = await fetchInvoice(merchant.urlOf(id), { nowMs: Date.now() });
    const seen: unknown[] = [];
    const wasm = {
      payIntent(req: any) {
        seen.push(req);
        return { paymentPayload: { x402Version: 2, accepted: req.requirements }, transactionId: 'cd'.repeat(32), consumed: [], feeSompi: '1', expiresAtMs: 0, payerSpent: '3000', intent: { actor: 'TokenToKas_sell', state: {}, intent: {} } };
      },
    } as any;
    const r = await payInvoiceWithIntent(wasm, new InvoiceClient({ url: f.url }), fetched, { payAsset: '70'.repeat(32), utxos: [], nowMs: Date.now(), options: { maxSell: '3000' } });
    assert.equal((seen[0] as any).requestHash, id, 'the request hash of an invoice payment is the invoice id');
    assert.equal((seen[0] as any).requirements.extra.route.binding, 'kob-intent-v1');
    assert.equal(r.payment.payerSpent, '3000');
    assert.equal(f.paid.length, 1);
    assert.equal(f.paid[0]!.id, id);
    // a KobWasm without intent support is refused clearly
    await assert.rejects(payInvoiceWithIntent({} as any, new InvoiceClient({ url: f.url }), fetched, { payAsset: 'KAS', utxos: [], nowMs: 0, options: {} }), (e: unknown) => e instanceof KobX402Error && e.code === 'unsupported');
  } finally {
    await f.close();
  }
});
