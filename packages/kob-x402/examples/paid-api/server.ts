// Example resource server: `GET /report` costs 0.5 KAS, or N units of a KCC-20 token, or a swap-and-pay in token A.
// The facilitator is a real one (the `kob-executor x402` service); this process holds no keys.
//
//   KOB_X402_FACILITATOR=http://127.0.0.1:8402  \
//   KOB_X402_API_KEY=...                        \
//   KOB_X402_PAY_TO=kaspatest:qq...             \
//   node examples/paid-api/server.ts
//
// Environment:
//   KOB_X402_FACILITATOR   facilitator base URL (required)
//   KOB_X402_API_KEY       facilitator API key
//   KOB_X402_PAY_TO        merchant address, Schnorr P2PK (required)
//   KOB_X402_NETWORK       kaspa:testnet-10 (default) | kaspa:mainnet
//   KOB_X402_PORT          listen port (default 8402 is the facilitator's; this defaults to 8080)
//   KOB_X402_PUBLIC_URL    public origin the payers request, e.g. https://api.example.com (default: the request's own)
//   KOB_X402_PRICE_KAS     price in KAS (default 0.5)
//   KOB_X402_TOKEN         KCC-20 covenant id to also accept as the merchant asset (optional)
//   KOB_X402_TOKEN_UNITS   token base units (required with KOB_X402_TOKEN)
//   KOB_X402_TOKEN_CUSTODY unconditional (default) | issuer-controlled
//   KOB_X402_SWAP_PAY_ASSET  KCC-20 covenant id a payer may pay with through swap-and-pay (optional; the merchant
//                            still receives KAS, or the token above when KOB_X402_TOKEN_UNITS is set)

import { createServer } from 'node:http';
import { createPaywall, kasToSompi, loadKobWasm } from '../../src/index.ts';
import type { NetworkId, OfferSpec } from '../../src/index.ts';

function need(name: string): string {
  const v = process.env[name];
  if (!v) {
    console.error(`missing ${name}`);
    process.exit(2);
  }
  return v;
}

const network = (process.env.KOB_X402_NETWORK ?? 'kaspa:testnet-10') as NetworkId;
const priceKas = process.env.KOB_X402_PRICE_KAS ?? '0.5';
const token = process.env.KOB_X402_TOKEN;
const tokenUnits = process.env.KOB_X402_TOKEN_UNITS;
const swapPayAsset = process.env.KOB_X402_SWAP_PAY_ASSET;

const offers: OfferSpec[] = [{ kind: 'native', amount: kasToSompi(priceKas) }];
if (token) {
  if (!tokenUnits) throw new Error('KOB_X402_TOKEN needs KOB_X402_TOKEN_UNITS');
  const custody = process.env.KOB_X402_TOKEN_CUSTODY === 'issuer-controlled' ? 'issuer-controlled' : 'unconditional';
  offers.push({ kind: 'kcc20', asset: token, amount: tokenUnits, token: { custody } });
}
if (swapPayAsset) {
  offers.push({ kind: 'swap', receive: 'kas', amount: kasToSompi(priceKas), payAssets: [{ asset: swapPayAsset }] });
}

const wasm = await loadKobWasm();
const paywall = createPaywall({
  wasm,
  network,
  payTo: need('KOB_X402_PAY_TO'),
  offers,
  facilitator: { url: need('KOB_X402_FACILITATOR'), ...(process.env.KOB_X402_API_KEY ? { apiKey: process.env.KOB_X402_API_KEY } : {}) },
  ...(process.env.KOB_X402_PUBLIC_URL ? { publicUrl: process.env.KOB_X402_PUBLIC_URL } : {}),
  resource: { description: 'Research report', mimeType: 'application/json' },
  // Runs only after the facilitator has verified, broadcast and observed the payment.
  handler: (request, paid) => {
    const path = new URL(request.url).pathname;
    if (path !== '/report') return new Response('not found', { status: 404 });
    return Response.json({
      title: 'KOB example report',
      paidWith: { transaction: paid.transactionId, asset: paid.asset, amount: paid.amount, payer: paid.payer ?? null },
      replayed: paid.replayed,
    });
  },
});

// Only /report is priced; everything else is a free 404 (never put unrelated paths behind a paywall by accident).
// A signed transaction travels in PAYMENT-SIGNATURE, so the header limit is raised above node's 16 KB default.
const guarded = paywall.nodeListener();
const server = createServer({ maxHeaderSize: 256 * 1024 }, (req, res) => {
  if (new URL(req.url ?? '/', 'http://x').pathname !== '/report') return void res.writeHead(404).end('not found');
  guarded(req, res);
});

const port = Number(process.env.KOB_X402_PORT ?? 8080);
server.listen(port, () => {
  console.log(`paid API on :${port}  GET /report`);
  for (const o of paywall.offers()) console.log(`  offer: ${o.extra.profile}${o.extra.route ? '+swap' : ''} ${o.amount} ${o.asset === 'KAS' ? 'sompi' : `units of ${o.asset.slice(0, 12)}...`}`);
});
