import { describe, expect, it } from 'vitest';
import { emptyIssueForm, planIssue, type IssueForm, type IssuePlan } from '../../kob/issue';
import type { BuiltTx, KeyUtxo } from '../../kob/types';
import { loadKobNode } from '../../kob/wasm.node';
import { t } from '../../i18n';
import { pubkeyOf } from '../../testing/local-signer';
import { buildIssuanceModel } from './issuance-model';
import { defaultSaleForm } from '../../kob/curve';

const kob = loadKobNode();
const KAS = 100_000_000n;
const SK = '33'.repeat(32);
const maker = pubkeyOf(SK);
const other = pubkeyOf('44'.repeat(32));

const utxo = (index: number, sompi: bigint): KeyUtxo => ({ transactionId: '07'.repeat(32), index, amount: sompi.toString(), blockDaaScore: '5000', pubkey: maker });
const form = (over: Partial<IssueForm> = {}): IssueForm => ({ ...emptyIssueForm(), name: 'Test Token', ticker: 'TEST', decimals: 8, supply: '1000', ...over });
const plan = (over: Partial<IssueForm> = {}): IssuePlan => planIssue(kob, form(over), { funding: [utxo(1, 500n * KAS)], maker, network: 'testnet-10' });
const en = (k: string, p?: Record<string, string | number | bigint>) => t(k, p);
const clone = <T,>(x: T): T => JSON.parse(JSON.stringify(x)) as T;

/** the confirmation of a creation: the preparation and the genesis (either may be tampered with) */
const model = (p: IssuePlan, built: BuiltTx = p.built, key = maker, prep: BuiltTx = p.prep, oneApproval = true) =>
  buildIssuanceModel({ kob, built, prep, oneApproval, token: p.token, maker: key, tr: (k, q) => t(k, q) });
/** the payload of a declaration JSON text */
const payloadOf = (json: string) => `00${Buffer.from(json, 'utf8').toString('hex')}`;

describe('issuance confirmation model', () => {
  it('an honest genesis verifies: the token, holders, carriers, fee and net effect are shown', () => {
    const p = plan();
    const m = model(p);
    expect(m.kind).toBe('issue');
    expect(m.blocking).toEqual([]);
    expect(m.canSign).toBe(true);
    const issue = m.sections.find((s) => s.id === 'issue')!;
    const row = (id: string) => issue.rows.find((r) => r.id === id)!;
    expect(row('ticker').value).toBe('TEST');
    expect(row('supply').value).toBe('1,000 TEST');
    expect(row('covenantId').value).toBe(p.token.covenantId);
    expect(row('holder-0').value).toBe('1,000 TEST');
    expect(row('holder-0').detail).toBe('your wallet');
    const locked = m.sections.find((s) => s.id === 'locked')!;
    expect(locked.rows.find((r) => r.id === 'locked-total')!.value).toBe('10 KAS');
    const net = m.sections.find((s) => s.id === 'net')!;
    expect(net.rows.find((r) => r.id === 'net-token')!.value).toBe('+1,000 TEST');
    expect(m.sections.find((s) => s.id === 'fee')!.rows[0]!.value).toMatch(/KAS$/);
    // the unaudited program and the blind wallet are always disclosed
    expect(m.warnings.map((w) => w.code)).toEqual(['unaudited']);
    expect(m.info.map((w) => w.code)).toContain('walletBlind');
    // what is recorded on chain with it (KCC-23)
    const declare = m.sections.find((s) => s.id === 'declare')!;
    expect(declare.rows.map((r) => [r.id, r.value])).toEqual([['declare-name', 'Test Token'], ['declare-symbol', 'TEST'], ['declare-decimals', '8']]);
    expect(m.advanced.payload).toEqual(['{"name":"Test Token","symbol":"TEST","decimals":8}']);
    expect(m.advanced.prepTxid).toBe(p.prep.tx.id);
    expect(m.advanced.signatures).toBe(2);
    // the fee is both transactions', the KAS spent is the wallet's coins of the preparation
    const fee = BigInt(p.prep.fee.fee) + BigInt(p.built.fee.fee);
    expect(m.sections.find((s) => s.id === 'fee')!.rows[0]!.value).toBe(`${Number(fee) / 1e8} KAS`);
    expect(m.sections.find((s) => s.id === 'spend')!.rows[0]!.value).toBe('500 KAS');
    expect(m.info.map((w) => w.code)).not.toContain('walletTwice');
    expect(model(p, p.built, maker, p.prep, false).info.map((w) => w.code)).toContain('walletTwice');
    // a saved creation finished later: only the genesis is asked for
    const resumed = buildIssuanceModel({ kob, built: p.built, prep: p.prep, oneApproval: false, resumed: true, token: p.token, maker, tr: (k, q) => t(k, q) });
    expect(resumed.info.map((w) => w.code)).toEqual(expect.arrayContaining(['resumed']));
    expect(resumed.info.map((w) => w.code)).not.toContain('walletTwice');
  });

  it('the declaration must be what the token is shown as, and the preparation must pay only the wallet', () => {
    const p = plan({ description: 'Some text', x: '@kob' });
    const d = model(p).sections.find((s) => s.id === 'declare')!;
    expect(d.rows.find((r) => r.id === 'declare-description')!.value).toBe('Some text');
    expect(d.rows.find((r) => r.id === 'declare-x')!.value).toBe('https://x.com/kob');
    // another declaration than the claimed token (here: 2 decimals)
    const lying = clone(p.prep);
    lying.tx.payload = payloadOf('{"name":"Test Token","symbol":"TEST","decimals":2}');
    expect(model(p, p.built, maker, lying).blocking.map((b) => b.code)).toContain('declaration');
    const notKcc23 = clone(p.prep);
    notKcc23.tx.payload = '017b7d';
    expect(model(p, p.built, maker, notKcc23).blocking.map((b) => b.code)).toContain('declaration');
    // the preparation's output 0 or change to another key
    for (const n of [0, 1]) {
      const away = clone(p.prep);
      away.tx.outputs[n]!.scriptPublicKey = `000020${other}ac`;
      expect(model(p, p.built, maker, away).blocking.map((b) => b.code), `output ${n}`).toContain('prepOutput');
    }
    // a genesis that does not spend this preparation
    const unchained = clone(p.built);
    unchained.tx.inputs[0]!.transactionId = '99'.repeat(32);
    expect(model(p, unchained).blocking.map((b) => b.code)).toContain('prepSpend');
  });

  it('extra holders are listed and disclosed as tokens going to other keys', () => {
    const p = plan({ supply: '1000', holders: [{ owner: maker, ownerScheme: 0, amount: '600' }, { owner: other, ownerScheme: 0, amount: '400' }] });
    const m = model(p);
    expect(m.canSign).toBe(true);
    const issue = m.sections.find((s) => s.id === 'issue')!;
    expect(issue.rows.find((r) => r.id === 'holder-1')!.tone).toBe('warn');
    expect(m.info.find((f) => f.code === 'holdersOther')!.params).toEqual({ count: 1 });
    expect(m.sections.find((s) => s.id === 'net')!.rows.find((r) => r.id === 'net-token')!.value).toBe('+600 TEST');
    expect(m.sections.find((s) => s.id === 'locked')!.rows.find((r) => r.id === 'locked-total')!.value).toBe('20 KAS');
  });

  it('blocks when the planner claims a different supply than the outputs carry', () => {
    const p = plan();
    const bad = { ...p, token: { ...p.token, supply: (BigInt(p.token.supply) * 2n).toString() } };
    expect(model(bad).blocking.map((b) => b.code)).toContain('supply');
  });

  it('blocks a token output whose script is not the claimed token state (tokens redirected)', () => {
    const p = plan();
    const built = clone(p.built);
    // the claim says the tokens are owned by the maker; the transaction pays them to another key's token script
    const swapped = plan({ holders: [{ owner: other, ownerScheme: 0, amount: '1000' }] });
    built.tx.outputs[swapped.token.outputs[0]!.index]!.scriptPublicKey = swapped.built.tx.outputs[swapped.token.outputs[0]!.index]!.scriptPublicKey;
    const codes = model(p, built).blocking.map((b) => b.code);
    expect(codes).toContain('outputScript');
  });

  it('blocks a KAS output to another key and a wrong carrier', () => {
    const p = plan();
    // a genesis with an extra output to another key
    const built = clone(p.built);
    built.tx.outputs.push({ ...built.tx.outputs[0]!, scriptPublicKey: `000020${other}ac`, covenant: null });
    expect(model(p, built).blocking.map((b) => b.code)).toContain('outputOther');
    const built2 = clone(p.built);
    built2.tx.outputs[p.token.outputs[0]!.index]!.value = (BigInt(built2.tx.outputs[p.token.outputs[0]!.index]!.value) + 1n).toString();
    expect(model(p, built2).blocking.map((b) => b.code)).toEqual(expect.arrayContaining(['outputCarrier', 'fee']));
  });

  it('blocks a foreign input and a signature request for another key', () => {
    const p = plan();
    expect(model(p, p.built, other).blocking.map((b) => b.code)).toEqual(expect.arrayContaining(['input', 'signKey']));
  });

  it('blocks an unreasonable fee', () => {
    const p = plan();
    const prep = clone(p.prep);
    const change = prep.fee.changeOutput!;
    const take = 2n * KAS;
    prep.tx.outputs[change]!.value = (BigInt(prep.tx.outputs[change]!.value) - take).toString();
    prep.fee.fee = (BigInt(prep.fee.fee) + take).toString();
    expect(model(p, p.built, maker, prep).blocking.map((b) => b.code)).toContain('feeHigh');
    // a declared fee that is not inputs minus outputs
    const off = clone(p.prep);
    off.fee.fee = (BigInt(off.fee.fee) + 1n).toString();
    expect(model(p, p.built, maker, off).blocking.map((b) => b.code)).toContain('fee');
  });

  it('the fee policy: a declared rate above max(maxRate, floor) blocks, a dynamic cap raises the 1 KAS ceiling', () => {
    const pol = (o: Partial<{ dynamic: boolean; maxRate: bigint; maxFeeSompi: bigint }> = {}) => ({ dynamic: true, floor: 100n, maxRate: 1000n, maxFeeSompi: 100_000_000n, ...o });
    const m = (p: IssuePlan, feePolicy?: ReturnType<typeof pol>, built: BuiltTx = p.built, prep: BuiltTx = p.prep) =>
      buildIssuanceModel({ kob, built, prep, token: p.token, maker, ...(feePolicy ? { feePolicy } : {}) });
    const p = plan();
    expect(m(p, pol()).blocking).toEqual([]);
    const dear = clone(p.built);
    dear.fee.feeRate = '1500';
    expect(m(p, pol(), dear).blocking.map((b) => b.code)).toContain('feeRate');
    expect(m(p, pol({ maxRate: 2000n }), dear).blocking.map((b) => b.code)).not.toContain('feeRate');
    expect(m(p, pol({ dynamic: false }), p.built).blocking).toEqual([]);
    const at194 = clone(p.built);
    at194.fee.feeRate = '194';
    expect(m(p, pol({ dynamic: false }), at194).blocking.map((b) => b.code)).toContain('feeRate');
    expect(m(p, undefined, dear).blocking).toEqual([]);
    // the preparation's declared rate counts too
    const dearPrep = clone(p.prep);
    dearPrep.fee.feeRate = '1500';
    expect(m(p, pol(), p.built, dearPrep).blocking.map((b) => b.code)).toContain('feeRate');
    // 1.5 KAS of fees: above the fixed 1 KAS ceiling, allowed by a dynamic 2 KAS cap, not by a cap of 1 KAS or by a policy that is off
    const burnt = clone(p.prep);
    const change = burnt.fee.changeOutput!;
    const take = 150_000_000n - BigInt(burnt.fee.fee) - BigInt(p.built.fee.fee);
    burnt.tx.outputs[change]!.value = (BigInt(burnt.tx.outputs[change]!.value) - take).toString();
    burnt.fee.fee = (150_000_000n - BigInt(p.built.fee.fee)).toString();
    expect(m(p, pol({ maxFeeSompi: 200_000_000n }), p.built, burnt).blocking.map((b) => b.code)).not.toContain('feeHigh');
    expect(m(p, pol({ maxFeeSompi: 100_000_000n }), p.built, burnt).blocking.map((b) => b.code)).toContain('feeHigh');
    expect(m(p, pol({ dynamic: false, maxFeeSompi: 200_000_000n }), p.built, burnt).blocking.map((b) => b.code)).toContain('feeHigh');
    expect(en('confirm.issue.finding.feeRate', { rate: '1500', max: '1000' })).toMatch(/1500 sompi per gram.*1000/);
  });

  it('every finding and label has a sentence', () => {
    const p = plan({ holders: [{ owner: maker, ownerScheme: 0, amount: '600' }, { owner: other, ownerScheme: 0, amount: '400' }] });
    const bad = clone(p.prep);
    bad.tx.outputs[bad.fee.changeOutput!]!.scriptPublicKey = `000020${other}ac`;
    const lying = clone(p.prep);
    lying.tx.payload = '007b7d';
    for (const m of [model(p), model(p, p.built, maker, bad), model(p, p.built, maker, lying, false)]) {
      const all = [m.heading, m.intro, ...m.sections.flatMap((s) => [s.title, s.note ?? '', ...s.rows.flatMap((r) => [r.label, r.value, r.detail ?? ''])]), ...[...m.blocking, ...m.warnings, ...m.info].map((f) => f.text ?? '')];
      for (const s of all) {
        expect(s).not.toMatch(/\bconfirm\.[a-zA-Z]/);
        expect(s).not.toMatch(/\{\w+\}/);
      }
    }
    expect(en('confirm.issue.finding.unaudited')).toMatch(/audited/);
  });

  it('the short summary: what is created, kept, on sale, the fees of both transactions and the KAS locked, with the explanations behind "?"', () => {
    const p = plan({ supply: '1000000', sale: { ...defaultSaleForm(), mode: 'curve', keepPct: '10' } });
    const m = buildIssuanceModel({ kob, built: p.built, prep: p.prep, oneApproval: true, token: p.token, curve: p.curve!, maker, tr: (k, q) => t(k, q) });
    const brief = m.brief!;
    expect(brief.map((r) => r.id)).toEqual(['create', 'keep', 'sale', 'fee', 'locked']);
    const r = (id: string) => brief.find((x) => x.id === id)!;
    expect(r('create').value).toBe('1,000,000 TEST');
    expect(r('create').detail).toBe('Test Token, 8 decimals, fixed supply');
    expect(r('keep').value).toBe('100,000 TEST');
    expect(r('sale').value).toBe('900,000 TEST');
    expect(r('sale').detail).toMatch(/^on a curve: 100 levels, [\d.]+ to [\d.]+ KAS$/);
    const fee = BigInt(p.prep.fee.fee) + BigInt(p.built.fee.fee);
    expect(r('fee').value).toBe(`${Number(fee) / 1e8} KAS`);
    expect(r('fee').detail).toBe('for 2 transactions, not returned');
    expect(r('locked').value).toBe(m.sections.find((s) => s.id === 'locked')!.rows.find((x) => x.id === 'locked-total')!.value);
    // nothing is lost: the intro, the lock's note and each carrier are behind the "?"s
    expect(r('create').help).toEqual([m.intro]);
    expect(r('locked').help![0]).toBe(t('confirm.issue.lockedNote'));
    expect(r('locked').help!.some((h) => h.startsWith('KAS on the curve order: '))).toBe(true);
    // the unaudited program stays a warning (the screen's one risk line)
    expect(m.warnings.map((w) => w.code)).toEqual(['unaudited']);
    // one price: placed after creation, shown in the summary; tokens to another key are a row of their own
    const q = plan({ supply: '1000', holders: [{ owner: maker, ownerScheme: 0, amount: '600' }, { owner: other, ownerScheme: 0, amount: '400' }] });
    const ask = buildIssuanceModel({ kob, built: q.built, prep: q.prep, token: q.token, askSale: { sell: 50_000_000_000n, price: 2n * KAS }, maker, tr: (k, x) => t(k, x) }).brief!;
    expect(ask.find((x) => x.id === 'sale')!.detail).toBe('at 2 KAS each, as a sell order you place after creation');
    expect(ask.find((x) => x.id === 'others')).toMatchObject({ value: '400 TEST', tone: 'warn', detail: 'One output sends tokens to another key, as you asked.' });
    for (const s of [...brief, ...ask].flatMap((x) => [x.label, x.value, x.detail ?? '', ...(x.help ?? [])])) {
      expect(s).not.toMatch(/confirm\.[a-zA-Z]/);
      expect(s).not.toMatch(/\{\w+\}/);
    }
  });

  it('a curve created with the token verifies, is shown, and any change to it blocks', () => {
    const p = plan({ supply: '1000000', sale: { ...defaultSaleForm(), mode: 'curve', keepPct: '10' } });
    const withCurve = (curve = p.curve!, built = p.built) => buildIssuanceModel({ kob, built, prep: p.prep, oneApproval: true, token: p.token, curve, maker, tr: (k, q) => t(k, q) });
    const m = withCurve();
    expect(m.blocking).toEqual([]);
    const c = m.sections.find((s) => s.id === 'curve')!;
    expect(c.rows.find((r) => r.id === 'curve-amount')!.value).toBe('900,000 TEST');
    expect(c.rows.find((r) => r.id === 'curve-levels')!.value).toBe('100');
    const issue = m.sections.find((s) => s.id === 'issue')!;
    expect(issue.rows.find((r) => r.id === 'holder-0')!.detail).toBe('your curve order (on sale)');
    expect(issue.rows.find((r) => r.id === 'holder-1')!.detail).toBe('your wallet');
    // no "tokens to other keys" for the curve's custody
    expect(m.info.map((i) => i.code)).not.toContain('holdersOther');
    // both carriers are locked
    expect(m.sections.find((s) => s.id === 'locked')!.rows.find((r) => r.id === 'locked-curve')).toBeDefined();
    // a curve claimed for another maker, a changed table, or a claim left out: blocked
    const other2 = clone(p.curve!);
    other2.state.state.maker = other;
    expect(withCurve(other2).blocking.map((b) => b.code)).toContain('curve');
    const table = clone(p.curve!);
    table.state.state.table[0] = '1';
    expect(withCurve(table).blocking.map((b) => b.code)).toContain('curve');
    expect(model(p).blocking.map((b) => b.code)).toContain('outputOther');
    for (const s of [...m.sections.flatMap((x) => [x.title, x.note ?? '', ...x.rows.flatMap((r) => [r.label, r.value, r.detail ?? ''])]), ...withCurve(other2).blocking.map((b) => b.text ?? '')]) {
      expect(s).not.toMatch(/confirm.[a-zA-Z]/);
      expect(s).not.toMatch(/{w+}/);
    }
  });
});
