// Issuance: form validation, planning over kob-wasm, and the strongest oracle available: the planned genesis, signed locally and
// finalized with tightened budgets, passes kob-wasm `validate` (the script engine runs the covenants and checks the storage-mass
// commitment and fee floor).
import { describe, expect, it } from 'vitest';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { loadKobNode } from './wasm.node';
import { pubkeyOf, signBuilt } from '../testing/local-signer';
import {
  emptyIssueForm, formatUnits, IssueFormError, issueLimits, issuedTokenUtxos, normalizeTicker, parseDecimal, planIssue,
  registryEntryFromIssue, validateIssueForm,
  type IssueForm, type IssueLimits, type IssuePlan, type PlanIssueOptions,
} from './issue';
import type { KeyUtxo } from './types';

const kob = loadKobNode();
const limits: IssueLimits = issueLimits(kob);
const KAS = 100_000_000n;
const SK = '33'.repeat(32);
const maker = pubkeyOf(SK);
const otherKey = pubkeyOf('44'.repeat(32));

function utxo(index: number, sompi: bigint, pubkey = maker): KeyUtxo {
  return { transactionId: '07'.repeat(32), index, amount: sompi.toString(), blockDaaScore: '5000', pubkey };
}
function form(over: Partial<IssueForm> = {}): IssueForm {
  return { ...emptyIssueForm(), name: 'Test Token', ticker: 'TEST', decimals: 8, supply: '1000', ...over };
}
function opts(over: Partial<PlanIssueOptions> = {}): PlanIssueOptions {
  return { funding: [utxo(1, 500n * KAS)], maker, network: 'testnet-10', ...over };
}
const codes = (f: IssueForm, tickers?: string[]) => validateIssueForm(f, limits, tickers).map((i) => i.code);

/** plan -> local signatures -> finalize(tightenBudgets) -> validate */
function signAndValidate(plan: IssuePlan) {
  const sigs = signBuilt(plan.built, [SK]);
  const signed = kob.finalize(plan.built, sigs, { tightenBudgets: true });
  const report = kob.validate(signed) as { fee: string; minFee: string };
  expect(BigInt(report.fee)).toBeGreaterThanOrEqual(BigInt(report.minFee));
  return signed;
}

describe('limits', () => {
  it('come from kob-wasm and match the protocol rules', () => {
    expect(limits.maxSupply).toBe('2900000000000000000');
    expect(limits.maxGenesisOutputs).toBe(64);
    expect(limits.defaultCarrier).toBe((10n * KAS).toString());
    expect([limits.maxTokenInputs, limits.maxTokenOutputs, limits.maxDecimals]).toEqual([8, 8, 18]);
    expect(limits.program).toBe('KCC20Ref_8x8');
    expect(issueLimits(kob)).toBe(limits);
  });
});

describe('amount helpers', () => {
  it('parses decimal text exactly with bigint', () => {
    expect(parseDecimal('1000', 8)).toEqual({ value: 100_000_000_000n });
    expect(parseDecimal('0.00000001', 8)).toEqual({ value: 1n });
    expect(parseDecimal('12.5', 2)).toEqual({ value: 1250n });
    expect(parseDecimal('1.500', 2)).toEqual({ value: 150n });
    expect(parseDecimal('2900000000.000000000', 8)).toEqual({ value: 290_000_000_000_000_000n });
    expect(parseDecimal('1.005', 2)).toEqual({ error: 'precision' });
    for (const bad of ['', '.5', '1.', '-1', '1e3', '1,000', '0x10', ' ']) expect(parseDecimal(bad, 8), bad).toEqual({ error: 'syntax' });
    // beyond 2^53: exact
    expect(parseDecimal('9007199254740993', 0)).toEqual({ value: 9007199254740993n });
  });

  it('formats base units', () => {
    expect(formatUnits(1250n, 2)).toBe('12.5');
    expect(formatUnits(1200n, 2)).toBe('12');
    expect(formatUnits(5n, 8)).toBe('0.00000005');
    expect(formatUnits(7n, 0)).toBe('7');
    expect(formatUnits(0n, 3)).toBe('0');
  });

  it('normalises tickers like the registry', () => {
    expect(normalizeTicker('KR0N')).toBe('KR0N');
    expect(normalizeTicker('LIO')).toBe(normalizeTicker('110'));
    expect(normalizeTicker('kron')).toBe('KR0N');
  });
});

describe('validateIssueForm', () => {
  it('accepts a normal form', () => {
    expect(validateIssueForm(form(), limits)).toEqual([]);
    expect(codes(form({ ticker: 'AB' }))).toEqual([]);
    expect(codes(form({ ticker: 'ABCDEFGHIJ12', decimals: 0 }))).toEqual([]);
  });

  it('checks name and ticker', () => {
    expect(codes(form({ name: '  ' }))).toEqual(['name.empty']);
    expect(codes(form({ name: 'x'.repeat(65) }))).toEqual(['name.too_long']);
    expect(codes(form({ name: 'x'.repeat(64) }))).toEqual([]);
    expect(codes(form({ name: 'a‮b' }))).toEqual(['name.bad_chars']);
    expect(codes(form({ name: 'a​b' }))).toEqual(['name.bad_chars']);
    for (const t of ['', 'T', 'ABCDEFGHIJKLM']) expect(codes(form({ ticker: t })), t).toEqual(['ticker.length']);
    for (const t of ['test', 'TE ST', 'T-ST', 'TÉST']) expect(codes(form({ ticker: t })), t).toEqual(['ticker.chars']);
  });

  it('flags existing and lookalike tickers', () => {
    expect(codes(form({ ticker: 'TEST' }), ['TEST', 'OTHER'])).toEqual(['ticker.exists']);
    expect(codes(form({ ticker: 'KR0N' }), ['KRON'])).toEqual(['ticker.lookalike']);
    expect(codes(form({ ticker: 'LI0N' }), ['110N'])).toEqual(['ticker.lookalike']);
    expect(codes(form({ ticker: 'TEST' }), ['TEXT', 'ZZZZ'])).toEqual([]);
    const i = validateIssueForm(form({ ticker: 'KR0N' }), limits, ['KRON'])[0];
    expect(i.severity).toBe('error');
    expect(i.field).toBe('ticker');
    expect(i.params).toEqual({ ticker: 'KR0N', existing: 'KRON' });
  });

  it('checks decimals and supply bounds', () => {
    expect(codes(form({ decimals: 19 }))).toEqual(['decimals.invalid']);
    expect(codes(form({ decimals: -1 }))).toEqual(['decimals.invalid']);
    expect(codes(form({ decimals: 1.5 }))).toEqual(['decimals.invalid']);
    expect(codes(form({ decimals: 18, supply: '1' }))).toEqual([]);
    expect(codes(form({ supply: '' }))).toEqual(['supply.invalid']);
    expect(codes(form({ supply: 'abc' }))).toEqual(['supply.invalid']);
    expect(codes(form({ supply: '0' }))).toEqual(['supply.zero']);
    expect(codes(form({ supply: '0.00000000' }))).toEqual(['supply.zero']);
    expect(codes(form({ supply: '1.123456789', decimals: 8 }))).toEqual(['supply.precision']);
    // MAX_SUPPLY = 2.9e18 base units: exactly the maximum is fine, one base unit above is not
    expect(codes(form({ decimals: 0, supply: '2900000000000000000' }))).toEqual([]);
    expect(codes(form({ decimals: 0, supply: '2900000000000000001' }))).toEqual(['supply.too_large']);
    expect(codes(form({ decimals: 8, supply: '29000000000' }))).toEqual([]);
    expect(codes(form({ decimals: 8, supply: '29000000000.00000001' }))).toEqual(['supply.too_large']);
    expect(codes(form({ decimals: 18, supply: '2.9' }))).toEqual([]);
    expect(codes(form({ decimals: 18, supply: '2.900000000000000001' }))).toEqual(['supply.too_large']);
  });

  it('checks holders: sums, owners, schemes, amounts', () => {
    const h = (owner: string, ownerScheme: number, amount: string) => ({ owner, ownerScheme, amount });
    expect(codes(form({ holders: [h(maker, 0, '600'), h('c4'.repeat(32), 4, '400')] }))).toEqual([]);
    expect(codes(form({ holders: [h(maker, 0, '600'), h('c4'.repeat(32), 4, '399')] }))).toEqual(['holders.sum_mismatch']);
    expect(codes(form({ holders: [h(maker, 0, '1001')] }))).toEqual(['holders.sum_mismatch']);
    expect(codes(form({ holders: [h('abcd', 0, '1000')] }))).toEqual(['holder.owner.invalid']);
    expect(codes(form({ holders: [h('zz'.repeat(32), 1, '1000')] }))).toEqual(['holder.owner.invalid']);
    expect(codes(form({ holders: [h(maker, 9, '1000')] }))).toEqual(['holder.scheme.invalid']);
    expect(codes(form({ holders: [h(maker, 0, '0'), h(maker, 0, '1000')] }))).toEqual(['holder.amount.zero']);
    expect(codes(form({ holders: [h(maker, 0, 'x')] }))).toEqual(['holder.amount.invalid']);
    expect(codes(form({ holders: [h(maker, 0, '1000.000000001')] }))).toEqual(['holder.amount.precision']);
    // scheme 0 needs a real x-only key (an off-curve value would burn the tokens); hashes and covenant ids are opaque
    const offCurve = '00'.repeat(31) + '05';
    expect(codes(form({ holders: [h(offCurve, 0, '1000')] }))).toEqual(['holder.owner.not_a_key']);
    expect(codes(form({ holders: [h(offCurve, 1, '1000')] }))).toEqual([]);
    // sum arithmetic is exact past 2^53
    const big = form({ decimals: 0, supply: '2900000000000000000', holders: [h(maker, 0, '2900000000000000000')] });
    expect(codes(big)).toEqual([]);
    const off = form({ decimals: 0, supply: '2900000000000000000', holders: [h(maker, 0, '2899999999999999999'), h(maker, 0, '2')] });
    expect(codes(off)).toEqual(['holders.sum_mismatch']);
  });

  it('applies the borrow rules', () => {
    const cov = { owner: 'c4'.repeat(32), ownerScheme: 4, amount: '1000', borrowScheme: 1, borrowGuard: '01'.repeat(32) };
    expect(codes(form({ holders: [cov] }))).toEqual(['holder.borrow.covenant']);
    expect(codes(form({ holders: [cov], allowBorrow: true }))).toEqual(['holder.borrow.covenant']);
    const p2pk = { owner: maker, ownerScheme: 0, amount: '1000', borrowScheme: 1, borrowGuard: '01'.repeat(32) };
    expect(codes(form({ holders: [p2pk] }))).toEqual(['holder.borrow.flag']);
    expect(codes(form({ holders: [p2pk], allowBorrow: true }))).toEqual([]);
    expect(codes(form({ holders: [{ ...p2pk, borrowScheme: 0 }], allowBorrow: true }))).toEqual(['holder.borrow.guard']);
    expect(codes(form({ holders: [{ ...p2pk, borrowScheme: 7 }], allowBorrow: true }))).toEqual(['holder.borrow.scheme']);
  });

  it('warns about many holders and refuses more than the genesis maximum', () => {
    const many = (n: number) =>
      form({ supply: String(n), holders: Array.from({ length: n }, (_, i) => ({ owner: (i + 1).toString(16).padStart(2, '0').repeat(32), ownerScheme: 1, amount: '1' })) });
    expect(validateIssueForm(many(8), limits)).toEqual([]);
    const nine = validateIssueForm(many(9), limits);
    expect(nine.map((i) => [i.code, i.severity])).toEqual([['holders.consolidation', 'warning']]);
    expect(codes(many(64))).toEqual(['holders.consolidation']);
    expect(codes(many(65))).toEqual(['holders.too_many']);
  });

  it('checks carrier and display fields', () => {
    expect(codes(form({ carrier: '' }))).toEqual([]);
    expect(codes(form({ carrier: '10' }))).toEqual([]);
    expect(codes(form({ carrier: '0.5' }))).toEqual([]);
    expect(codes(form({ carrier: '0' }))).toEqual(['carrier.zero']);
    expect(codes(form({ carrier: 'ten' }))).toEqual(['carrier.invalid']);
    expect(codes(form({ carrier: '1.000000001' }))).toEqual(['carrier.precision']);
    expect(codes(form({ website: 'http://example.org' }))).toEqual(['website.https']);
    expect(codes(form({ website: 'https://example.org' }))).toEqual([]);
    expect(codes(form({ icon: 'data:image/png;base64,AA' }))).toEqual(['icon.scheme']);
    expect(codes(form({ icon: 'ipfs://bafy' }))).toEqual([]);
    expect(codes(form({ icon: 'https://example.org/i.png' }))).toEqual([]);
    expect(codes(form({ description: 'd'.repeat(513) }))).toEqual(['description.invalid']);
    expect(codes(form({ description: 'd'.repeat(512) }))).toEqual([]);
    expect(codes(form({ description: 'a\nb' }))).toEqual(['description.invalid']);
  });

  it('reports every problem at once with the field of each', () => {
    const issues = validateIssueForm(form({ name: '', ticker: 'x', supply: '0' }), limits);
    expect(issues.map((i) => [i.code, i.field])).toEqual([['name.empty', 'name'], ['ticker.length', 'ticker'], ['supply.zero', 'supply']]);
    expect(issues.every((i) => i.severity === 'error' && i.message.length > 0)).toBe(true);
  });
});

describe('planIssue', () => {
  it('plans, signs, finalizes and validates a default issuance (everything to the maker)', () => {
    const plan = planIssue(kob, form({ description: 'A test token', website: 'https://example.org' }), opts());
    const { built, token } = plan;
    expect(token).toMatchObject({ program: 'KCC20Ref_8x8', ticker: 'TEST', name: 'Test Token', decimals: 8, supply: '100000000000' });
    expect(token.carrier).toBe((10n * KAS).toString());
    expect(token.outputs).toHaveLength(1);
    expect(token.outputs[0]).toMatchObject({ index: 0, amount: '100000000000', owner: maker, ownerScheme: 0, borrowScheme: 0 });
    // BuiltTx contract: one P2PK plan and one SIGHASH_ALL request per funding input, change after the token output
    expect(built.plans).toEqual([{ kind: 'p2pk', pubkey: maker }]);
    expect(built.sign).toHaveLength(1);
    expect(built.sign[0]).toMatchObject({ inputIndex: 0, pubkey: maker, sighashType: 1, redeemScript: null });
    expect(built.tx.outputs).toHaveLength(2);
    expect(built.fee.changeOutput).toBe(1);
    expect(plan.fee).toBe(BigInt(built.fee.fee));
    // the covenant id of the token is the one of the genesis group and of the output binding
    expect(built.covenants).toHaveLength(1);
    expect(built.covenants[0].covenantId).toBe(token.covenantId);
    expect(built.covenants[0].outputs).toEqual([0]);
    expect(built.tx.outputs[0].covenant).toEqual({ authorizingInput: 0, covenantId: token.covenantId });
    expect(built.tx.outputs[1].covenant).toBeNull();
    expect(built.tx.inputs[0].utxo.blockDaaScore).toBe('5000');
    // signing pipeline
    const signed = signAndValidate(plan);
    expect(signed.tx.id).toBe(built.tx.id);
    expect(signed.tx.outputs).toEqual(built.tx.outputs);
  });

  it('token UTXOs decode and re-derive the output script public keys', () => {
    const plan = planIssue(kob, form(), opts());
    const utxos = issuedTokenUtxos(plan);
    expect(utxos).toHaveLength(1);
    for (const u of utxos) {
      const out = plan.built.tx.outputs[u.index];
      expect(u.transactionId).toBe(plan.built.tx.id);
      expect(u.covenantId).toBe(plan.token.covenantId);
      expect(u.amount).toBe(out.value);
      const hex = kob.encodeTokenState(u.state);
      expect(kob.decodeTokenState(hex)).toEqual(u.state);
      expect(kob.tokenScriptPublicKey('KCC20Ref_8x8', u.state)).toBe(out.scriptPublicKey);
    }
    // the template hash of the token info is the one the program is pinned to
    expect(kob.templates().find((t) => t.name === 'KCC20Ref_8x8')!.hash).toBe(plan.token.templateHash);
  });

  it('issues to several holders, including a covenant-held one with borrowing disabled', () => {
    const covOwner = 'c4'.repeat(32);
    const f = form({
      supply: '1000.5',
      decimals: 2,
      holders: [
        { owner: maker, ownerScheme: 0, amount: '600.25' },
        { owner: covOwner, ownerScheme: 4, amount: '300.25' },
        { owner: otherKey, ownerScheme: 0, amount: '100' },
      ],
    });
    const plan = planIssue(kob, f, opts());
    expect(plan.token.supply).toBe('100050');
    expect(plan.token.outputs.map((o) => o.amount)).toEqual(['60025', '30025', '10000']);
    expect(plan.token.outputs.reduce((s, o) => s + BigInt(o.amount), 0n)).toBe(100050n);
    expect(plan.token.outputs[1]).toMatchObject({ owner: covOwner, ownerScheme: 4, borrowScheme: 0, borrowGuard: '00'.repeat(32) });
    expect(plan.built.covenants[0].outputs).toEqual([0, 1, 2]);
    for (const u of issuedTokenUtxos(plan)) {
      expect(kob.tokenScriptPublicKey('KCC20Ref_8x8', u.state)).toBe(plan.built.tx.outputs[u.index].scriptPublicKey);
      expect(plan.built.tx.outputs[u.index].covenant!.covenantId).toBe(plan.token.covenantId);
    }
    signAndValidate(plan);
  });

  it('honours carrier, fee rate, change key and extension defaults', () => {
    const plan = planIssue(kob, form({ carrier: '2.5' }), opts({ feeRate: 200n, changeTo: otherKey }));
    expect(plan.built.tx.outputs[0].value).toBe((250_000_000n).toString());
    expect(plan.built.fee.feeRate).toBe('200');
    expect(BigInt(plan.built.fee.minFee)).toBe(BigInt(plan.built.fee.mass.feeMass) * 200n);
    expect(plan.built.tx.outputs[1].scriptPublicKey).toBe(`0000${'20' + otherKey + 'ac'}`);
    expect(plan.token.extensionCommitment).toBe('00'.repeat(32));
    signAndValidate(plan);
  });

  it('warns when the genesis has more outputs than a transfer can consolidate', () => {
    const f = form({
      supply: '900',
      decimals: 0,
      holders: Array.from({ length: 9 }, (_, i) => ({ owner: (i + 1).toString(16).padStart(2, '0').repeat(32), ownerScheme: 1, amount: '100' })),
    });
    const plan = planIssue(kob, f, opts({ funding: [utxo(0, 2000n * KAS)] }));
    expect(plan.warnings.some((w) => w.includes('9 genesis outputs'))).toBe(true);
    signAndValidate(plan);
  });

  it('selects the fewest funding UTXOs, largest first, only the maker\'s plain P2PK ones', () => {
    // one 200 KAS UTXO covers the 10 KAS carrier: the small ones stay untouched
    const funding = [utxo(1, 6n * KAS), utxo(2, 200n * KAS), utxo(3, 30n * KAS), utxo(4, 500n * KAS, otherKey)];
    const plan = planIssue(kob, form(), opts({ funding }));
    expect(plan.built.tx.inputs.map((i) => i.index)).toEqual([2]);
    // 4 UTXOs of 6 KAS: two are needed for the 10 KAS carrier plus fee
    const small = [1, 2, 3, 4].map((i) => utxo(i, 6n * KAS));
    const plan2 = planIssue(kob, form(), opts({ funding: small }));
    expect(plan2.built.tx.inputs).toHaveLength(2);
    expect(plan2.built.sign.map((s) => s.inputIndex)).toEqual([0, 1]);
    signAndValidate(plan2);
    // a UTXO carrying a covenant id is not plain P2PK
    const cov = { ...utxo(9, 500n * KAS), covenantId: '11'.repeat(32) };
    expect(() => planIssue(kob, form(), opts({ funding: [cov] }))).toThrow(IssueFormError);
  });

  it('uses another UTXO when the first selection is a little short for the fee', () => {
    // exactly the carrier in the largest UTXO: the fee needs the second one
    const plan = planIssue(kob, form(), opts({ funding: [utxo(1, 10n * KAS), utxo(2, KAS)] }));
    expect(plan.built.tx.inputs).toHaveLength(2);
    signAndValidate(plan);
  });

  it('reports insufficient funds as a form error', () => {
    const shortfall = (funding: KeyUtxo[]) => {
      try {
        planIssue(kob, form(), opts({ funding }));
      } catch (e) {
        expect(e).toBeInstanceOf(IssueFormError);
        return (e as IssueFormError).issues;
      }
      throw new Error('expected a failure');
    };
    for (const funding of [[], [utxo(1, 5n * KAS)], [utxo(1, 10n * KAS)], [utxo(1, 10n * KAS + 1000n)], [utxo(1, 500n * KAS, otherKey)]]) {
      const i = shortfall(funding);
      expect(i).toHaveLength(1);
      expect(i[0].code).toBe('funds.insufficient');
    }
  });

  it('refuses invalid forms before touching the wasm', () => {
    try {
      planIssue(kob, form({ ticker: 'bad', supply: '0' }), opts());
      throw new Error('expected a failure');
    } catch (e) {
      expect(e).toBeInstanceOf(IssueFormError);
      expect((e as IssueFormError).issues.map((i) => i.code)).toEqual(['ticker.chars', 'supply.zero']);
    }
    expect(() => planIssue(kob, form({ ticker: 'KR0N' }), opts({ registryTickers: ['KRON'] }))).toThrow(/looks like/);
  });

  it('a mismatching signature is refused by finalize', () => {
    const plan = planIssue(kob, form(), opts());
    const wrong = signBuilt(plan.built, [SK]).map((s) => ({ ...s, signature: s.signature.replace(/^../, (b) => (b === '00' ? '01' : '00')) }));
    expect(() => kob.finalize(plan.built, wrong)).toThrow();
  });
});

describe('registry entry', () => {
  // structural check against registry/tokens.schema.json ($defs.token and $defs.display)
  const schema = JSON.parse(readFileSync(fileURLToPath(new URL('../../../registry/tokens.schema.json', import.meta.url)), 'utf8'));
  type Prop = { type?: string | string[]; enum?: unknown[]; const?: unknown; pattern?: string; minimum?: number; maximum?: number; minLength?: number; maxLength?: number; oneOf?: Prop[]; $ref?: string;
    properties?: Record<string, Prop>; required?: string[]; additionalProperties?: boolean };
  const resolve = (p: Prop): Prop => (p.$ref ? { ...resolve(schema.$defs[p.$ref.split('/').pop()!]), ...p, $ref: undefined } : p);
  function check(value: unknown, raw: Prop, path: string): void {
    const p = resolve(raw);
    if (p.oneOf) {
      expect(p.oneOf.some((alt) => { try { check(value, alt, path); return true; } catch { return false; } }), path).toBe(true);
      return;
    }
    if (p.const !== undefined) expect(value, path).toBe(p.const);
    if (p.enum) expect(p.enum, path).toContain(value);
    const types = p.type === undefined ? [] : Array.isArray(p.type) ? p.type : [p.type];
    if (types.length) {
      const actual = value === null ? 'null' : Number.isInteger(value) ? 'integer' : typeof value;
      expect(types.includes(actual) || (actual === 'integer' && types.includes('number')), `${path}: ${actual} not in ${types}`).toBe(true);
    }
    if (typeof value === 'string') {
      if (p.pattern) expect(new RegExp(p.pattern).test(value), `${path} matches ${p.pattern}`).toBe(true);
      if (p.minLength !== undefined) expect(value.length, path).toBeGreaterThanOrEqual(p.minLength);
      if (p.maxLength !== undefined) expect(value.length, path).toBeLessThanOrEqual(p.maxLength);
    }
    if (typeof value === 'number') {
      if (p.minimum !== undefined) expect(value, path).toBeGreaterThanOrEqual(p.minimum);
      if (p.maximum !== undefined) expect(value, path).toBeLessThanOrEqual(p.maximum);
    }
    const props = p.properties;
    if (props && value && typeof value === 'object') {
      const obj = value as Record<string, unknown>;
      for (const k of p.required ?? []) expect(obj, `${path}.${k} required`).toHaveProperty(k);
      for (const [k, v] of Object.entries(obj)) {
        if (p.additionalProperties === false) expect(Object.keys(props), `${path}.${k} allowed`).toContain(k);
        if (props[k]) check(v, props[k], `${path}.${k}`);
      }
    }
  }

  it('is a pending-review, unverified entry of the shape of registry/tokens.schema.json', () => {
    const plan = planIssue(kob, form({ description: 'A test token', website: 'https://example.org', icon: 'ipfs://bafy' }), opts());
    const entry = registryEntryFromIssue(plan.token, plan.docs);
    expect(entry).toMatchObject({
      ticker: 'TEST', name: 'Test Token', family: 'kcc20', covenant_id: plan.token.covenantId, template_id: 'kcc20-ref-8x8',
      extension_commitment: '00'.repeat(32), extension_class: 'fixed-supply-standard', decimals: 8,
      max_token_inputs: 8, max_token_outputs: 8, status: 'pending-review', verified: false,
    });
    expect(entry.display).toMatchObject({ description: 'A test token', website: 'https://example.org', icon: 'ipfs://bafy' });
    expect(entry.display!.kcc23).toMatchObject({ standard: 'KCC-20', symbol: 'TEST', totalSupply: '100000000000', fixedSupply: true });
    check(entry, schema.$defs.token, 'entry');
    // the template hash is the one the shipped registry pins for kcc20-ref-8x8
    const registry = JSON.parse(readFileSync(fileURLToPath(new URL('../../../registry/tokens.json', import.meta.url)), 'utf8'));
    const tpl = registry.templates.find((t: { id: string }) => t.id === entry.template_id);
    expect(tpl.template_hash).toBe(plan.token.templateHash);
    expect([tpl.max_token_inputs, tpl.max_token_outputs]).toEqual([8, 8]);
  });

  it('the schema check itself rejects a wrong entry', () => {
    const plan = planIssue(kob, form(), opts());
    const entry = registryEntryFromIssue(plan.token, plan.docs);
    expect(() => check({ ...entry, verified: 'yes' }, schema.$defs.token, 'entry')).toThrow();
    expect(() => check({ ...entry, extra: 1 }, schema.$defs.token, 'entry')).toThrow();
    expect(() => check({ ...entry, covenant_id: 'ABC' }, schema.$defs.token, 'entry')).toThrow();
  });

  it('refuses docs that describe another token', () => {
    const a = planIssue(kob, form(), opts());
    const b = planIssue(kob, form({ ticker: 'OTHR' }), opts());
    expect(() => registryEntryFromIssue(a.token, b.docs)).toThrow(/does not describe/);
  });
});
