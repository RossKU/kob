import { test } from 'node:test';
import assert from 'node:assert/strict';
import { buildOffer, buildOffers, classifyOffer, kasToSompi, parsePaymentRequired, rankOffers, selectOffer } from '../src/offers.ts';
import { encodePaymentRequired } from '../src/headers.ts';
import { KobX402Error } from '../src/errors.ts';
import type { OfferSpec, PaymentRequired } from '../src/types.ts';
import { MERCHANT, NETWORK, TOKEN_A, TOKEN_B, loadVector } from './helpers/env.ts';
import { stubWasm } from './helpers/stub-wasm.ts';

const wasm = stubWasm();
const ctx = { wasm, network: NETWORK, payTo: MERCHANT, maxTimeoutSeconds: 60, finality: 'accepted' as const };
const TOKEN_C = 'cc'.repeat(32);

const native: OfferSpec = { kind: 'native', amount: '50000000' };
const kcc20: OfferSpec = { kind: 'kcc20', asset: TOKEN_B, amount: '700', token: { custody: 'unconditional', ticker: 'BBB', decimals: 2 } };
const swapKas: OfferSpec = { kind: 'swap', receive: 'kas', amount: '50000000', payAssets: [{ asset: TOKEN_A }] };
const swapTok: OfferSpec = { kind: 'swap', receive: 'kcc20', amount: '700', asset: TOKEN_B, token: { custody: 'unconditional' }, payAssets: [{ asset: TOKEN_C }] };

function envelope(accepts: unknown[]): PaymentRequired {
  return { x402Version: 2, resource: { url: 'https://m.example/report' }, accepts: accepts as PaymentRequired['accepts'] };
}

test('offer building: native, kcc20 and swap entries carry the binding fields', () => {
  const n = buildOffer(native, ctx);
  assert.equal(n.asset, 'KAS');
  assert.equal(n.extra.binding, 'kaspa-exact-v2');
  assert.equal(n.extra.profile, 'standard-native');
  assert.equal(n.extra.transactionEncoding, 'kaspa-sdk-safe-json-v2.0.0');
  assert.match(n.extra.payToScriptPublicKey, /^000020[0-9a-f]{64}ac$/);

  const k = buildOffer(kcc20, ctx);
  assert.equal(k.asset, TOKEN_B);
  assert.equal(k.extra.profile, 'kcc20');
  assert.equal(k.extra.token?.custody, 'unconditional');
  assert.equal(k.extra.token?.carrier, '100000000');
  assert.equal(k.extra.route, undefined);

  const s = buildOffer(swapKas, ctx);
  assert.equal(s.extra.profile, 'standard-native');
  assert.equal(s.extra.route?.binding, 'kob-swap-v1');
  assert.equal(s.extra.route?.critical, true);
  assert.equal(s.extra.route?.payAssets[0]?.asset, TOKEN_A);
  assert.match(s.extra.route?.payAssets[0]?.templateHash ?? '', /^[0-9a-f]{64}$/);

  const st = buildOffer(swapTok, ctx);
  assert.equal(st.extra.profile, 'kcc20');
  assert.equal(st.asset, TOKEN_B);
  for (const o of [n, k, s, st]) assert.ok(classifyOffer(o), 'every built offer classifies');
});

test('offer building rejects bad configuration', () => {
  assert.throws(() => buildOffer({ kind: 'native', amount: '0' }, ctx), KobX402Error);
  assert.throws(() => buildOffer({ kind: 'native', amount: '01' }, ctx), KobX402Error);
  assert.throws(() => buildOffer({ kind: 'native', amount: '1.5' }, ctx), KobX402Error);
  assert.throws(() => buildOffer({ kind: 'kcc20', asset: 'XYZ', amount: '1', token: { custody: 'unconditional' } }, ctx), KobX402Error);
  assert.throws(() => buildOffer({ kind: 'swap', receive: 'kas', amount: '1', payAssets: [] }, ctx), KobX402Error);
  assert.throws(() => buildOffers([], ctx), KobX402Error);
});

test('kasToSompi', () => {
  assert.equal(kasToSompi('0.5'), '50000000');
  assert.equal(kasToSompi('1'), '100000000');
  assert.equal(kasToSompi('0.00000001'), '1');
  assert.throws(() => kasToSompi('0.000000001'), KobX402Error);
  assert.throws(() => kasToSompi('-1'), KobX402Error);
  assert.throws(() => kasToSompi('1e3'), KobX402Error);
});

test('parsePaymentRequired: header, JSON text and value; rejects a broken envelope', () => {
  const pr = envelope([buildOffer(native, ctx)]);
  assert.deepEqual(parsePaymentRequired(encodePaymentRequired(pr)), pr);
  assert.deepEqual(parsePaymentRequired(JSON.stringify(pr)), pr);
  assert.deepEqual(parsePaymentRequired(pr), pr);
  assert.throws(() => parsePaymentRequired('%%%'), KobX402Error);
  assert.throws(() => parsePaymentRequired({ ...pr, x402Version: 1 }), KobX402Error);
  assert.throws(() => parsePaymentRequired({ ...pr, accepts: 'no' }), KobX402Error);
  assert.throws(() => parsePaymentRequired({ ...pr, resource: {} }), KobX402Error);
});

test('preference: standard-native, then kcc20, then swap (KAS-paying before token-paying)', () => {
  const offers = [swapTok, swapKas, kcc20, native].map((s) => buildOffer(s, ctx));
  const pr = envelope(offers);
  const rich = { network: NETWORK, tokens: { [TOKEN_A]: '5', [TOKEN_B]: '1000', [TOKEN_C]: '9' } } as const;
  const kinds = rankOffers(pr, rich).map((o) => `${o.kind}/${o.receives}`);
  assert.deepEqual(kinds, ['native/kas', 'kcc20/kcc20', 'swap/kas', 'swap/kcc20']);
  assert.equal(selectOffer(pr, rich)?.kind, 'native');
  // without the native offer: kcc20 first
  assert.equal(selectOffer(envelope(offers.slice(0, 3)), rich)?.kind, 'kcc20');
  // only swaps left: the KAS-paying route first, and the pay asset is one the payer holds
  const sw = selectOffer(envelope([offers[0]!, offers[1]!]), rich);
  assert.equal(sw?.kind, 'swap');
  assert.equal(sw?.receives, 'kas');
  assert.equal(sw?.payAsset, TOKEN_A);
});

test('capabilities: KAS-only payers never get token or swap offers; holdings gate kcc20 and swaps', () => {
  const offers = [kcc20, swapKas, swapTok].map((s) => buildOffer(s, ctx));
  const pr = envelope(offers);
  assert.equal(selectOffer(pr, { network: NETWORK, kasOnly: true, tokens: { [TOKEN_B]: '1000', [TOKEN_A]: '5' } }), null);
  assert.equal(selectOffer(pr, { network: NETWORK }), null, 'holds nothing');
  assert.equal(selectOffer(pr, { network: NETWORK, tokens: { [TOKEN_B]: '699' } }), null, 'kcc20: balance below the amount');
  assert.equal(selectOffer(pr, { network: NETWORK, tokens: { [TOKEN_B]: '700' } })?.kind, 'kcc20');
  assert.equal(selectOffer(pr, { network: NETWORK, tokens: { [TOKEN_A]: '1' } })?.kind, 'swap');
  assert.equal(selectOffer(pr, { network: NETWORK, tokens: { [TOKEN_A]: '1' }, allowSwap: false }), null);
  assert.equal(selectOffer(pr, { network: NETWORK, tokens: { [TOKEN_A]: '0' } }), null, 'zero balance is not held');
});

test('capabilities: network, ceilings and issuer-controlled custody', () => {
  const pr = envelope([buildOffer(native, ctx), buildOffer({ ...kcc20, token: { custody: 'issuer-controlled' } }, ctx)]);
  assert.equal(selectOffer(pr, { network: 'kaspa:mainnet' }), null, 'other network');
  assert.equal(selectOffer(pr, { network: NETWORK, maxAmount: { KAS: '49999999' } }), null, 'above the ceiling');
  assert.equal(selectOffer(pr, { network: NETWORK, maxAmount: { KAS: '50000000' } })?.kind, 'native');
  const onlyIssuer = envelope([pr.accepts[1]!]);
  const holds = { network: NETWORK, tokens: { [TOKEN_B]: '1000' } };
  assert.equal(selectOffer(onlyIssuer, holds), null, 'issuer-controlled is a risk flag, off by default');
  assert.equal(selectOffer(onlyIssuer, { ...holds, allowIssuerControlled: true })?.custody, 'issuer-controlled');
});

test('foreign entries are skipped, never fatal', () => {
  const good = buildOffer(native, ctx);
  const additive = loadVector('x402-http/exact-transaction.json').paymentRequired.accepts[0];
  const foreign = [
    { scheme: 'batch-settlement', network: NETWORK, amount: '1', asset: 'KAS', payTo: MERCHANT, maxTimeoutSeconds: 60, extra: { binding: 'kaspa-escrow-v3' } },
    { scheme: 'exact', network: 'eip155:8453', amount: '1000', asset: '0xUSDC', payTo: '0xabc', maxTimeoutSeconds: 60, extra: { name: 'USD Coin' } },
    { scheme: 'exact', network: 'kaspa:testnet-10', amount: '1', asset: 'KAS', payTo: MERCHANT, maxTimeoutSeconds: 60, extra: { binding: 'kaspa-exact-v9', profile: 'standard-native' } },
    additive,
    { ...good, amount: '001' },
    { ...good, amount: '-5' },
    { ...good, extra: { ...good.extra, finality: 'mempool' } },
    { ...good, extra: { ...good.extra, profile: 'kcc20' } }, // kcc20 without extra.token
    { ...good, extra: { ...good.extra, route: { binding: 'other-swap-v9', critical: true, payAssets: [{ asset: TOKEN_A }] } } }, // unknown critical route
    'garbage',
    null,
    42,
  ];
  const pr = envelope([...foreign, good]);
  const sel = selectOffer(pr, { network: NETWORK });
  assert.ok(sel);
  assert.equal(sel.index, foreign.length);
  assert.equal(sel.requirements, good);
  // an envelope with only foreign entries selects nothing (and does not throw)
  assert.equal(selectOffer(envelope(foreign), { network: NETWORK }), null);
  assert.equal(selectOffer(envelope([]), { network: NETWORK }), null);
});

test('swap routes that accept KAS are open to KAS-only payers; token-paying routes are not', () => {
  const kasRoute: OfferSpec = { kind: 'swap', receive: 'kcc20', amount: '700', asset: TOKEN_B, token: { custody: 'unconditional' }, payAssets: [{ asset: 'KAS' }, { asset: TOKEN_C }] };
  const built = buildOffer(kasRoute, ctx);
  assert.deepEqual(built.extra.route?.payAssets[0], { asset: 'KAS' }, 'the KAS pay asset carries no pinned program');
  assert.ok(classifyOffer(built));
  const pr = envelope([built]);
  assert.equal(selectOffer(pr, { network: NETWORK, kasOnly: true })?.payAsset, 'KAS');
  assert.equal(selectOffer(pr, { network: NETWORK })?.payAsset, 'KAS', 'KAS is listed first');
  assert.equal(selectOffer(pr, { network: NETWORK, kasOnly: true, allowSwap: false }), null);
  // a route listing only a token stays closed to a KAS-only payer
  assert.equal(selectOffer(envelope([buildOffer(swapTok, ctx)]), { network: NETWORK, kasOnly: true }), null);
  // KAS can only be a pay asset when the merchant receives a token
  assert.throws(() => buildOffer({ kind: 'swap', receive: 'kas', amount: '5', payAssets: [{ asset: 'KAS' }] }, ctx), KobX402Error);
});

test('swap offers take a KRON token (or a KaspaCom-template token) as a pay asset; a KRON token is never the merchant asset', () => {
  const KRON = 'dd'.repeat(32);
  const KRON_HASH = '2ed46a7edf5b168e67dba56998c58255235bebac436940a85115ca31d5c559f2'; // pinned KronToken2433
  const KASPACOM_HASH = '911f0638ccb7368bf36d117f1725073ae7ee487ce8b58ca3e8375051c2d40f6c'; // pinned KaspaCom KCC20 0.2.5
  const KC = 'ee'.repeat(32);
  // the configuration names the program and the family: no extension commitment for KRON, and no registry lookup
  const spec: OfferSpec = {
    kind: 'swap',
    receive: 'kas',
    amount: '50000000',
    payAssets: [
      { asset: KRON, templateHash: KRON_HASH, family: 'kron' },
      { asset: KC, templateHash: KASPACOM_HASH, extensionCommitment: 'cc'.repeat(32) },
    ],
  };
  const built = buildOffer(spec, ctx);
  assert.deepEqual(built.extra.route?.payAssets[0], { asset: KRON, templateHash: KRON_HASH, extensionCommitment: '0'.repeat(64) });
  assert.deepEqual(built.extra.route?.payAssets[1], { asset: KC, templateHash: KASPACOM_HASH, extensionCommitment: 'cc'.repeat(32) });
  assert.ok(classifyOffer(built));
  const pr = envelope([built]);
  // a payer holding the KRON token pays with it; one holding KaspaCom-template units pays with those
  assert.equal(selectOffer(pr, { network: NETWORK, tokens: { [KRON]: '3000' } })?.payAsset, KRON);
  assert.equal(selectOffer(pr, { network: NETWORK, tokens: { [KC]: '3000' } })?.payAsset, KC);
  assert.equal(selectOffer(pr, { network: NETWORK }), null, 'holds neither');
  // without the family the stub registry lookup answers: the merchant asset stays kcc20
  const k = buildOffer(kcc20, ctx);
  assert.ok(classifyOffer(k));
  const krMerchant = { ...k, extra: { ...k.extra, token: { ...k.extra.token!, family: 'kron' } } };
  assert.equal(classifyOffer(krMerchant), null, 'a KRON token is not a kcc20 merchant asset');
});
