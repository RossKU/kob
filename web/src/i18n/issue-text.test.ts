import { describe, expect, it } from 'vitest';
import { allKeys, rawEntry, type Params } from './index';
import { issueText, keyedText, signFlowText, walletNoticeText, type IssueLike } from './issue-text';
import { rawOf } from './build-error';
import { ISSUE_CATALOG } from '../kob/orders/common-issues';
import { COND_ISSUE_CATALOG } from '../kob/orders/cond-issues';
import { SIGNING_ISSUE_CODES, decodeSigning, describeInputsForWallet, type SigningIssue } from '../kob/decode';
import { emptyIssueForm, issueLimits, validateIssueForm } from '../kob/issue';
import { planOrder } from '../kob/plan';
import { REGISTRY_ISSUE_CODES, UNTRADABLE_REASONS, lookalikeReport, parseRegistry } from '../kob/registry';
import type { ActionRequest, BuiltTx, Hex } from '../kob/types';
import { formatKas } from '../kob/units';
import { NodeError } from '../data/node-error';
import { SignFlowError } from '../wallet/sign';
import { WalletError } from '../wallet/types';
import { MAKER, OTHER, goldenRequest, tradableRegistryJson, nodeFactsOf } from '../testing/chain-fixtures';
import { KAS, TOK, kob, makeEnv } from '../testing/fixtures';

const leftover = /[{}]/;
const K = kob();
const COV: Hex = '70'.repeat(32);
const issue = (code: string, params?: Params, extra: Partial<IssueLike> = {}): IssueLike => ({ code, message: `English fallback of ${code}`, ...(params ? { params } : {}), ...extra });

describe('issueText: parameter formatting', () => {
  it('formats {x:kas} params as KAS, trimmed', () => {
    const i = issue('INSUFFICIENT_KAS', { needed: 20n * KAS, have: 150_000_000n, shortfall: 1_850_000_000n });
    expect(issueText(i, {})).toBe('Not enough KAS: this order needs 20 KAS, your spendable balance is 1.5 KAS (short by 18.5 KAS).');
  });

  it('formats {x:bps} params as a percentage number', () => {
    expect(issueText(issue('SLIPPAGE_HIGH', { bps: 1250n }), {})).toContain('slippage tolerance of 12.50%');
  });

  it('formats the price params that the catalogue prints plainly (prices per token) as KAS', () => {
    const i = issue('PRICE_NOT_ON_TICK', { tick: 100n, below: 245_000_000n, above: 245_000_100n });
    expect(issueText(i, {})).toBe('The price must be a multiple of the tick (100 sompi per token). The nearest valid prices are 2.45 KAS and 2.450001 KAS.');
    expect(issueText(issue('PRICE_FAR_FROM_MARKET', { percent: '62.5', reference: 250_000_000n }), {})).toContain('62.5% away from the market (2.5 KAS)');
  });

  it('formats token amounts with the token decimals and ticker when known, else as base units', () => {
    const i = issue('INSUFFICIENT_TOKENS', { needed: 12_500n, have: 3_000n, shortfall: 9_500n });
    expect(issueText(i, { tokenDecimals: 3, tokenTicker: 'TST3' })).toBe('Not enough tokens: this order needs 12.5 TST3, you hold 3 TST3 (short by 9.5 TST3).');
    expect(issueText(i, { tokenDecimals: 3 })).toContain('needs 12.5, you hold 3 (short by 9.5)');
    expect(issueText(i, {})).toContain('needs 12500 base units, you hold 3000 base units');
  });

  it('formats the depth amounts of market and fill-or-kill findings in the token decimals', () => {
    expect(issueText(issue('FOK_INSUFFICIENT_DEPTH', { amount: 3_000n, available: 1_500n }), { tokenDecimals: 3, tokenTicker: 'TST3' })).toBe('Fill-or-kill needs 3 TST3 but only 1.5 TST3 cross at your price, so it could not fill.');
    expect(issueText(issue('MARKET_DEPTH_INSUFFICIENT', { amount: 3_000n, available: 1_500n }), { tokenDecimals: 3, tokenTicker: 'TST3' })).toContain('holds only 1.5 TST3 of your 3 TST3');
  });

  it('localises the direction / side / counterparty words the planners pass in English', () => {
    expect(issueText(issue('COND_TP_STOP_ORDER', { direction: 'above', takeProfit: 3n, stop: 2n }), {})).toBe('The take-profit must be above the stop.');
    expect(issueText(issue('COND_STOP_ALREADY_REACHED', { direction: 'at or below' }), {})).toContain('already at or below your stop');
    expect(issueText(issue('NO_LIQUIDITY', { counterparty: 'bids' }), {})).toContain('no bids');
  });

  it('shortens 64-hex ids and outpoints, and stringifies everything else', () => {
    const t = issueText(issue('SELF_TRADE', { ownCovenantId: COV, ownPrice: 250_000_000n }), {});
    expect(t).toContain('7070…7070');
    expect(t).not.toContain(COV);
    expect(issueText(issue('cancel.stray-other-extension', { outpoint: `${COV}:2`, amount: 5n }), { tokenDecimals: 0 })).toContain('7070…7070:2');
    expect(issueText(issue('DAY_ORDER_ENDS_SOON', { minutes: 4 }), {})).toBe('This day order ends at 00:00 UTC, in 4 minutes.');
  });
});

describe('issueText: context merged into the params', () => {
  it('merges issue.input and issue.output as {input} / {output}', () => {
    expect(issueText(issue('input-script-mismatch', undefined, { input: 2 }), {})).toBe('Input 2: the script of the spent UTXO is not the one the plan describes.');
    expect(issueText(issue('output-script-mismatch', undefined, { output: 3 }), {})).toContain('Output 3:');
  });

  it('builds {where} from the index of the finding (order-not-maker can be about an input or an output)', () => {
    expect(issueText(issue('order-not-maker', { maker: OTHER.pk }, { input: 0 }), {})).toMatch(/^Input 0: The order belongs to another key \([0-9a-f]{4}…[0-9a-f]{4}\)/);
    expect(issueText(issue('order-not-maker', { maker: OTHER.pk }), {})).toMatch(/^The order belongs/);
  });

  it('numbers the holder of a form finding from its field (1-based)', () => {
    expect(issueText(issue('holder.owner.invalid', undefined, { field: 'holders[2].owner' }), {})).toBe('Holder 3: the owner is 64 hexadecimal characters.');
  });

  it('adds the registry path', () => {
    expect(issueText(issue('bad-hex', undefined, { path: '$.tokens[1].covenant_id' }), {})).toContain('($.tokens[1].covenant_id)');
  });

  it('turns cancel.insufficient-tokens into a shortfall in both parameter forms', () => {
    const ctx = { tokenDecimals: 3, tokenTicker: 'TST3' };
    expect(issueText(issue('cancel.insufficient-tokens', { need: 20_000n, have: 12_500n }), ctx)).toContain('7.5 TST3 more are needed');
    expect(issueText(issue('cancel.insufficient-tokens', { need: 7_500n }), ctx)).toContain('7.5 TST3 more are needed');
  });

  it('turns the DAA gap of refund.not-yet into a readable wait (10 DAA per second)', () => {
    const at = (due: bigint, now: bigint) => issueText(issue('refund.not-yet', { dueDaa: due, nowDaa: now }), {});
    expect(at(1_012_000n, 1_000_000n)).toContain('DAA 1012000 (now 1000000), in about 20 min');
    expect(at(1_000_000n + 10n * 90_000n, 1_000_000n)).toContain('in about 1 d 1 h');
    expect(at(1_000_100n, 1_000_000n)).toContain('in about less than a minute');
  });

  // C5 R-3: a raw builder text is never shown inline: the sentence carries its plain-words class, the raw text goes behind "Details"
  // (rawOf). This test pinned the raw English inside the sentence.
  it('replaces the raw builder detail of a protocol refusal by a plain sentence', () => {
    expect(issueText(issue('BUILD_REJECTED', { reason: 'amount must be positive' }), {})).toBe('The protocol refused this order: the order terms are not valid (an amount, price or minimum fill is out of range)');
    expect(issueText({ code: 'cancel.build-failed', message: 'token-holding order: custody token UTXO required' }, {})).toContain('token custody of the order was not found');
    expect(rawOf(issue('BUILD_REJECTED', { reason: 'amount must be positive' }))).toBe('amount must be positive');
    expect(rawOf({ code: 'cancel.build-failed', message: 'x' })).toBe('x');
    expect(rawOf({ code: 'cancel.not-maker', message: 'x' })).toBeNull();
  });
});

describe('issueText: fallbacks', () => {
  it('falls back to the English message for a code without a sentence', () => {
    expect(issueText({ code: 'NOT_A_REAL_CODE', message: 'English text' }, {})).toBe('English text');
  });

  it('keyedText returns the fallback, else the key, for an unknown code', () => {
    expect(keyedText('wallet', 'nope', undefined, undefined)).toBe('issues.wallet.nope');
  });

  it('keyedText translates wallet errors, neutral rejection first', () => {
    expect(keyedText('wallet', 'rejected', undefined, undefined)).toBe('You declined the request in your wallet. Nothing was sent.');
    expect(keyedText('wallet', 'other', undefined, 'boom')).toContain('Details: boom');
  });
});

/** Every English placeholder gets a plausible value: catalogue codes are rendered from the params their planners really pass. */
function synthParams(message: string): Params {
  const p: Params = {};
  for (const m of message.matchAll(/\{(\w+)(?::\w+)?\}/g)) {
    const n = m[1];
    p[n] = n === 'direction' ? 'below' : n === 'side' ? 'sell' : n === 'counterparty' ? 'bids' : n === 'percent' ? '12.5' : n === 'type' || n === 'reason' ? 'x' : n === 'ownCovenantId' ? COV : 1_500_000n;
  }
  return p;
}

describe('issueText: every planner catalogue code renders with no placeholder left over', () => {
  for (const [name, catalog] of [['ISSUE_CATALOG', ISSUE_CATALOG], ['COND_ISSUE_CATALOG', COND_ISSUE_CATALOG]] as const) {
    it(name, () => {
      for (const [code, entry] of Object.entries(catalog)) {
        const s = issueText({ code, message: entry.message, params: synthParams(entry.message) }, {});
        expect(s, `${code}`).not.toMatch(leftover);
      }
    });
  }
});

describe('issueText: real plan issues (planOrder with the fixture environment)', () => {
  it('a limit off the tick is PRICE_NOT_ON_TICK, rendered in Japanese without leftovers', () => {
    const plan = planOrder(makeEnv(), { type: 'limit', side: 'buy', price: 245_000_050n, amount: 10n * TOK });
    const i = plan.issues.find((x) => x.code === 'PRICE_NOT_ON_TICK');
    expect(i).toBeDefined();
    expect(issueText(i!, {})).toContain('nearest valid prices are 2.45 KAS and 2.450001 KAS');
  });

  it('too little KAS is INSUFFICIENT_KAS with the exact shortfall', () => {
    const plan = planOrder(makeEnv({ funding: [5n * KAS] }), { type: 'limit', side: 'buy', price: 250_000_000n, amount: 10n * TOK });
    const i = plan.issues.find((x) => x.code === 'INSUFFICIENT_KAS');
    expect(i).toBeDefined();
    const p = i!.params as { needed: bigint; have: bigint; shortfall: bigint };
  });

  it('too few tokens is INSUFFICIENT_TOKENS in the token decimals', () => {
    const plan = planOrder(makeEnv({ tokenAmounts: [2n * TOK] }), { type: 'limit', side: 'sell', price: 250_000_000n, amount: 10n * TOK });
    const i = plan.issues.find((x) => x.code === 'INSUFFICIENT_TOKENS');
    expect(i).toBeDefined();
    expect(issueText(i!, { tokenDecimals: 3, tokenTicker: 'TST3' })).toBe('Not enough tokens: this order needs 10 TST3, you hold 2 TST3 (short by 8 TST3).');
  });

  it('a conditional order with a wrong take-profit / stop order uses the localised direction', () => {
    const plan = planOrder(makeEnv(), { type: 'oco', side: 'sell', amount: TOK, takeProfit: 230_000_000n, stop: 240_000_000n });
    const i = plan.issues.find((x) => x.code === 'COND_TP_STOP_ORDER');
    expect(i).toBeDefined();
    expect(issueText(i!, {})).toBe('The take-profit must be above the stop.');
  });

  it('every issue of several bad plans render with no leftover', () => {
    const plans = [
      planOrder(makeEnv(), { type: 'limit', side: 'sell', price: 0n, amount: 0n }),
      planOrder(makeEnv(), { type: 'limit', side: 'buy', price: 245_000_050n, amount: 10n * TOK, tip: -1n }),
      planOrder(makeEnv(), { type: 'oco', side: 'sell', amount: TOK, takeProfit: 230_000_000n, stop: 240_000_000n }),
      planOrder(makeEnv({ funding: [KAS] }), { type: 'limit', side: 'buy', price: 250_000_000n, amount: 10n * TOK }),
      planOrder(makeEnv(), { type: 'limit', side: 'sell', price: 240_000_000n, amount: TOK, crossing: 'reject' }),
    ];
    let seen = 0;
    for (const plan of plans) {
      for (const i of plan.issues) {
        seen++;
        expect(issueText(i, { tokenDecimals: 3, tokenTicker: 'TST3' }), `${i.code}`).not.toMatch(leftover);
      }
    }
    expect(seen).toBeGreaterThanOrEqual(5);
  });
});

describe('issueText: pre-sign findings of the decoder', () => {
  const clone = <T>(v: T): T => JSON.parse(JSON.stringify(v)) as T;
  const golden = (name: string, maker = MAKER): BuiltTx => K.build(goldenRequest<ActionRequest>(name, maker.pk));
  const tampered = (name: string, f: (b: BuiltTx) => void): BuiltTx => {
    const b = clone(golden(name));
    f(b);
    return b;
  };
  const registry = parseRegistry(tradableRegistryJson(), { kob: K });
  const decode = (built: BuiltTx, withRegistry = true) => decodeSigning({ kob: K, built, maker: MAKER.pk, registry: withRegistry ? registry : null, nodeInputs: nodeFactsOf(built) });
  const allIssues = (s: ReturnType<typeof decode>): SigningIssue[] => [...s.blocking, ...s.warnings, ...s.info];

  it('the findings of decoded transactions render without leftovers', () => {
    const summaries = [
      decode(golden('create.ask', OTHER)), // another key's order: order-not-maker, payment-out, transfer-out, sign-foreign-key
      decode(tampered('create.ask', (x) => { x.tx.outputs[3].value = (BigInt(x.tx.outputs[3].value) - 5n * KAS).toString(); })), // fee-mismatch, fee-excessive
      decode(tampered('create.ask', (x) => { x.tx.outputs[3].value = (BigInt(x.tx.outputs[3].value) + 10_000n * KAS).toString(); })), // kas-unbalanced
      decode(tampered('create.ask', (x) => { x.tx.outputs[3].scriptPublicKey = '0000aa20' + 'cd'.repeat(32) + '87'; })), // output-unknown
      decode(tampered('create.bid', (x) => { x.sign[0].sighashType = 2; })), // sighash-type
      decode(tampered('create.bid', (x) => { x.sign = []; })), // unsigned-maker-input
      decode(tampered('create.ask', (x) => { x.plans.pop(); })), // malformed-built
      decode(golden('create.ask'), false), // token-unlisted
      decode(golden('cancel.ask.sweepStray')), // kas-released, strays-swept
    ];
    const codes = new Set<string>();
    for (const s of summaries) {
      for (const i of allIssues(s)) {
        codes.add(i.code);
        expect(issueText(i, { tokenDecimals: 3, tokenTicker: 'TST3' }), `${i.code}`).not.toMatch(leftover);
      }
    }
    for (const c of ['order-not-maker', 'payment-out', 'transfer-out', 'sign-foreign-key', 'fee-excessive', 'fee-mismatch', 'kas-unbalanced', 'output-unknown', 'sighash-type', 'unsigned-maker-input', 'malformed-built', 'token-unlisted', 'kas-released', 'strays-swept']) {
      expect(codes.has(c), `decoder did not produce ${c}`).toBe(true);
    }
  });

  it('shows amounts, recipients and indexes of a payment to another key', () => {
    const s = decode(tampered('create.ask', (x) => { x.tx.outputs[3].scriptPublicKey = `000020${OTHER.pk}ac`; }));
    const pay = s.blocking.find((b) => b.code === 'payment-out')!;
    const en = issueText(pay, {});
    expect(en).toMatch(/^Output 3: [\d.]+ KAS would be PAID to another key \([0-9a-f]{4}…[0-9a-f]{4}\)\.$/);
  });

  it('renders a hand-made finding for every SIGNING_ISSUE_CODE with realistic params', () => {
    const sample: Record<string, Partial<SigningIssue>> = {
      'malformed-built': {}, 'recover-failed': {}, 'order-spk-mismatch': { output: 1 }, 'order-not-maker': { output: 0, params: { maker: OTHER.pk } },
      'order-unrecorded': { output: 0 }, 'order-token-mismatch': { output: 0, params: { covenantId: COV } }, 'input-script-mismatch': { input: 1 },
      'output-script-mismatch': { output: 2 }, 'token-output-count': {}, 'token-unbalanced': { params: { in: 1000n, out: 900n } }, 'kas-unbalanced': {},
      'fee-mismatch': { params: { derived: 5_000_000n, declared: 4_000_000n } }, 'fee-excessive': { params: { fee: 500_000_000n, moved: 20n * KAS } }, 'fee-rate-excessive': { params: { rate: '1500', max: 1000n } },
      'payment-out': { output: 3, params: { recipient: OTHER.pk, amount: 12n * KAS } }, 'transfer-out': { output: 2, params: { recipient: OTHER.pk, amount: 1500n } },
      'token-to-unknown-owner': { output: 1 }, 'output-unknown': { output: 4 }, 'sign-foreign-key': { input: 0 }, 'sign-mismatch': { input: 0 },
      'sign-unexpected': { input: 5 }, 'sighash-type': { input: 0 }, 'expected-orders': { params: { planned: 1, actual: 2 } },
      'expected-kas-locked': { params: { expected: 10n * KAS, actual: 12n * KAS } }, 'expected-tokens-escrowed': { params: { expected: 1000n, actual: 900n } },
      'expected-cancel-ids': { params: { expected: 1, actual: 2 } }, 'expected-max-fee': { params: { fee: 3_000_000n, max: 2_000_000n } }, 'tx-version': {}, 'input-unconfirmed': { input: 2 }, 'token-state-unplain': { output: 1 }, 'inputs-unverified': {},
      'fee-high': { params: { fee: 5_000_000n, minimum: 1_000_000n } }, 'unsigned-maker-input': { input: 1 }, 'foreign-input': { input: 1 }, 'payload-unknown': {},
      'payload-invalid': {}, 'token-unlisted': { params: { covenantId: COV } }, 'no-order-records': {}, 'payment-expected': { output: 1 }, 'transfer-expected': { output: 1 },
      'strays-swept': { input: 1, params: { amount: 250n } }, 'kas-released': { params: { released: 20n * KAS } },
      'pair-token-mismatch': { output: 0, params: { covenantId: COV, token: 'B' } }, 'pair-family-mismatch': { output: 0, params: { token: 'A' } }, 'pair-terms-invalid': { output: 0 },
      'expected-quote-escrowed': { params: { expected: 1000n, actual: 900n } },
      'trigger-evidence-invalid': { input: 0, params: { evidence: 2 } }, 'trigger-rule-unmet': { input: 0, params: { evidence: 1 } }, 'trigger-evidence': { input: 0, params: { evidence: 1 } },
      'other-strays-swept': { input: 1, params: { amount: '2.5 TSTB', token: 'TSTB (7171…7171)' } },
      'sweep-invalid': { output: 0, input: 0 }, 'expected-sweep-ids': { params: { expected: 1, actual: 0 } }, 'sweep-custody-spent': { input: 1 },
      'swept-in-place': { input: 0, output: 0, params: { utxos: 3 } },
    };
    expect(Object.keys(sample).sort()).toEqual([...SIGNING_ISSUE_CODES].sort());
    for (const code of SIGNING_ISSUE_CODES) {
      const i: SigningIssue = { code, severity: 'blocking', message: `English ${code}`, ...sample[code] };
      const s = issueText(i, { tokenDecimals: 3, tokenTicker: 'TST3' });
      expect(s, `${code}`).not.toMatch(leftover);
      expect(s, `${code}`).not.toBe(i.message);
    }
  });
});

describe('cancel, refund, snapshot, issuance, registry and wallet findings', () => {
  it('renders a realistic finding for every cancel / refund code', () => {
    const cases: [string, Params | undefined][] = [
      ['cancel.build-failed', undefined], ['cancel.custody-missing', { covenantId: COV }], ['cancel.funding-added', { inputs: 1 }],
      ['cancel.insufficient-funds', { shortfall: 100_000n }], ['cancel.insufficient-tokens', { need: 5_000n }], ['cancel.not-maker', { covenantId: COV }],
      ['cancel.nothing', undefined], ['cancel.replacement-restarts-unarmed', undefined], ['cancel.replacement-wrong-maker', undefined],
      ['cancel.replacement-wrong-token', undefined], ['cancel.stray-other-extension', { outpoint: `${COV}:1`, amount: 100n }],
      ['cancel.strays-abandoned', { count: 2, amount: 300n }], ['cancel.strays-exceed-slots', { strays: 5, room: 2 }], ['cancel.top-up', { amount: 4_000n }],
      ['refund.no-clock', undefined], ['refund.not-yet', { dueDaa: 2_000_000n, nowDaa: 1_000_000n }], ['refund.strays-stay', { count: 2 }],
      ['sweep.in-place', undefined], ['sweep.later-extension', { count: 1, amount: 5n }], ['sweep.later-slots', { count: 2, amount: 7n }], ['sweep.needs-funding', undefined],
      ['sweep.nothing', undefined], ['sweep.stray-unproven', { outpoint: `${COV}:2`, amount: 9n }], ['sweep.strays-unknown', undefined],
      ['snapshot.state-unknown', undefined], ['snapshot.no-current-utxo', undefined], ['snapshot.no-extension-commitment', undefined],
      ['snapshot.bad-state', undefined], ['snapshot.not-live', undefined],
    ];
    for (const [code, params] of cases) {
      const s = issueText(issue(code, params), { tokenDecimals: 3, tokenTicker: 'TST3' });
      expect(s, `${code}`).not.toMatch(leftover);
      expect(s, `${code}`).not.toBe(`English fallback of ${code}`);
    }
    expect(issueText(issue('cancel.strays-exceed-slots', { strays: 5, room: 2 }), {})).toContain('5 stray token UTXOs, but one transaction can move only 2');
  });

  it('renders the findings of a real issuance form validation (holders numbered, amounts in base units)', () => {
    const limits = issueLimits(K);
    const form = {
      ...emptyIssueForm(), name: 'Test', ticker: 'TESTX', decimals: 2, supply: '100',
      website: 'http://example.com', icon: 'ftp://x',
      holders: [{ owner: 'zz', ownerScheme: 0, amount: '60' }, { owner: OTHER.pk, ownerScheme: 0, amount: '30' }, { owner: MAKER.pk, ownerScheme: 0, amount: '0' }],
    };
    const issues = validateIssueForm(form, limits, ['TEST0']);
    expect(issues.map((i) => i.code)).toEqual(expect.arrayContaining(['holder.owner.invalid', 'holder.amount.zero', 'website.https', 'icon.scheme']));
    for (const i of issues) {
      expect(issueText(i, { tokenDecimals: 2 }), `${i.code}`).not.toMatch(leftover);
    }
    const bad = issues.find((i) => i.code === 'holder.owner.invalid')!;

    const sum = validateIssueForm({ ...emptyIssueForm(), name: 'Test', ticker: 'TESTX', decimals: 2, supply: '100', holders: [{ owner: MAKER.pk, ownerScheme: 0, amount: '60' }] }, limits, []).find((i) => i.code === 'holders.sum_mismatch');
    expect(sum).toBeDefined();
    expect(issueText(sum!, { tokenDecimals: 2 })).toBe('The holders receive 60 in total, but the supply is 100.');
    const lookalike = validateIssueForm({ ...emptyIssueForm(), name: 'Test', ticker: 'TEST1', decimals: 2, supply: '1' }, limits, ['TESTL']).find((i) => i.code === 'ticker.lookalike');
  });

  it('renders every issue form, registry and verification code with the params they carry', () => {
    const cases: [string, Params | undefined][] = [
      ['name.too_long', { max: 64 }], ['ticker.length', { min: 2, max: 12 }], ['ticker.exists', { ticker: 'KRON' }], ['decimals.invalid', { max: 18 }],
      ['supply.precision', { decimals: 8 }], ['supply.too_large', { max: 1_000_000n }], ['holders.too_many', { max: 30 }],
      ['holders.consolidation', { holders: 12, maxInputs: 8 }], ['holders.sum_mismatch', { holders: 500n, supply: 1000n }],
      ['holder.amount.precision', { decimals: 8 }], ['description.invalid', { max: 512 }], ['website.invalid', { max: 512 }], ['icon.invalid', { max: 512 }],
      ['funds.insufficient', { need: 30n * KAS, have: 12n * KAS }], ['maker.invalid', undefined], ['carrier.zero', undefined],
    ];
    for (const [code, params] of cases) {
      const s = issueText(issue(code, params, { field: 'holders[1].amount' }), { tokenDecimals: 8 });
      expect(s, `${code}`).not.toMatch(leftover);
      expect(s, `${code}`).not.toBe(`English fallback of ${code}`);
    }
    for (const code of REGISTRY_ISSUE_CODES) {
      const s = issueText(issue(code, undefined, { path: '$.tokens[0]' }), {});
      expect(s, `${code}`).not.toMatch(leftover);
      expect(s, `${code}`).toContain('$.tokens[0]');
    }
    expect(issueText(issue('funds.insufficient', { need: 30n * KAS, have: 12n * KAS }), {})).toContain('locks 30 KAS as carriers');
  });

  it('gives every untradable reason and lookalike level a sentence', () => {
    for (const r of UNTRADABLE_REASONS) expect(keyedText('untradable', r, undefined, undefined), `${r}`).not.toMatch(/^issues\./);
    const registry = parseRegistry(tradableRegistryJson(), { kob: K });
    const real = registry.tokens[0];
    const strong = lookalikeReport(registry, real.ticker, '99'.repeat(32));
    expect(strong.level).toBe('strong');
    const en = keyedText('lookalike', strong.level, { ticker: real.ticker, real: `${real.ticker} (7070…7070)`, id: '9999…9999' }, strong.message);
    expect(en).toContain('WARNING');
    expect(en).toContain('is NOT');
    expect(keyedText('lookalike', 'none', { name: 'TST (7070…7070) [verified]' }, undefined)).toBe('TST (7070…7070) [verified] is a registered token.');
  });
});

describe('wallet, sign flow and node errors', () => {
  it('translates every WalletError code; a wallet rejection is neutral', () => {
    for (const code of ['rejected', 'timeout', 'no-signature', 'unsupported', 'network', 'other'] as const) {
      const e = new WalletError(code, 'English detail');
      const s = keyedText('wallet', e.code, undefined, e.message);
      expect(s, `${code}`).not.toMatch(leftover);
      expect(s).not.toBe('English detail');
    }
    expect(keyedText('wallet', 'rejected', undefined, undefined)).not.toMatch(/error|fail/i);
  });

  it('translates SignFlowError by stage: wallet codes, signature / validation, node codes at the broadcast stage', () => {
    const rejected = new SignFlowError('signing', new WalletError('rejected', 'You rejected the request in KasWare.'), 'You rejected the request in KasWare.', 'rejected');
    expect(rejected.rejectedByUser).toBe(true);
    expect(signFlowText(rejected)).toBe('You declined the request in your wallet. Nothing was sent.');

    const sig = new SignFlowError('finalizing', new Error('bad'), 'The wallet\'s signature does not match this transaction, nothing was sent. (bad)', 'signature');
    expect(signFlowText(sig)).toContain('signature does not match');
    const val = new SignFlowError('validating', new Error('bad'), 'The transaction failed the script check and was not sent. (bad)', 'validation');

    const nodeErr = new NodeError('script', 'A script or covenant rule failed: bad signature', 'RPC Server (remote error) -> code:0  message:`Rejected transaction abc: signature verification failed` data:None');
    const submit = new SignFlowError('submitting', nodeErr, nodeErr.message, nodeErr.code);
    expect(signFlowText(submit)).toBe('A script or covenant rule failed on the node, so the transaction was not accepted. Details: signature verification failed');

    const orphan = new SignFlowError('submitting', new NodeError('orphan', 'x'), 'x', 'orphan');
    // an unknown code falls back to the English message
  });

  it('translates every node error code and every wallet-popup notice of describeInputsForWallet', () => {
    for (const code of ['unavailable', 'network-mismatch', 'orphan', 'double-spend', 'already-known', 'fee', 'script', 'invalid', 'bad-response', 'other']) {
      expect(keyedText('node', code, { reason: 'why' }, undefined), `${code}`).not.toMatch(leftover);
    }
    const seen = new Set<string>();
    for (const name of ['cancel.ask', 'create.ask', 'refund.bid']) {
      const built = K.build(goldenRequest<ActionRequest>(name, MAKER.pk));
      for (const wallet of [undefined, 'kasware', 'kaspire', 'kastle'] as const) {
        for (const n of describeInputsForWallet(built, wallet).notices) {
          seen.add(n.code);
          const s = walletNoticeText(n);
          expect(s, `${n.code}`).not.toMatch(leftover);
          expect(s, `${n.code}`).not.toBe(n.message);
        }
      }
    }
    // the cancel / refund transactions of the golden vectors reach the generic, per-wallet and no-signature notices
    for (const c of ['blind-tokens', 'kasware-spend', 'kaspire-covenant', 'kastle-balance', 'covenant-inputs-unsigned', 'no-signature']) expect(seen.has(c), c).toBe(true);
    expect(walletNoticeText({ code: 'kastle-scripts', message: 'x', params: { inputs: 2 } })).toContain('(2 here)');
  });
});

describe('dictionary sanity for the helper keys', () => {
  it('has the helper keys', () => {
    const helpers = allKeys().filter((k) => /^issues\.(fmt|where|term|dur)\./.test(k));
    expect(helpers.length).toBeGreaterThanOrEqual(15);
    for (const k of helpers) expect(rawEntry(k), k).toBeTruthy();
  });
});
