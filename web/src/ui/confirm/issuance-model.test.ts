import { describe, expect, it } from 'vitest';
import { emptyIssueForm, planIssue, type IssueForm, type IssuePlan } from '../../kob/issue';
import type { BuiltTx, KeyUtxo } from '../../kob/types';
import { loadKobNode } from '../../kob/wasm.node';
import { t } from '../../i18n';
import { pubkeyOf } from '../../testing/local-signer';
import { buildIssuanceModel } from './issuance-model';

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

const model = (p: IssuePlan, built: BuiltTx = p.built, key = maker) =>
  buildIssuanceModel({ kob, built, token: p.token, maker: key, tr: (k, q) => t(k, q) });

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
    const built = clone(p.built);
    const change = built.fee.changeOutput!;
    built.tx.outputs[change]!.scriptPublicKey = `000020${other}ac`;
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
    const built = clone(p.built);
    const change = built.fee.changeOutput!;
    const take = 2n * KAS;
    built.tx.outputs[change]!.value = (BigInt(built.tx.outputs[change]!.value) - take).toString();
    built.fee.fee = (BigInt(built.fee.fee) + take).toString();
    expect(model(p, built).blocking.map((b) => b.code)).toContain('feeHigh');
  });

  it('the fee policy: a declared rate above max(maxRate, floor) blocks, a dynamic cap raises the 1 KAS ceiling', () => {
    const pol = (o: Partial<{ dynamic: boolean; maxRate: bigint; maxFeeSompi: bigint }> = {}) => ({ dynamic: true, floor: 100n, maxRate: 1000n, maxFeeSompi: 100_000_000n, ...o });
    const m = (p: IssuePlan, feePolicy?: ReturnType<typeof pol>, built: BuiltTx = p.built) => buildIssuanceModel({ kob, built, token: p.token, maker, ...(feePolicy ? { feePolicy } : {}) });
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
    // 1.5 KAS fee: above the fixed 1 KAS ceiling, allowed by a dynamic 2 KAS cap, not by a cap of 1 KAS or by a policy that is off
    const burnt = clone(p.built);
    const change = burnt.fee.changeOutput!;
    const take = 150_000_000n - BigInt(burnt.fee.fee);
    burnt.tx.outputs[change]!.value = (BigInt(burnt.tx.outputs[change]!.value) - take).toString();
    burnt.fee.fee = '150000000';
    expect(m(p, pol({ maxFeeSompi: 200_000_000n }), burnt).blocking.map((b) => b.code)).not.toContain('feeHigh');
    expect(m(p, pol({ maxFeeSompi: 100_000_000n }), burnt).blocking.map((b) => b.code)).toContain('feeHigh');
    expect(m(p, pol({ dynamic: false, maxFeeSompi: 200_000_000n }), burnt).blocking.map((b) => b.code)).toContain('feeHigh');
    expect(en('confirm.issue.finding.feeRate', { rate: '1500', max: '1000' })).toMatch(/1500 sompi per gram.*1000/);
  });

  it('every finding and label has a sentence', () => {
    const p = plan({ holders: [{ owner: maker, ownerScheme: 0, amount: '600' }, { owner: other, ownerScheme: 0, amount: '400' }] });
    const bad = clone(p.built);
    bad.tx.outputs[bad.fee.changeOutput!]!.scriptPublicKey = `000020${other}ac`;
    for (const m of [model(p), model(p, bad)]) {
      const all = [m.heading, m.intro, ...m.sections.flatMap((s) => [s.title, s.note ?? '', ...s.rows.flatMap((r) => [r.label, r.value, r.detail ?? ''])]), ...[...m.blocking, ...m.warnings, ...m.info].map((f) => f.text ?? '')];
      for (const s of all) {
        expect(s).not.toMatch(/\bconfirm\.[a-zA-Z]/);
        expect(s).not.toMatch(/\{\w+\}/);
      }
    }
    expect(en('confirm.issue.finding.unaudited')).toMatch(/audited/);
  });
});
