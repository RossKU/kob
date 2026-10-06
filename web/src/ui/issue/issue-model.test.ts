import { describe, expect, it } from 'vitest';
import { kob } from '../../testing/fixtures';
import { pubkeyOf, signBuilt } from '../../testing/local-signer';
import { issueLimits, issuedTokenUtxos, planIssue, validateIssueForm, IssueFormError, type IssueLimits } from '../../kob/issue';
import type { KeyUtxo } from '../../kob/types';
import {
  defaultCarrierText, emptyUiState, filterDecimals, filterOwner, filterTicker, formToIssueForm, groupByField, holderViews, humanAmount,
  metadataDocFileName, modelIssues, parseDecimalsText, registryEntryFileName, remainingText, resultViewModel, reviewSummary, sharesOf,
  shortfallOf, supplyDocFileName, supplyPreview, uiFieldOf, walletRowIncluded, type IssueUiState,
} from './issue-model';

const k = kob();
const limits: IssueLimits = issueLimits(k);
const SK = '33'.repeat(32);
const wallet = pubkeyOf(SK);
const other = pubkeyOf('44'.repeat(32));
const KAS = 100_000_000n;

const state = (over: Partial<IssueUiState> = {}): IssueUiState => ({ ...emptyUiState(), name: 'Test Token', ticker: 'TEST', decimals: '8', supply: '1000', ...over });
const utxo = (index: number, sompi: bigint): KeyUtxo => ({ transactionId: '07'.repeat(32), index, amount: sompi.toString(), blockDaaScore: '5000', pubkey: wallet });
const codes = (s: IssueUiState, tickers: string[] = []) => validateIssueForm(formToIssueForm(s, wallet), limits, tickers).map((i) => i.code);

describe('input filters', () => {
  it('uppercases the ticker as typed and drops whitespace, keeping other characters for the validator', () => {
    expect(filterTicker('kas t 1')).toBe('KAST1');
    expect(filterTicker('ex-kcc')).toBe('EX-KCC');
    expect(filterTicker('a'.repeat(50))).toHaveLength(32);
    expect(codes(state({ ticker: filterTicker('ex-kcc') }))).toEqual(['ticker.chars']);
  });

  it('keeps decimals to digits and parses them strictly', () => {
    expect(filterDecimals('1x8.')).toBe('18');
    expect(filterDecimals('123')).toBe('12');
    expect(parseDecimalsText('18')).toBe(18);
    expect(parseDecimalsText('')).toBeNaN();
    expect(parseDecimalsText('-1')).toBeNaN();
    expect(parseDecimalsText('8.5')).toBeNaN();
  });

  it('normalises pasted owner keys', () => {
    expect(filterOwner('  AB CD\n')).toBe('abcd');
    expect(filterOwner('F'.repeat(80))).toBe('f'.repeat(64));
  });
});

describe('shares', () => {
  it('computes the wallet share as supply minus the others with bigint precision (18 decimals)', () => {
    const s = state({
      decimals: '18',
      supply: '2.9',
      extras: [{ owner: other, amount: '0.000000000000000001' }],
    });
    const sh = sharesOf(s);
    expect(sh.supply).toBe(2_900_000_000_000_000_000n);
    expect(sh.others).toBe(1n);
    expect(sh.remaining).toBe(2_900_000_000_000_000_000n - 1n);
    expect(remainingText(s)).toBe('2.899999999999999999');
    // 2.9 tokens at 18 decimals is exactly the maximum supply
    expect(codes(s)).toEqual([]);
  });

  it('max supply at the protocol limit is valid, one base unit more is not', () => {
    const ok = state({ decimals: '0', supply: BigInt(limits.maxSupply).toString() });
    expect(codes(ok)).toEqual([]);
    const over = state({ decimals: '0', supply: (BigInt(limits.maxSupply) + 1n).toString() });
    expect(codes(over)).toEqual(['supply.too_large']);
  });

  it('reports precision errors of the supply and of holder amounts', () => {
    expect(codes(state({ decimals: '2', supply: '1.234' }))).toEqual(['supply.precision']);
    const s = state({ decimals: '2', supply: '100', extras: [{ owner: other, amount: '1.001' }] });
    expect(codes(s)).toContain('holder.amount.precision');
  });

  it('with no extra holders the list is empty (all to the wallet) and the remaining share is the whole supply', () => {
    const s = state();
    expect(walletRowIncluded(s)).toBe(false);
    expect(formToIssueForm(s, wallet).holders).toEqual([]);
    expect(remainingText(s)).toBe('1000');
    expect(codes(s)).toEqual([]);
  });

  it('puts the wallet first with the remainder, then the extras', () => {
    const s = state({ supply: '1000', extras: [{ owner: other.toUpperCase(), amount: '250.5' }, { owner: 'ab'.repeat(32), amount: '100' }] });
    const f = formToIssueForm(s, wallet);
    expect(f.holders).toEqual([
      { owner: wallet, ownerScheme: 0, amount: '649.5' },
      { owner: other, ownerScheme: 0, amount: '250.5' },
      { owner: 'ab'.repeat(32), ownerScheme: 0, amount: '100' },
    ]);
    // the holders add up to the supply: no sum mismatch
    expect(codes(s)).not.toContain('holders.sum_mismatch');
    expect(walletRowIncluded(s)).toBe(true);
  });

  it('drops the wallet row when the others take the whole supply', () => {
    const s = state({ supply: '10', extras: [{ owner: other, amount: '4' }, { owner: 'cd'.repeat(32), amount: '6' }] });
    expect(walletRowIncluded(s)).toBe(false);
    expect(formToIssueForm(s, wallet).holders.map((h) => h.amount)).toEqual(['4', '6']);
    expect(codes(s)).not.toContain('holders.sum_mismatch');
  });

  it('flags holders that exceed the supply and keeps the plan blocked', () => {
    const s = state({ supply: '10', extras: [{ owner: other, amount: '10.5' }] });
    expect(sharesOf(s).remaining).toBe(-50_000_000n);
    expect(remainingText(s)).toBe('');
    const mi = modelIssues(s, wallet);
    expect(mi).toEqual([{ code: 'holders.exceeds', severity: 'error', params: { others: '10.5', supply: '10' } }]);
    // the mapped form has a zero wallet share, which the planner refuses too
    expect(codes(s)).toContain('holder.amount.zero');
  });

  it('warns when an extra holder is the wallet itself', () => {
    const s = state({ extras: [{ owner: wallet, amount: '1' }] });
    expect(modelIssues(s, wallet).map((i) => [i.code, i.severity])).toEqual([['holders.duplicate', 'warning']]);
  });

  it('an unparsable supply leaves the shares unknown', () => {
    const s = state({ supply: 'abc', extras: [{ owner: other, amount: '1' }] });
    expect(sharesOf(s).remaining).toBeNull();
    expect(walletRowIncluded(s)).toBe(true);
    expect(codes(s)).toContain('supply.invalid');
  });

  it('too many holders is an error (limit from kob-wasm)', () => {
    const extras = Array.from({ length: limits.maxGenesisOutputs }, (_, i) => ({ owner: (i + 1).toString(16).padStart(2, '0').repeat(32), amount: '1' }));
    const s = state({ decimals: '0', supply: '100000', extras });
    expect(codes(s)).toContain('holders.too_many');
  });
});

describe('field mapping', () => {
  it('maps holder findings to UI rows (row 0 = the wallet)', () => {
    expect(uiFieldOf('holders[0].amount', true)).toBe('holder-0-amount');
    expect(uiFieldOf('holders[2].owner', true)).toBe('holder-2-owner');
    // without the wallet row the first planner holder is UI row 1
    expect(uiFieldOf('holders[0].owner', false)).toBe('holder-1-owner');
    expect(uiFieldOf('holders', true)).toBe('holders');
    expect(uiFieldOf('ticker', true)).toBe('ticker');
    expect(uiFieldOf(undefined, true)).toBeNull();
  });

  it('groups findings by field, general ones under the empty key', () => {
    const issues = validateIssueForm(formToIssueForm(state({ name: '', ticker: 'x', supply: '0' }), wallet), limits);
    const g = groupByField(issues, false);
    expect([...g.keys()]).toEqual(['name', 'ticker', 'supply']);
    const g2 = groupByField([{ code: 'funds.insufficient', severity: 'error', message: 'm' }], false);
    expect(g2.get('')).toHaveLength(1);
  });

  it('a lookalike or duplicate ticker blocks with a ticker-level finding', () => {
    expect(codes(state({ ticker: 'EXKCC' }), ['EXKCC'])).toEqual(['ticker.exists']);
    expect(codes(state({ ticker: 'K0N1' }), ['KONI'])).toEqual(['ticker.lookalike']);
  });
});

describe('funding shortfall', () => {
  it('reads the exact missing KAS from funds.insufficient', () => {
    expect(shortfallOf({ code: 'funds.insufficient', params: { need: 1_000_000_000n, have: 350_000_000n } })).toEqual({
      need: 1_000_000_000n, have: 350_000_000n, missing: 650_000_000n,
    });
    // params arrive as strings from JSON as well
    expect(shortfallOf({ code: 'funds.insufficient', params: { need: '1000000000', have: '1000000000' } })?.missing).toBe(0n);
    expect(shortfallOf({ code: 'other', params: { need: 1n, have: 0n } })).toBeNull();
    expect(shortfallOf({ code: 'funds.insufficient', params: { need: 'x', have: 0n } })).toBeNull();
  });

  it('agrees with planIssue when the wallet cannot pay the carriers', () => {
    const f = formToIssueForm(state({ carrier: '30', supply: '10', extras: [{ owner: other, amount: '5' }] }), wallet);
    try {
      planIssue(k, f, { funding: [utxo(1, 45n * KAS)], maker: wallet, network: 'testnet-10' });
      throw new Error('expected a shortfall');
    } catch (e) {
      expect(e).toBeInstanceOf(IssueFormError);
      const sf = shortfallOf((e as IssueFormError).issues[0])!;
      expect(sf).toEqual({ need: 60n * KAS, have: 45n * KAS, missing: 15n * KAS });
    }
  });
});

describe('display helpers', () => {
  it('formats amounts in human units with grouping', () => {
    expect(humanAmount('100000000000', 8)).toBe('1,000');
    expect(humanAmount(1n, 18)).toBe('0.000000000000000001');
    expect(humanAmount(123456789012345678901234567890n, 18)).toBe('123,456,789,012.34567890123456789');
    expect(humanAmount(5n, 0)).toBe('5');
  });

  it('previews the supply and hides the preview for junk', () => {
    expect(supplyPreview(state({ supply: '1000000' }))).toEqual({ human: '1,000,000', base: '100000000000000' });
    expect(supplyPreview(state({ supply: '0' }))).toBeNull();
    expect(supplyPreview(state({ supply: '1.' }))).toBeNull();
    expect(supplyPreview(state({ decimals: '' }))).toBeNull();
  });

  it('shows the protocol default carrier', () => {
    expect(defaultCarrierText(limits)).toBe('10');
  });

  it('names the files after the ticker', () => {
    expect(registryEntryFileName('KAS1')).toBe('KAS1.registry-entry.json');
    expect(supplyDocFileName('KAS1')).toBe('KAS1.supply.json');
    expect(metadataDocFileName('KAS1')).toBe('KAS1.metadata.json');
    expect(registryEntryFileName('../x')).toBe('___x.registry-entry.json');
    expect(registryEntryFileName('')).toBe('token.registry-entry.json');
  });
});

describe('a real issuance from a model-built form', () => {
  const s = state({
    name: 'Model Coin',
    ticker: 'MODL',
    decimals: '2',
    supply: '1000.5',
    carrier: '2.5',
    description: 'made in a test',
    website: 'https://example.org',
    extras: [{ owner: other, amount: '100.25' }, { owner: 'ab'.repeat(32), amount: '50' }],
  });

  it('has no findings, plans, signs and validates', () => {
    expect(validateIssueForm(formToIssueForm(s, wallet), limits, ['EXKCC'])).toEqual([]);
    const plan = planIssue(k, formToIssueForm(s, wallet), { funding: [utxo(1, 500n * KAS)], maker: wallet, network: 'testnet-10', registryTickers: ['EXKCC'] });
    expect(plan.built.tx.outputs[0].covenant?.covenantId).toBe(plan.token.covenantId);
    expect(plan.token.supply).toBe('100050');
    expect(plan.token.outputs.map((o) => [o.owner, o.amount])).toEqual([[wallet, '85025'], [other, '10025'], ['ab'.repeat(32), '5000']]);
    // consensus check: the genesis is valid
    const signed = k.finalize(plan.built, signBuilt(plan.built, [SK]), { tightenBudgets: true });
    expect(BigInt((k.validate(signed) as { fee: string; minFee: string }).fee)).toBeGreaterThanOrEqual(0n);

    // issuedTokenUtxos match the holders
    const utxos = issuedTokenUtxos(plan);
    expect(utxos.map((u) => [u.state.owner, u.state.amount])).toEqual(plan.token.outputs.map((o) => [o.owner, o.amount]));
    expect(utxos.every((u) => u.amount === (250_000_000n).toString())).toBe(true);
    expect(utxos.reduce((a, u) => a + BigInt(u.state.amount), 0n)).toBe(100050n);

    // review summary
    const sum = reviewSummary(plan, wallet);
    expect(sum).toMatchObject({ name: 'Model Coin', ticker: 'MODL', decimals: 2, supplyBase: 100050n, supplyHuman: '1,000.5', carrierEach: 250_000_000n, carrierTotal: 750_000_000n, outputCount: 3, program: 'KCC20Ref_8x8' });
    expect(sum.fee).toBe(BigInt(plan.built.fee.fee));
    expect(sum.holders.map((h) => [h.human, h.isWallet])).toEqual([['850.25', true], ['100.25', false], ['50', false]]);
    expect(holderViews(plan.token, null).some((h) => h.isWallet)).toBe(false);

    // result view model
    const txid = plan.built.tx.id;
    const vm = resultViewModel(plan, txid, wallet);
    expect(vm.rows.find((r) => r.id === 'tokenId')?.value).toBe(plan.token.covenantId);
    expect(vm.rows.find((r) => r.id === 'txid')?.value).toBe(txid);
    expect(vm.rows.find((r) => r.id === 'templateHash')?.value).toBe(plan.token.templateHash);
    expect(vm.rows.find((r) => r.id === 'extension')?.value).toBe(plan.token.extensionCommitment);
    expect(vm.status).toEqual({ verified: false, pendingReview: true });
    const entry = JSON.parse(vm.registryEntryJson);
    expect(entry).toMatchObject({ ticker: 'MODL', covenant_id: plan.token.covenantId, status: 'pending-review', verified: false, decimals: 2 });
    expect(vm.registryEntryJson.endsWith('}\n')).toBe(true);
    expect(vm.registryEntryFile).toBe('MODL.registry-entry.json');
    expect(JSON.parse(vm.supplyJson)).toEqual(plan.docs.supply);
    expect(JSON.parse(vm.metadataJson)).toEqual(plan.docs.metadata);
  });

  it('refuses a result whose docs do not describe the token', () => {
    const plan = planIssue(k, formToIssueForm(s, wallet), { funding: [utxo(1, 500n * KAS)], maker: wallet, network: 'testnet-10' });
    const forged = { token: { ...plan.token, covenantId: 'ee'.repeat(32) }, docs: plan.docs };
    expect(() => resultViewModel(forged, plan.built.tx.id)).toThrow(/does not describe/);
  });

  it('the empty-holders default sends everything to the wallet', () => {
    const plan = planIssue(k, formToIssueForm(state(), wallet), { funding: [utxo(1, 500n * KAS)], maker: wallet, network: 'testnet-10' });
    expect(plan.token.outputs).toHaveLength(1);
    expect(plan.token.outputs[0]).toMatchObject({ owner: wallet, amount: '100000000000' });
    expect(reviewSummary(plan, wallet).carrierTotal).toBe(10n * KAS);
  });
});
