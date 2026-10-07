import { describe, expect, it } from 'vitest';
import {
  SIGNING_ISSUE_CODES, decodeSigning, describeInputsForWallet, p2pkOwner, type ExpectedSigning, type SigningIssueCode, type SigningSummary,
} from './decode';
import { parseRegistry, type TokenRegistry } from './registry';
import { loadKobNode } from './wasm.node';
import type { ActionRequest, BuiltTx, Hex, OrderState } from './types';
import { goldenRequest, MAKER, OTHER, placeGolden, signAndValidate, TOKEN, tradableRegistryJson, nodeFactsOf } from '../testing/chain-fixtures';

const kob = loadKobNode();
const registry: TokenRegistry = parseRegistry(tradableRegistryJson(), { kob });
const clone = <T>(v: T): T => JSON.parse(JSON.stringify(v)) as T;
const KAS = 100_000_000n;
/** the reference program's refund and keeper tips (kob-wasm keeperTips) */
const REFUND = BigInt(kob.keeperTips().KCC20Ref.refundTip);
const KEEPER = BigInt(kob.keeperTips().KCC20Ref.keeperTip);

const buildGolden = (name: string, maker = MAKER): BuiltTx => kob.build(goldenRequest<ActionRequest>(name, maker.pk));
const decode = (built: BuiltTx, extra: { expected?: ExpectedSigning; maker?: Hex; registry?: TokenRegistry | null } = {}): SigningSummary =>
  decodeSigning({ kob, built, maker: extra.maker ?? MAKER.pk, registry: extra.registry === undefined ? registry : extra.registry, expected: extra.expected, nodeInputs: nodeFactsOf(built) });
const blockingCodes = (s: SigningSummary): SigningIssueCode[] => s.blocking.map((b) => b.code);
const p2pkSpk = (pk: Hex) => `000020${pk}ac`;
const orderStateOf = (name: string): OrderState => (goldenRequest<{ order: OrderState }>(name).order);

describe('create transactions: what the wallet will sign, derived from the built tx only', () => {
  it('create.ask: a limit sell escrows tokens in custody, returns token change, locks carriers', () => {
    const built = buildGolden('create.ask');
    const s = decode(built);
    expect(s.ok).toBe(true);
    expect(s.blocking).toEqual([]);
    expect(s.kind).toBe('create');
    expect(s.signatures).toBe(2);
    // one order, re-derived from the placement record
    expect(s.orders).toHaveLength(1);
    const o = s.orders[0];
    expect(o.verified).toBe(true);
    expect(o.covenantId).toBe(built.covenants[0].covenantId);
    expect(o.description).toMatchObject({ kind: 'KobAsk', side: 'sell', scale: 1000n, minFill: 1000n, amountLeft: 10_000n, price: 250_000_000n, tip: 100_000n, tokenAmount: 10_000n, tif: 'gtc', refundTip: REFUND, maker: MAKER.pk });
    expect(o.custody).toMatchObject({ amount: 10_000n, carrier: 10n * KAS });
    expect(o.value).toBe(10n * KAS);
    // inputs: token leader (maker-owned, signed) + funding (signed)
    expect(s.inputs.map((i) => [i.type, i.ownedBy, i.willSign])).toEqual([['token', 'maker', true], ['kas', 'maker', true]]);
    expect(s.inputs[0].token).toMatchObject({ amount: 12_000n, ownerScheme: 0, leader: true, program: 'KCC20Ref' });
    expect(s.inputs[0].token!.ref.display).toBe('TST (7070…7070) [verified]');
    // outputs: order, custody, token change, KAS change
    expect(s.outputs.map((x) => x.kind)).toEqual(['order', 'custody', 'token-change', 'kas-change']);
    expect(s.outputs[1].token).toMatchObject({ amount: 10_000n, ownerScheme: 4, escrowOf: o.covenantId });
    // net effects
    const t = s.net.tokens[0];
    expect(t).toMatchObject({ fromWallet: 12_000n, toMaker: 2_000n, escrowed: 10_000n, toOthers: 0n, walletDelta: -10_000n });
    expect(t.human.escrowed).toBe('10 TST');
    expect(t.human.walletDelta).toBe('-10 TST');
    expect(s.net.kas.locked).toMatchObject({ total: 20n * KAS, carriers: 20n * KAS - REFUND, refundTips: REFUND, escrow: 0n });
    expect(s.net.kas.toOthers).toBe(0n);
    // fee re-derived from inputs minus outputs equals the builder's report
    expect(s.fee.sompi).toBe(BigInt(built.fee.fee));
    expect(s.net.kas.fromWallet - s.net.kas.toWallet).toBe(s.net.kas.locked.total + s.fee.sompi);
    expect(s.warnings).toEqual([]);
  });

  it('a consensus-valid placement decodes without findings (built, signed locally, validated in the script engine)', () => {
    for (const name of ['create.ask', 'create.bid', 'create.condAsk', 'create.condBid', 'create.ifdBid', 'create.ifdAsk']) {
      const placed = placeGolden(kob, name);
      expect(placed.signed.tx.id).toBe(placed.built.tx.id);
      const s = decode(placed.built);
      expect(s.blocking, name).toEqual([]);
      expect(s.orders.map((o) => o.covenantId), name).toEqual(placed.recovered.map((r) => r.covenantId));
    }
  });

  it('create.bid: escrow is the KAS budget, carrier reserves and refund tip are itemised, no tokens move', () => {
    const s = decode(buildGolden('create.bid'));
    expect(s.blocking).toEqual([]);
    const o = s.orders[0];
    // a bid's quantity is its KAS budget: no amount of tokens
    expect(o.description).toMatchObject({ kind: 'KobBid', side: 'buy', amountLeft: null, tokenAmount: null });
    expect(o.custody).toBeNull();
    const value = BigInt(goldenRequest<{ value: string }>('create.bid').value);
    expect(o.value).toBe(value);
    // the delivery carrier (10 KAS) is the reserve line; the escrow is what is left after the refund tip
    expect(o.locked).toMatchObject({ total: value, escrow: value - REFUND - 10n * KAS, refundTips: REFUND, reserves: 10n * KAS, carriers: 0n });
    expect(s.net.tokens).toEqual([]);
    expect(s.signatures).toBe(1);
  });

  it('every order type decodes to its kind, side and terms', () => {
    const cases: [string, string, 'sell' | 'buy'][] = [
      ['create.ask', 'KobAsk', 'sell'], ['create.ask.twap', 'KobAsk', 'sell'], ['create.ask.dutch', 'KobAsk', 'sell'], ['create.ask.market', 'KobAsk', 'sell'],
      ['create.bid', 'KobBid', 'buy'], ['create.bid.dca', 'KobBid', 'buy'], ['create.bid.market', 'KobBid', 'buy'],
      ['create.condAsk', 'KobCondAsk', 'sell'], ['create.condBid', 'KobCondBid', 'buy'],
      ['create.ifdBid', 'KobIfdBid', 'buy'], ['create.ifdBid.stopEntry', 'KobIfdBid', 'buy'], ['create.ifdBid.repeat', 'KobIfdBid', 'buy'],
      ['create.ifdAsk', 'KobIfdAsk', 'sell'], ['create.ifdAsk.repeat', 'KobIfdAsk', 'sell'],
    ];
    for (const [name, kind, side] of cases) {
      const s = decode(buildGolden(name));
      expect(s.blocking, name).toEqual([]);
      expect(s.orders, name).toHaveLength(1);
      expect(s.orders[0].description.kind, name).toBe(kind);
      expect(s.orders[0].description.side, name).toBe(side);
    }
    const twap = decode(buildGolden('create.ask.twap')).orders[0].description;
    expect(twap.auction).not.toBeNull();
    expect(twap.auction!.interval > 0n && twap.auction!.maxFill > 0n).toBe(true);
    const cond = decode(buildGolden('create.condAsk')).orders[0].description;
    expect(cond.trigger).toMatchObject({ stopPrice: 200_000_000n, slipBps: 300n, armed: 0n, keeperTip: KEEPER, minTouch: 1n });
    expect(cond.price).toBe(300_000_000n);
    const stopEntry = decode(buildGolden('create.ifdBid.stopEntry')).orders[0].description;
    expect(stopEntry.entry!.entryStop > 0n).toBe(true);
    const repeat = decode(buildGolden('create.ifdBid.repeat')).orders[0].description;
    expect(repeat.entry!.rptAmount > repeat.amountLeft!).toBe(true);
  });

  it('an if-done entry shows the exit it commits to (take-profit / stop-loss) and its carriers', () => {
    const s = decode(buildGolden('create.ifdBid'));
    const d = s.orders[0].description;
    expect(d.entry).not.toBeNull();
    const exit = d.entry!.exit!;
    expect(exit).toMatchObject({ kind: 'KobCondAsk', side: 'sell', maker: MAKER.pk });
    expect(exit.price > d.price).toBe(true); // take-profit above the entry limit
    expect(exit.trigger!.stopPrice > 0n).toBe(true);
    expect(s.orders[0].locked.reserves).toBe(d.reservedKas);
    const sellFirst = decode(buildGolden('create.ifdAsk')).orders[0];
    expect(sellFirst.custody!.amount).toBe(sellFirst.description.tokenAmount);
    expect(sellFirst.description.entry!.exit!.kind).toBe('KobCondBid');
  });

  it('day orders show their wall-clock deadline; x402 / note records of the payload are surfaced', () => {
    const day = decode(buildGolden('create.ask.day'));
    expect(day.orders[0].deadline).toBe(1_790_726_400n);
    const x402 = decode(buildGolden('create.ask.8x8.x402'), { registry: null });
    expect(x402.blocking).toEqual([]);
    expect(x402.payload!.records.map((r) => r.type)).toContain('x402');
  });

  it('a registry-less decode still works and flags the token as unlisted (never trusted by name)', () => {
    const s = decode(buildGolden('create.ask'), { registry: null });
    expect(s.blocking).toEqual([]);
    expect(s.warnings.map((w) => w.code)).toEqual(['token-unlisted']);
    expect(s.net.tokens[0].ref).toMatchObject({ ticker: null, decimals: null, inRegistry: false, tradable: false });
    expect(s.net.tokens[0].ref.display).toBe('unknown token (7070…7070)');
    expect(s.net.tokens[0].human.escrowed).toBe('10000 base units');
  });
});

describe('cancel, cancel-replace, cancel-position, refund and send', () => {
  it('cancel.ask returns custody + carriers to the maker; nothing leaves', () => {
    const built = buildGolden('cancel.ask');
    const s = decode(built);
    expect(s.blocking).toEqual([]);
    expect(s.kind).toBe('cancel');
    expect(s.signatures).toBe(1);
    expect(s.spends).toHaveLength(1);
    expect(s.spends[0]).toMatchObject({ action: 'cancel', makerIsWallet: true, tokensReleased: 10_000n, strays: 0n });
    expect(s.spends[0].description).toMatchObject({ kind: 'KobAsk', amountLeft: 10_000n });
    expect(s.inputs.map((i) => [i.type, i.ownedBy, i.willSign])).toEqual([['order', 'covenant', true], ['token', 'covenant', false]]);
    expect(s.orders).toEqual([]);
    expect(s.outputs.map((o) => o.kind)).toEqual(['token-change', 'kas-change']);
    expect(s.net.tokens[0]).toMatchObject({ released: 10_000n, toMaker: 10_000n, fromWallet: 0n, escrowed: 0n, walletDelta: 10_000n });
    expect(s.net.kas).toMatchObject({ toOthers: 0n });
    expect(s.net.kas.locked.total).toBe(0n);
    expect(s.net.kas.released).toBe(20n * KAS);
    expect(s.info.map((i) => i.code)).toContain('kas-released');
  });

  it('cancel.ask.sweepStray reports the swept stray tokens', () => {
    const s = decode(buildGolden('cancel.ask.sweepStray'));
    expect(s.blocking).toEqual([]);
    expect(s.spends[0]).toMatchObject({ tokensReleased: 10_001n, strays: 1n });
    expect(s.net.tokens[0].toMaker).toBe(10_001n);
    expect(s.info.map((i) => i.code)).toContain('strays-swept');
  });

  it('cancel of every order kind: only the maker signs and everything returns to the maker', () => {
    for (const name of ['cancel.ask', 'cancel.bid', 'cancel.condAsk', 'cancel.condBid', 'cancel.ifdBid', 'cancel.ifdAsk', 'cancel.ifdAsk.emptyRepeat', 'cancel.bid.sweepStray']) {
      const s = decode(buildGolden(name));
      expect(s.blocking, name).toEqual([]);
      expect(s.kind, name).toBe('cancel');
      expect(s.net.kas.toOthers, name).toBe(0n);
      expect(s.net.tokens.every((t) => t.toOthers === 0n && t.escrowed === 0n), name).toBe(true);
    }
  });

  it('cancel.position shows every order of the position and one token return', () => {
    const s = decode(buildGolden('cancel.position.repeatBuyFirst'));
    expect(s.blocking).toEqual([]);
    expect(s.kind).toBe('cancel-position');
    expect(s.spends.map((x) => x.description!.kind)).toEqual(['KobIfdBid', 'KobCondAsk', 'KobCondAsk']);
    expect(s.spends.every((x) => x.action === 'cancel')).toBe(true);
    expect(s.signatures).toBe(3);
    expect(s.outputs.filter((o) => o.kind === 'token-change')).toHaveLength(1);
    expect(s.net.tokens[0].toMaker).toBe(7_000n);
    expect(s.spends[1].description!.booked).not.toBeNull(); // a booked exit of the repeat
  });

  it('cancel-replace: the replacement order is decoded next to the cancelled one, tokens are re-escrowed', () => {
    const s = decode(buildGolden('cancelReplace.askToOco.partial'));
    expect(s.blocking).toEqual([]);
    expect(s.kind).toBe('cancel-replace');
    expect(s.spends[0].description!.kind).toBe('KobAsk');
    expect(s.orders[0].description).toMatchObject({ kind: 'KobCondAsk', amountLeft: 6_000n });
    expect(s.net.tokens[0]).toMatchObject({ released: 10_000n, escrowed: 6_000n, toMaker: 4_000n });
    const topUp = decode(buildGolden('cancelReplace.ask.topUp'));
    expect(topUp.blocking).toEqual([]);
    expect(topUp.net.tokens[0]).toMatchObject({ released: 10_000n, fromWallet: 3_000n, escrowed: 12_000n, toMaker: 1_000n });
    const bid = decode(buildGolden('cancelReplace.bid'));
    expect(bid.orders[0].description.kind).toBe('KobBid');
    expect(bid.blocking).toEqual([]);
  });

  it('refunds need no wallet signature and pay the maker (keeper tip is the fee unless the maker refunds)', () => {
    for (const name of ['refund.ask.expiry', 'refund.bid', 'refund.condAsk', 'refund.ifdBid', 'close.ifdAsk.emptyRepeat', 'refund.ask.iocKill']) {
      const s = decode(buildGolden(name));
      expect(s.blocking, name).toEqual([]);
      expect(s.kind, name).toBe('refund');
      expect(s.signatures, name).toBe(0);
      expect(s.spends[0].action, name).toBe('refund');
    }
  });

  it('send.tokens: a transfer to another key is BLOCKING unless the plan announced exactly that transfer', () => {
    const built = buildGolden('send.tokens', OTHER); // the golden sender key becomes OTHER; recipient stays the golden 531fe6...
    const s = decode(built, { maker: OTHER.pk });
    expect(s.kind).toBe('send');
    expect(blockingCodes(s)).toEqual(['transfer-out']);
    const recipient = s.outputs.find((o) => o.kind === 'token-out')!.recipient!;
    expect(s.net.tokens[0]).toMatchObject({ fromWallet: 12_000n, toOthers: 9_000n, toMaker: 3_000n, walletDelta: -9_000n });
    const ok = decode(built, { maker: OTHER.pk, expected: { transfers: [{ pubkey: recipient, amount: 9_000n, covenantId: TOKEN.covenantId }] } });
    expect(ok.blocking).toEqual([]);
    expect(ok.info.map((i) => i.code)).toContain('transfer-expected');
    const wrongAmount = decode(built, { maker: OTHER.pk, expected: { transfers: [{ pubkey: recipient, amount: 8_999n }] } });
    expect(blockingCodes(wrongAmount)).toEqual(['transfer-out']);
    const wrongKey = decode(built, { maker: OTHER.pk, expected: { transfers: [{ pubkey: MAKER.pk, amount: 9_000n }] } });
    expect(blockingCodes(wrongKey)).toEqual(['transfer-out']);
  });
});

describe('expected: the planner\'s claims are compared with the derived facts', () => {
  it('matching claims produce no finding', () => {
    const built = buildGolden('create.ask');
    const s = decode(built, {
      expected: { orders: [orderStateOf('create.ask')], kasLocked: 20n * KAS, tokensEscrowed: 10_000n, maxFee: 20_000_000n },
    });
    expect(s.blocking).toEqual([]);
    // kasLocked may also be stated as the net wallet outflow or including the token-change carrier (planner disclosure vocabulary)
    expect(decode(built, { expected: { kasLocked: 30n * KAS } }).blocking).toEqual([]);
  });

  it('a different order (any state field) is blocking', () => {
    const built = buildGolden('create.ask');
    for (const mutate of [
      (st: any) => { st.price = '250000001'; },
      (st: any) => { st.amountLeft = '9999'; },
      (st: any) => { st.minFill = '999'; },
      (st: any) => { st.tip = '0'; },
      (st: any) => { st.expiryDaa = '400000001'; },
    ]) {
      const exp = clone(orderStateOf('create.ask'));
      mutate(exp.state);
      expect(blockingCodes(decode(built, { expected: { orders: [exp] } })), JSON.stringify(exp.state)).toEqual(['expected-orders']);
    }
    expect(blockingCodes(decode(built, { expected: { orders: [] } }))).toEqual(['expected-orders']);
    expect(blockingCodes(decode(built, { expected: { orders: [orderStateOf('create.ask'), orderStateOf('create.ask')] } }))).toEqual(['expected-orders']);
  });

  it('wrong locked KAS, escrowed tokens, cancel ids or a fee above the plan are blocking', () => {
    const built = buildGolden('create.ask');
    expect(blockingCodes(decode(built, { expected: { kasLocked: 19n * KAS } }))).toEqual(['expected-kas-locked']);
    expect(blockingCodes(decode(built, { expected: { tokensEscrowed: 10_001n } }))).toEqual(['expected-tokens-escrowed']);
    const goldenFee = BigInt(built.fee.fee);
    expect(blockingCodes(decode(built, { expected: { maxFee: goldenFee } }))).toEqual([]);
    expect(blockingCodes(decode(built, { expected: { maxFee: goldenFee - 1n } }))).toEqual(['expected-max-fee']);
    const cancel = buildGolden('cancel.ask');
    const id = decode(cancel).spends[0].covenantId!;
    expect(decode(cancel, { expected: { cancelIds: [id] } }).blocking).toEqual([]);
    expect(blockingCodes(decode(cancel, { expected: { cancelIds: ['ab'.repeat(32)] } }))).toEqual(['expected-cancel-ids']);
    expect(blockingCodes(decode(cancel, { expected: { cancelIds: [] } }))).toEqual(['expected-cancel-ids']);
  });

  it('an if-done plan lists the committed exit last: it matches the entry\'s commitment; a different exit does not', () => {
    const built = buildGolden('create.ifdBid');
    const s = decode(built);
    const entry = orderStateOf('create.ifdBid');
    const exitPlain = kob.decodeState(
      'KobCondAsk',
      (entry.state as any).exitState + '20' + '00'.repeat(32) + '08' + '00'.repeat(8) + '08' + '00'.repeat(8) + '20' + (entry.state as any).extensionCommitment,
    ) as OrderState;
    const withExit = decode(built, { expected: { orders: [entry, { ...exitPlain, state: { ...exitPlain.state, amountLeft: '5000' } } as OrderState] } });
    expect(withExit.blocking).toEqual([]);
    expect(s.blocking).toEqual([]);
    const badExit = { ...exitPlain, state: { ...exitPlain.state, tpPrice: String(BigInt((exitPlain.state as any).tpPrice) + 1n) } } as OrderState;
    expect(blockingCodes(decode(built, { expected: { orders: [entry, badExit] } }))).toEqual(['expected-orders']);
  });
});

describe('tamper tests: every way of making the wallet sign something else is a blocking finding', () => {
  const tampered = (name: string, f: (b: BuiltTx) => void, maker = MAKER): BuiltTx => {
    const b = clone(buildGolden(name, maker));
    f(b);
    return b;
  };

  it('an output redirected to another key is a payment out', () => {
    const b = tampered('create.ask', (x) => { x.tx.outputs[3].scriptPublicKey = p2pkSpk(OTHER.pk); });
    const s = decode(b);
    expect(s.ok).toBe(false);
    expect(blockingCodes(s)).toEqual(['payment-out']);
    expect(s.outputs[3]).toMatchObject({ kind: 'payment-out', flagged: true, recipient: OTHER.pk });
    expect(s.net.kas.toOthers).toBe(BigInt(b.tx.outputs[3].value));
  });

  it('an output redirected to an unknown script is blocking', () => {
    const b = tampered('cancel.bid', (x) => { x.tx.outputs[0].scriptPublicKey = '0000aa20' + 'cd'.repeat(32) + '87'; });
    expect(blockingCodes(decode(b))).toEqual(['output-unknown']);
  });

  it('an inflated fee (change output shaved) is blocking twice: it contradicts the builder and it is excessive', () => {
    const b = tampered('create.ask', (x) => { x.tx.outputs[3].value = (BigInt(x.tx.outputs[3].value) - 5n * KAS).toString(); });
    const s = decode(b);
    expect(blockingCodes(s).sort()).toEqual(['fee-excessive', 'fee-mismatch']);
    expect(s.fee.sompi).toBe(BigInt(b.fee.fee) + 5n * KAS);
  });

  it('a fee that matches the report but is far above the minimum warns (not blocking below the excess bound)', () => {
    const b = tampered('create.ask', (x) => {
      const extra = 5_000_000n;
      x.tx.outputs[3].value = (BigInt(x.tx.outputs[3].value) - extra).toString();
      x.fee.fee = (BigInt(x.fee.fee) + extra).toString();
    });
    const s = decode(b);
    expect(s.blocking).toEqual([]);
    expect(s.warnings.map((w) => w.code)).toContain('fee-high');
  });

  it('outputs exceeding the inputs are blocking (kas-unbalanced)', () => {
    const b = tampered('create.ask', (x) => { x.tx.outputs[3].value = (BigInt(x.tx.outputs[3].value) + 10_000n * KAS).toString(); });
    const codes = blockingCodes(decode(b));
    expect(codes).toContain('kas-unbalanced');
    expect(codes).toContain('fee-mismatch');
  });

  it('a state field changed in the payload (output script kept) fails the placement record check', () => {
    const b = tampered('create.ask', (x) => {
      // price 250_000_000: LEB128 80 e5 9a 77 inside the compact state of the (version 3) placement record
      const price = '80e59a77';
      expect(x.tx.payload).toContain(price);
      x.tx.payload = x.tx.payload.replace(price, '81e59a77');
    });
    const s = decode(b);
    expect(blockingCodes(s)).toContain('recover-failed');
    expect(blockingCodes(s)).toContain('order-unrecorded');
    expect(s.orders).toEqual([]);
  });

  it('an order output whose script was swapped for another order state is not the recorded one', () => {
    const other = buildGolden('create.ask.twap');
    const b = tampered('create.ask', (x) => { x.tx.outputs[0].scriptPublicKey = other.tx.outputs[0].scriptPublicKey; });
    expect(blockingCodes(decode(b))).toContain('recover-failed');
  });

  it('a stripped payload leaves an unrecorded covenant', () => {
    const b = tampered('create.bid', (x) => { x.tx.payload = ''; });
    const s = decode(b);
    expect(blockingCodes(s).sort()).toEqual(['order-unrecorded', 'output-unknown']);
  });

  it('tokens redirected to another key are a transfer out (consistent tamper) or a script mismatch (inconsistent one)', () => {
    const consistent = tampered('create.ask', (x) => {
      const plan: any = x.plans[0];
      plan.nextStates[1].owner = OTHER.pk;
      x.tx.outputs[2].scriptPublicKey = kob.tokenScriptPublicKey('KCC20Ref', plan.nextStates[1]);
    });
    const s = decode(consistent);
    expect(blockingCodes(s)).toEqual(['transfer-out']);
    expect(s.net.tokens[0].toOthers).toBe(2_000n);
    const inconsistent = tampered('create.ask', (x) => { (x.plans[0] as any).nextStates[1].owner = OTHER.pk; });
    const codes = blockingCodes(decode(inconsistent));
    expect(codes).toContain('output-script-mismatch');
    expect(codes).toContain('transfer-out');
  });

  it('tokens locked in a covenant this tx does not create are blocking', () => {
    const b = tampered('create.ask', (x) => {
      const plan: any = x.plans[0];
      plan.nextStates[0].owner = 'ab'.repeat(32);
      x.tx.outputs[1].scriptPublicKey = kob.tokenScriptPublicKey('KCC20Ref', plan.nextStates[0]);
    });
    const codes = blockingCodes(decode(b));
    expect(codes).toContain('token-to-unknown-owner');
    expect(codes).toContain('recover-failed');
  });

  it('tokens created or destroyed (input and output sums differ) are blocking', () => {
    const b = tampered('create.ask', (x) => {
      const plan: any = x.plans[0];
      plan.nextStates[1].amount = '1999';
      x.tx.outputs[2].scriptPublicKey = kob.tokenScriptPublicKey('KCC20Ref', plan.nextStates[1]);
    });
    expect(blockingCodes(decode(b))).toEqual(['token-unbalanced']);
  });

  it('a token output the plan does not list is blocking', () => {
    const b = tampered('create.ask', (x) => { (x.plans[0] as any).nextStates.pop(); });
    expect(blockingCodes(decode(b))).toContain('token-output-count');
  });

  it('an input whose UTXO script is not the one its plan describes is blocking', () => {
    const b = tampered('cancel.ask', (x) => { x.tx.inputs[0].utxo.scriptPublicKey = '0000aa20' + 'ee'.repeat(32) + '87'; });
    expect(blockingCodes(decode(b))).toEqual(['input-script-mismatch']);
    const c = tampered('create.ask', (x) => { x.tx.inputs[1].utxo.scriptPublicKey = p2pkSpk(OTHER.pk); });
    expect(blockingCodes(decode(c))).toEqual(['input-script-mismatch']);
    const d = tampered('cancel.ask', (x) => { (x.plans[1] as any).state.amount = '10001'; });
    expect(blockingCodes(decode(d))).toContain('input-script-mismatch');
  });

  it('another key\'s order: creating or cancelling it with this wallet is blocking', () => {
    const create = buildGolden('create.ask', OTHER);
    const s = decode(create); // wallet is MAKER
    // the golden change also returns to OTHER: KAS and tokens would leave the wallet key
    expect(blockingCodes(s).sort()).toEqual(['order-not-maker', 'payment-out', 'sign-foreign-key', 'sign-foreign-key', 'transfer-out']);
    const cancel = buildGolden('cancel.ask', OTHER);
    const c = decode(cancel);
    expect(blockingCodes(c)).toContain('order-not-maker');
    expect(blockingCodes(c)).toContain('sign-foreign-key');
    expect(c.spends[0].makerIsWallet).toBe(false);
  });

  it('signature requests are checked: foreign key, wrong sighash type, unknown input, plan mismatch', () => {
    const foreign = tampered('create.bid', (x) => { x.sign[0].pubkey = OTHER.pk; });
    expect(blockingCodes(decode(foreign)).sort()).toEqual(['sign-foreign-key', 'sign-mismatch']);
    const type = tampered('create.bid', (x) => { x.sign[0].sighashType = 2; });
    expect(blockingCodes(decode(type))).toEqual(['sighash-type']);
    const ghost = tampered('create.bid', (x) => { x.sign[0].inputIndex = 7; });
    expect(blockingCodes(decode(ghost))).toEqual(['sign-unexpected']);
    const onCovenant = tampered('cancel.ask', (x) => { x.sign.push({ ...x.sign[0], inputIndex: 1 }); });
    expect(blockingCodes(decode(onCovenant))).toEqual(['sign-unexpected']);
    const missing = tampered('create.bid', (x) => { x.sign = []; });
    expect(decode(missing).warnings.map((w) => w.code)).toContain('unsigned-maker-input');
  });

  it('a malformed built (plans / roles do not line up with the inputs) is refused outright', () => {
    const b = tampered('create.ask', (x) => { x.plans.pop(); });
    const s = decode(b);
    expect(blockingCodes(s)).toEqual(['malformed-built']);
    expect(s.ok).toBe(false);
  });

  it('a tx that is not version 1 warns', () => {
    const b = tampered('create.bid', (x) => { x.tx.version = 0; });
    expect(decode(b).warnings.map((w) => w.code)).toContain('tx-version');
  });

  it('every documented code has a severity and every blocking code used above is exported', () => {
    expect(new Set(SIGNING_ISSUE_CODES).size).toBe(SIGNING_ISSUE_CODES.length);
    const s = decode(tampered('create.ask', (x) => { x.tx.outputs[3].scriptPublicKey = p2pkSpk(OTHER.pk); }));
    for (const i of [...s.blocking, ...s.warnings, ...s.info]) expect(SIGNING_ISSUE_CODES).toContain(i.code);
    expect(s.blocking[0].severity).toBe('blocking');
    expect(s.blocking[0].message).toMatch(/PAID to another key/);
  });
});

describe('the token an order pins must be the registered token', () => {
  it('a token pin that differs from the registry (template, extension commitment) is blocking; unlisted (delisted / unknown) tokens warn', () => {
    // golden 8x8 order trades the 7070.. covenant as a KCC20Ref_8x8 token, but the registry lists 7070.. as the 3x3 reference program
    const wrongProgram = decode(buildGolden('create.ask.8x8.x402'));
    expect(blockingCodes(wrongProgram)).toContain('order-token-mismatch');
    const reg = (mutate: (d: any) => void) => {
      const doc = tradableRegistryJson();
      mutate(doc);
      return parseRegistry(doc, { kob });
    };
    const otherExt = reg((d) => { d.tokens[0].extension_commitment = 'dd'.repeat(32); });
    expect(blockingCodes(decode(buildGolden('create.ask'), { registry: otherExt }))).toEqual(['order-token-mismatch']);
    expect(blockingCodes(decode(buildGolden('create.bid'), { registry: otherExt }))).toEqual(['order-token-mismatch']);
    // a pending-review, unverified entry on a reviewed template is tradable (open token list): no unlisted warning, no more strict than an open-list token
    const pending = reg((d) => { d.tokens[0].status = 'pending-review'; d.tokens[0].verified = false; });
    const bid = decode(buildGolden('create.bid'), { registry: pending });
    expect(bid.blocking).toEqual([]);
    expect(bid.warnings.map((w) => w.code)).toEqual([]);
    const delisted = reg((d) => { d.tokens[0].status = 'delisted'; });
    expect(decode(buildGolden('create.bid'), { registry: delisted }).warnings.map((w) => w.code)).toEqual(['token-unlisted']); // even though no token moves in a bid
    const unknown = decode(buildGolden('create.ask'), { registry: reg((d) => { d.tokens = [d.tokens[1]]; }) });
    expect(unknown.warnings.map((w) => w.code)).toEqual(['token-unlisted']); // once, not per output
  });

  it('the wallet key is compared case-insensitively', () => {
    expect(decode(buildGolden('create.bid'), { maker: MAKER.pk.toUpperCase() }).blocking).toEqual([]);
  });
});

describe('what the wallet popup will not show', () => {
  it('lists the signed inputs, the covenant inputs it is not asked to sign, and per-wallet notices', () => {
    const cancel = buildGolden('cancel.ask.sweepStray');
    const d = describeInputsForWallet(cancel);
    expect(d.signCount).toBe(1);
    expect(d.inputs.map((i) => [i.kind, i.willSign, i.needsRedeemScript])).toEqual([['covenant', true, true], ['covenant', false, false], ['covenant', false, false]]);
    expect(d.unsignedCovenantInputs).toBe(2);
    expect(d.notices.map((n) => n.code)).toEqual(expect.arrayContaining(['blind-tokens', 'kasware-spend', 'kaspire-covenant', 'kastle-balance', 'kastle-scripts', 'covenant-inputs-unsigned']));
    const kasware = describeInputsForWallet(cancel, 'kasware').notices.map((n) => n.code);
    expect(kasware).toContain('kasware-spend');
    expect(kasware).not.toContain('kaspire-covenant');
    expect(kasware).not.toContain('kastle-balance');
    const create = describeInputsForWallet(buildGolden('create.bid'));
    expect(create.inputs).toEqual([{ index: 0, kind: 'p2pk', willSign: true, needsRedeemScript: false }]);
    expect(create.notices).toEqual([]);
    const refund = describeInputsForWallet(buildGolden('refund.bid'));
    expect(refund.signCount).toBe(0);
    expect(refund.notices.map((n) => n.code)).toContain('no-signature');
  });

  it('p2pkOwner extracts the key of a plain P2PK script only', () => {
    expect(p2pkOwner(p2pkSpk(MAKER.pk))).toBe(MAKER.pk);
    expect(p2pkOwner('0000aa20' + 'ab'.repeat(32) + '87')).toBeNull();
    expect(p2pkOwner('000021' + 'ab'.repeat(33) + 'ab')).toBeNull();
  });
});

describe('the decoder agrees with consensus on real, signed transactions', () => {
  it('signs, finalizes and validates what decodeSigning approved (cancel, replace, position)', () => {
    for (const name of ['create.ask', 'create.ifdAsk.repeat', 'cancel.ask.sweepStray', 'cancelReplace.ask.topUp', 'cancel.position.repeatBuyFirst', 'cancel.bid']) {
      const built = buildGolden(name);
      const s = decode(built);
      expect(s.ok, name).toBe(true);
      const signed = signAndValidate(kob, built);
      // the decoded fee is what the network actually charges
      const inSum = signed.tx.inputs.reduce((a, i) => a + BigInt(i.utxo.amount), 0n);
      const outSum = signed.tx.outputs.reduce((a, o) => a + BigInt(o.value), 0n);
      expect(inSum - outSum, name).toBe(s.fee.sompi);
    }
  });
});
