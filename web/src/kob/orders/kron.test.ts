// The KRON family end to end through the planners: every order kind on a KRON token (KobAskKron, ..., 46-byte token state with address
// presence, no extension commitment), consensus-checked (sign locally, finalize with tightened budgets, script-engine validate) like the KCC-20 ones.
import { describe, expect, it } from 'vitest';
import type { CondIntent } from '../intent-cond';
import type { SimpleIntent } from '../intent-simple';
import { planSimple } from './simple';
import { errors } from '../plan-types';
import { MAKER_PK, TOK, consensusCheck, kob, makeEnv, marketKron, tokenUtxo } from '../../testing/fixtures';
import { askState, bidState, committedExit, ifdAsk, ifdBid, planOk } from './cond-testkit';
import { baseKind, familyOfKind } from '../order-facts';
import { isCovenantOwned, isKeyOwned, isKronState } from '../token-state';

const K = kob();
const env = () => makeEnv({ market: marketKron() });
const P = 250_000_000n;

describe('KRON market', () => {
  it('has the KRON program, family and no extension commitment', () => {
    const m = marketKron();
    expect(m).toMatchObject({ family: 'kron', program: 'KronToken2433', prefixLen: 0, suffixLen: 2387, slots: { inputs: 4, outputs: 5 }, extensionCommitment: '00'.repeat(32) });
    expect(tokenUtxo(m, 5000n).state).toMatchObject({ id_type: 3, is_minter: 0 });
  });
});

describe('simple orders on a KRON token', () => {
  const simple = (intent: SimpleIntent, e = env()) => {
    const p = planSimple(e, intent);
    expect(errors(p), JSON.stringify(errors(p))).toEqual([]);
    consensusCheck(K, p.built!);
    return p;
  };

  it('limit sell escrows the tokens in a covenant-owned KRON custody; limit buy carries no extension commitment', () => {
    const sell = simple({ type: 'limit', side: 'sell', price: P, amount: 10n * TOK });
    expect(sell.states[0].kind).toBe('KobAskKron');
    const rec = K.recoverOrders(sell.built!.tx);
    expect(rec).toHaveLength(1);
    expect(rec[0].custody!.state).toMatchObject({ id_type: 2, owner: rec[0].covenantId, amount: '10000' });
    expect(isCovenantOwned(rec[0].custody!.state)).toBe(true);
    // the KRON token input carries no signature of its own: only the P2PK funding input signs
    expect(sell.built!.plans.some((p) => p.kind === 'kronToken')).toBe(true);
    expect(sell.built!.sign.every((s) => s.pubkey === MAKER_PK)).toBe(true);
    const buy = simple({ type: 'limit', side: 'buy', price: P, amount: 10n * TOK });
    expect(buy.states[0].kind).toBe('KobBidKron');
    expect((buy.states[0].state as { extensionCommitment: string }).extensionCommitment).toBe('00'.repeat(32));
  });

  it('IOC and FOK on both sides validate', () => {
    for (const side of ['sell', 'buy'] as const) {
      simple({ type: 'ioc', side, price: side === 'sell' ? 245_000_000n : 250_000_000n, amount: 4n * TOK });
      simple({ type: 'fok', side, price: side === 'sell' ? 243_000_000n : 250_000_000n, amount: side === 'sell' ? 6n * TOK : 4n * TOK });
    }
  });

  it("the token change keeps the maker's KRON tokens key-owned (address presence)", () => {
    const p = simple({ type: 'limit', side: 'sell', price: P, amount: 10n * TOK }, makeEnv({ market: marketKron(), tokenAmounts: [100n * TOK] }));
    const plan = p.built!.plans.find((x) => x.kind === 'kronToken');
    expect(plan && plan.kind === 'kronToken' ? plan.nextStates.some((s) => isKeyOwned(s) && s.owner === MAKER_PK) : false).toBe(true);
  });
});

describe('conditional orders on a KRON token', () => {
  const cases: [string, CondIntent][] = [
    ['stop-market sell', { type: 'stopMarket', side: 'sell', amount: 4n * TOK, stop: 230_000_000n }],
    ['take-profit sell', { type: 'takeProfit', side: 'sell', amount: 4n * TOK, price: 300_000_000n }],
    ['OCO sell', { type: 'oco', side: 'sell', amount: 4n * TOK, takeProfit: 300_000_000n, stop: 230_000_000n }],
    ['OCO buy', { type: 'oco', side: 'buy', amount: 4n * TOK, takeProfit: 200_000_000n, stop: 260_000_000n }],
  ];
  it.each(cases)('%s', (_n, intent) => {
    const { plan } = planOk(env(), intent);
    expect(familyOfKind(plan.states[0]!.kind)).toBe('kron');
    expect(['KobCondAsk', 'KobCondBid']).toContain(baseKind(plan.states[0]!.kind));
  });

  it('IFD buy-first commits a 279-byte exit; sell-first commits the 288-byte KRON exit that decodes as the planned one', () => {
    const e = env();
    const buyFirst = planOk(e, { type: 'ifd', side: 'buy', amount: 10n * TOK, entry: { price: 240_000_000n }, exit: { takeProfit: 300_000_000n } });
    expect(ifdBid(buyFirst.plan.states[0]!).exitState.length / 2).toBe(279);
    expect(committedExit(e, buyFirst.plan.states[0]!)).toEqual(buyFirst.plan.states[1]);
    expect(askState(buyFirst.plan.states[1]!).amountLeft).toBe('10000');
    const sellFirst = planOk(e, { type: 'ifd', side: 'sell', amount: 10n * TOK, entry: { price: 260_000_000n }, exit: { takeProfit: 240_000_000n } });
    expect(ifdAsk(sellFirst.plan.states[0]!).exitState.length / 2).toBe(288);
    expect(committedExit(e, sellFirst.plan.states[0]!)).toEqual(sellFirst.plan.states[1]);
    expect(bidState(sellFirst.plan.states[1]!).extensionCommitment).toBe('00'.repeat(32));
  });

  it('IFO and repeat entries validate', () => {
    planOk(env(), { type: 'ifo', side: 'buy', amount: 10n * TOK, entry: { price: 240_000_000n }, exit: { takeProfit: 300_000_000n, stop: 200_000_000n } });
    planOk(env(), { type: 'repeatIfd', side: 'buy', amount: 10n * TOK, entry: { price: 240_000_000n }, exit: { takeProfit: 300_000_000n }, repeat: { count: 2n } });
  });

  it('a KRON custody above the token program output limit is refused by the planners', () => {
    const big = makeEnv({ market: marketKron(), tokenAmounts: [2_000_000_000n] });
    expect(planSimple(big, { type: 'limit', side: 'sell', price: P, amount: 1_000_000_001n }).issues.map((i) => i.code)).toEqual(['AMOUNT_TOO_LARGE']);
    expect(errors(planSimple(big, { type: 'limit', side: 'sell', price: 1_000n, amount: 1_000_000_000n }))).toEqual([]);
  });
});

describe('KRON token state helpers', () => {
  it('distinguish the layouts and ownership and round-trip through kob-wasm', () => {
    const k = { amount: '5', owner: MAKER_PK, id_type: 3, is_minter: 0 };
    expect(isKronState(k)).toBe(true);
    expect(isKeyOwned(k)).toBe(true);
    expect(isKeyOwned({ ...k, id_type: 0 })).toBe(false);
    expect(K.decodeTokenState(K.encodeTokenState(k))).toEqual(k);
  });
});
