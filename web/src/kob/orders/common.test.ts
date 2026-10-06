import { describe, expect, it } from 'vitest';
import {
  DEFAULT_CARRIER, DUST_OUTPUT_MIN, bidBudgetRate, bidEscrowOf, buildCreate, carrierOf, defaultMinFillFor, failedPlan, finishPlan, heldAmount, makeAsk, makeBid,
  makeDisclosure, refundTipFor, tokenCarrierFloor, usableTokenUtxos, verifyBuilt,
} from './common';
import { ISSUE_CATALOG, customIssue, hasError, issue } from './common-issues';
import { FIXTURE_CARRIER, KAS, MAKER_PK, TOK, makeEnv, tokenUtxo } from '../../testing/fixtures';
import type { OrderState } from '../types';
import { ceilDiv } from '../units';

const P = 250_000_000n;

describe('bid sizing (kob-protocol BidState::escrow)', () => {
  it('the budget rate is the all-in at the cap: a rising bid budgets its priceEnd', () => {
    expect(bidBudgetRate(P, 100_000n, 0n, 0n)).toBe(P + 100_000n);
    expect(bidBudgetRate(P, 100_000n, 1n, P + 5_000_000n)).toBe(P + 5_000_000n + 100_000n);
    // a "rising" bid whose end is below the start (or slope 0) is capped by its start price
    expect(bidBudgetRate(P, 0n, 1n, P - 1n)).toBe(P);
    expect(bidBudgetRate(P, 0n, 0n, P * 2n)).toBe(P);
  });

  it('escrow = used(amount) + (fills - 1) + fills x deliveryCarrier + reserve, used = ceil(amount x rate / scale) (kob-wasm bidEscrow)', () => {
    const env = makeEnv();
    const bid = makeBid(env, { minFill: TOK, price: 245_000_000n, tip: 100_000n, expiryDaa: 99n });
    expect(bidEscrowOf(env.kob, bid, 10n * TOK, 3n)).toBe(5_451_000_002n); // golden create.bid
    expect(bidEscrowOf(env.kob, bid, 5n * TOK, 5n)).toBe(6_225_500_004n); // golden create.bid.dca
    // an amount that is not a multiple of the scale: the budget is rounded up
    expect(env.kob.bidUsed(bid, 1_234n)).toBe(ceilDiv(1_234n * 245_100_000n, 1_000n));
    expect(bidEscrowOf(env.kob, bid, 1_234n, 2n)).toBe(ceilDiv(1_234n * 245_100_000n, 1_000n) + 1n + 2n * FIXTURE_CARRIER);
    const reserved = makeBid(env, { minFill: 1n, price: 1n, expiryDaa: 99n, deliveryCarrier: 2n, reserve: 3n });
    expect(bidEscrowOf(env.kob, reserved, 1_000n, 1n)).toBe(1n + 2n + 3n);
  });

  it('the default minimum fill is the amount worth 10 KAS at the price, clamped to 1..amount (kob-wasm defaultMinFill)', () => {
    const env = makeEnv();
    expect(defaultMinFillFor(env, 10n * TOK, P)).toBe(4_000n);
    expect(defaultMinFillFor(env, 10n * TOK, 300_000_000n)).toBe(3_334n);
    expect(defaultMinFillFor(env, 3n * TOK, P)).toBe(3n * TOK);
    expect(defaultMinFillFor(env, 10n * TOK, 1_000_000_000_000n)).toBe(1n);
    expect(defaultMinFillFor(env, 0n, P)).toBe(1n);
  });
});

describe('state builders', () => {
  it('makeAsk / makeBid carry the token identity and its scale, prices per whole token as given, and default sensibly', () => {
    // the wallet default carrier (no carrier in the env)
    const env = makeEnv({ carrier: null });
    const a = makeAsk(env, { amount: 3n * TOK, minFill: 500n, price: P, expiryDaa: 99n });
    expect(a).toEqual({
      kind: 'KobAsk',
      state: {
        maker: MAKER_PK, tokenCovId: env.token.covenantId, tokenTplHash: env.token.templateHash, tplPrefixLen: '1', tplSuffixLen: '2977', scale: '1000', minFill: '500',
        price: String(P), tip: '0', tif: '0', activeFrom: '0', expiryDaa: '99', refundTip: '3500000', interval: '0', maxFill: '0', slope: '0', priceEnd: '0',
        decayStep: '1', amountLeft: '3000',
      },
    });
    const b = makeBid(env, { minFill: 1n, price: P, expiryDaa: 99n, tif: 2, reserve: 7n });
    expect(b.kind).toBe('KobBid');
    expect(b.state).toMatchObject({ extensionCommitment: env.token.extensionCommitment, scale: '1000', minFill: '1', tif: '2', reserve: '7', deliveryCarrier: String(DEFAULT_CARRIER), refundTip: '3500000' });
    expect(carrierOf(env)).toBe(DEFAULT_CARRIER);
    expect(carrierOf({ carrier: 5n })).toBe(5n);
  });

  it('the refund tip is never below the program default (matcher.md §5)', () => {
    const env = makeEnv();
    const def = env.token.refundTip;
    expect(refundTipFor(env.token, 0n)).toBe(def);
    expect(refundTipFor(env.token, undefined)).toBe(def);
    expect(refundTipFor(env.token, def + 1n)).toBe(def + 1n);
    expect(makeAsk(env, { amount: TOK, minFill: 1n, price: P, expiryDaa: 99n, refundTip: 1n }).state).toMatchObject({ refundTip: String(def) });
    expect(makeBid(env, { minFill: 1n, price: P, expiryDaa: 99n, refundTip: 0n }).state).toMatchObject({ refundTip: String(def) });
    expect(makeAsk(env, { amount: TOK, minFill: 1n, price: P, expiryDaa: 99n, refundTip: def * 2n }).state).toMatchObject({ refundTip: String(def * 2n) });
  });
});

describe('token selection helpers', () => {
  it('usableTokenUtxos filters by token, owner, scheme, borrow flag and extension; heldAmount sums every base unit', () => {
    const env = makeEnv({ tokenAmounts: [] });
    const m = env.token;
    const good = tokenUtxo(m, 2_500n);
    const borrow = { ...tokenUtxo(m, 1_000n), state: { ...tokenUtxo(m, 1_000n).state, borrow_scheme: 1 } };
    env.tokenUtxos.push(good, borrow, tokenUtxo(m, 1_000n, 10n * KAS, 'cd'.repeat(32)));
    expect(usableTokenUtxos(env)).toEqual([good]);
    expect(heldAmount(env)).toBe(2_500n);
  });
});

describe('buildCreate / verifyBuilt', () => {
  const askSpec = (env = makeEnv(), amount = 4n * TOK) => {
    const order: OrderState = makeAsk(env, { amount, minFill: TOK, price: P, expiryDaa: env.clock.daa + 5_000n });
    return { env, order, spec: { order, value: carrierOf(env), tokenAmount: amount } };
  };

  it('builds a request with the selected tokens and funding and a self-verified transaction', () => {
    const { env, spec } = askSpec();
    const r = buildCreate(env, spec);
    expect(r.issues).toEqual([]);
    expect(r.request!.tokens).toHaveLength(1);
    expect(r.request!.tokenCarrier).toBe(String(FIXTURE_CARRIER));
    expect(r.tokenChange).toBe(96_000n);
    expect(r.tokenInputsKas).toBe(10n * KAS);
    expect(verifyBuilt(env, r.built!, spec)).toBeNull();
  });

  it('the wallet default carrier is kob-wasm defaultOrderCarrier (2 KAS): a default ask builds and verifies at it', () => {
    const { env, spec } = askSpec(makeEnv({ carrier: null }));
    expect(DEFAULT_CARRIER).toBe(2n * KAS);
    expect(env.kob.defaultConstants().defaultOrderCarrier).toBe(DEFAULT_CARRIER);
    expect(spec.value).toBe(DEFAULT_CARRIER);
    const r = buildCreate(env, spec);
    expect(r.issues).toEqual([]);
    expect(r.request!.tokenCarrier).toBe(String(DEFAULT_CARRIER));
    expect(verifyBuilt(env, r.built!, spec)).toBeNull();
  });

  it('verifyBuilt catches any divergence between plan and transaction', () => {
    const { env, order, spec } = askSpec();
    const r = buildCreate(env, spec);
    const built = r.built!;
    const other = makeAsk(env, { amount: 4n * TOK, minFill: TOK, price: P + 100n, expiryDaa: env.clock.daa + 5_000n });
    expect(verifyBuilt(env, built, { ...spec, order: other })).toMatch(/state differs/);
    expect(verifyBuilt(env, built, { ...spec, value: spec.value + 1n })).toMatch(/value differs/);
    expect(verifyBuilt(env, built, { ...spec, deadline: 5n })).toMatch(/deadline differs/);
    expect(verifyBuilt(env, built, { ...spec, tokenAmount: 5n * TOK })).toMatch(/custody amount differs/);
    // a transaction without a placement record is not a valid order creation
    const stripped = { ...built, tx: { ...built.tx, payload: '' } };
    expect(verifyBuilt(env, stripped, { ...spec, order })).toMatch(/placement record/);
  });

  it('refuses a carrier below the floor of a token output (program floor, KIP-9 dust bound) before building', () => {
    const env = makeEnv();
    const kaspacom = env.kob.templates().find((t) => t.name === 'KCC20KaspaCom_0_2_5')!;
    expect(tokenCarrierFloor(env.kob, kaspacom.hash)).toBe(50_000_000n);
    expect(tokenCarrierFloor(env.kob, env.token.templateHash)).toBe(DUST_OUTPUT_MIN);
    const { spec } = askSpec(makeEnv({ carrier: DUST_OUTPUT_MIN - 1n }));
    const r = buildCreate(makeEnv({ carrier: DUST_OUTPUT_MIN - 1n }), spec);
    expect(r.built).toBeNull();
    expect(r.issues.map((i) => i.code)).toEqual(['CARRIER_BELOW_FLOOR']);
    const bid = makeBid(env, { minFill: TOK, price: P, expiryDaa: env.clock.daa + 5_000n, deliveryCarrier: DUST_OUTPUT_MIN - 1n });
    const rb = buildCreate(env, { order: bid, value: 10n * KAS, tokenAmount: 0n });
    expect(rb.issues.map((i) => [i.code, i.field])).toEqual([['CARRIER_BELOW_FLOOR', 'deliveryCarrier']]);
  });

  it('reports failures as issues instead of throwing', () => {
    const { env, spec } = askSpec(makeEnv({ funding: [KAS] }));
    const r = buildCreate(env, spec);
    expect(r.built).toBeNull();
    expect(r.request).toBeNull();
    expect(r.issues.map((i) => i.code)).toEqual(['INSUFFICIENT_KAS']);
  });

  it('a token change is recycled into the wallet: only order + custody carriers are needed from the funding UTXO', () => {
    // the wallet has 100 tokens in one UTXO worth 10 KAS; selling 4 needs 10 (order) + 10 (custody) + 10 (change) - 10 (input) = 20 KAS + fee
    const { env, spec } = askSpec(makeEnv({ funding: [20n * KAS + 2_000_000n] }));
    expect(buildCreate(env, spec).built).not.toBeNull();
    const { env: env2, spec: spec2 } = askSpec(makeEnv({ funding: [20n * KAS - 1n] }));
    expect(buildCreate(env2, spec2).issues[0]).toMatchObject({ code: 'INSUFFICIENT_KAS', params: { needed: 20n * KAS, have: 20n * KAS - 1n, shortfall: 1n } });
  });
});

describe('plan assembly and disclosure', () => {
  it('failedPlan / finishPlan: ok only without errors and with a built tx', () => {
    const err = issue('AMOUNT_NOT_POSITIVE');
    const warn = issue('SLIPPAGE_HIGH', { bps: 2_000n });
    expect(failedPlan([err])).toMatchObject({ ok: false, built: null, request: null, disclosure: null });
    const env = makeEnv();
    const order = makeAsk(env, { amount: 1_234n, minFill: 1n, price: P, tip: 7n, expiryDaa: env.clock.daa + 5_000n });
    const res = buildCreate(env, { order, value: DEFAULT_CARRIER, tokenAmount: 1_234n });
    const disclosure = makeDisclosure(env, res, { order, amount: 1_234n, limitPrice: P, expectedPrice: null, worstPrice: null, expiryKind: 'gtc' });
    // the all-in total of an ask is what it receives at least: ceil(1234 x (P - 7) / 1000) (kob-wasm askProceedsAt)
    expect(disclosure).toMatchObject({ tokenAmount: 1_234n, scale: 1_000n, minFill: 1n, minTouch: null, allInPrice: P - 7n, tip: 7n });
    expect(disclosure.allInTotal).toBe(ceilDiv(1_234n * (P - 7n), 1_000n));
    expect(finishPlan([warn], [order], res, disclosure).ok).toBe(true);
    expect(finishPlan([err], [order], res, disclosure).ok).toBe(false);
    expect(finishPlan([], [order], { ...res, built: null, request: null }, disclosure)).toMatchObject({ ok: false, disclosure: null });
  });

  it('makeDisclosure refuses conditional kinds (their planner describes them)', () => {
    const env = makeEnv();
    const res = buildCreate(env, { order: makeAsk(env, { amount: TOK, minFill: 1n, price: P, expiryDaa: env.clock.daa + 1n }), value: DEFAULT_CARRIER, tokenAmount: TOK });
    const cond = { kind: 'KobCondAsk', state: {} } as unknown as OrderState;
    expect(() => makeDisclosure(env, res, { order: cond, amount: TOK, limitPrice: null, expectedPrice: null, worstPrice: null, expiryKind: 'gtc' })).toThrow(/conditional/);
  });
});

describe('issue catalogue', () => {
  it('fills placeholders, formats KAS and basis points, keeps unknown placeholders, and overrides severity on request', () => {
    expect(issue('TIP_EXCEEDS_PRICE', { tip: 150_000_000n }).message).toBe('The priority tip (1.5 KAS per token) must be smaller than the price.');
    expect(issue('SLIPPAGE_HIGH', { bps: 2_500n }).message).toContain('25.00%');
    expect(issue('PRICE_NOT_ON_TICK').message).toContain('{tick}');
    const i = issue('SLIPPAGE_HIGH', { bps: 1n }, 'slippageBps', 'error');
    expect(i).toMatchObject({ severity: 'error', field: 'slippageBps', params: { bps: 1n } });
    expect(hasError([issue('SLIPPAGE_HIGH', { bps: 1n })])).toBe(false);
    expect(hasError([issue('AMOUNT_NOT_POSITIVE')])).toBe(true);
    expect(customIssue('X_CODE', 'info', 'hello', { a: 1 }, 'f')).toEqual({ code: 'X_CODE', severity: 'info', message: 'hello', params: { a: 1 }, field: 'f' });
  });

  it('every catalogue entry has a severity and a non-empty English message; codes are upper snake case', () => {
    for (const [code, entry] of Object.entries(ISSUE_CATALOG)) {
      expect(code).toMatch(/^[A-Z][A-Z0-9_]+$/);
      expect(['error', 'warning', 'info']).toContain(entry.severity);
      expect(entry.message.length).toBeGreaterThan(10);
    }
    expect(Object.keys(ISSUE_CATALOG).length).toBeGreaterThan(40);
  });

});
