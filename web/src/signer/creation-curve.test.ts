// The private-key signer's check of a creation whose genesis places a sell curve: the claimed curve
// is re-derived from its state, and a claim that leaves it out, names another maker or another
// output is refused.
import { describe, expect, it } from 'vitest';
import { defaultSaleForm } from '../kob/curve';
import { emptyIssueForm, planIssue } from '../kob/issue';
import type { KeyUtxo } from '../kob/types';
import { loadKobNode } from '../kob/wasm.node';
import { pubkeyOf } from '../testing/local-signer';
import { genesisClaimOf } from '../wallet/create';
import { creationSummary } from './summary';

const kob = loadKobNode();
const KAS = 100_000_000n;
const maker = pubkeyOf('33'.repeat(32));
const utxo: KeyUtxo = { transactionId: '07'.repeat(32), index: 1, amount: (500n * KAS).toString(), blockDaaScore: '5000', pubkey: maker };
const plan = planIssue(kob, { ...emptyIssueForm(), name: 'Curve Coin', ticker: 'CURV', decimals: 8, supply: '1000000', sale: { ...defaultSaleForm(), mode: 'curve', keepPct: '10' } }, { funding: [utxo], maker, network: 'testnet-10' });
const clone = <T,>(x: T): T => JSON.parse(JSON.stringify(x)) as T;

describe('creation with a curve (signer frame)', () => {
  it('is summarised with the curve and the share on sale', () => {
    const s = creationSummary(kob, plan.prep, plan.built, maker, [], genesisClaimOf(plan.token, plan.curve));
    const line = (label: string) => s.lines.find((l) => l.label === label)?.value;
    expect(line('Sell on a curve')).toMatch(/^100 levels, /);
    expect(line('On your curve sell order')).toBe('900000 CURV');
    expect(line('To you')).toBe('+100000 CURV');
    expect(line('To other keys')).toBeUndefined();
  });

  it('refuses a claim without the curve, of another maker, or of another output', () => {
    expect(() => creationSummary(kob, plan.prep, plan.built, maker, [], genesisClaimOf(plan.token))).toThrow('claim');
    const other = clone(plan.curve!);
    other.state.state.maker = pubkeyOf('44'.repeat(32));
    expect(() => creationSummary(kob, plan.prep, plan.built, maker, [], genesisClaimOf(plan.token, other))).toThrow('claim');
    const moved = clone(plan.curve!);
    moved.output = 0;
    expect(() => creationSummary(kob, plan.prep, plan.built, maker, [], genesisClaimOf(plan.token, moved))).toThrow('claim');
  });
});
