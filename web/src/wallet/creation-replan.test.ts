// A saved creation is planned again from its facts before the wallet is asked for its genesis: a valid record reproduces its ids, a tampered one
// is refused.
import { describe, expect, it } from 'vitest';
import { replanCreation } from './creation-replan';
import { checkSavedCreation, CREATION_INVALID } from './create';
import { MAKER_PK, OTHER_PK } from '../testing/token-fixtures';
import { kob } from '../testing/wallet-fixtures';
import { emptyIssueForm, planIssue, type IssueForm, type IssuePlan } from '../kob/issue';
import type { CreationRecord } from '../data/creations';

const NET = 'testnet-10';
const KAS = 100_000_000n;
const fund = (n: number, kas: bigint, pubkey = MAKER_PK) => ({ transactionId: n.toString(16).padStart(2, '0').repeat(32), index: n, amount: (kas * KAS).toString(), blockDaaScore: '100', pubkey });

function plan(form: Partial<IssueForm> = {}, funding = [fund(10, 200n)], feeRate?: bigint, maker = MAKER_PK): IssuePlan {
  return planIssue(kob, { ...emptyIssueForm(), name: 'Flow Coin', ticker: 'FLOW', decimals: 2, supply: '1000', ...form }, {
    funding, maker, network: NET, ...(feeRate ? { feeRate } : {}),
  });
}

/** The record signAndSubmitCreation saves (the preparation's signature scripts do not matter here). */
const recordOf = (p: IssuePlan): CreationRecord => JSON.parse(JSON.stringify({
  network: NET, maker: MAKER_PK, createdAt: 1, covenantId: p.token.covenantId, prepTxid: p.prep.tx.id, genesisTxid: p.built.tx.id, prep: p.prep.tx,
  genesisBuilt: p.built, token: p.token, declaration: p.declaration, docs: p.docs, prepSent: true,
})) as CreationRecord;

describe('replanCreation', () => {
  it('a valid record reproduces its preparation and genesis', () => {
    const p = plan();
    const r = replanCreation(kob, recordOf(p), MAKER_PK);
    expect(r.ok).toBe(true);
    if (r.ok) {
      expect(r.plan.built.tx).toEqual(p.built.tx);
      expect(r.plan.prep.tx.id).toBe(p.prep.tx.id);
    }
  });

  it('every optional field, several holders, several coins, the 3 / 3 program and a fee rate', () => {
    const p = plan(
      {
        description: 'A coin', icon: 'https://example.com/a.png', imageHash: 'ab'.repeat(32), website: 'https://example.com', x: '@flowcoin', telegram: 'flowcoin_tg', program: '3x3',
        holders: [{ owner: MAKER_PK, ownerScheme: 0, amount: '750' }, { owner: OTHER_PK, ownerScheme: 0, amount: '250' }], carrier: '5',
      } as Partial<IssueForm>,
      [fund(11, 6n), fund(12, 7n), fund(13, 9n)],
      2000n,
    );
    expect(p.prep.tx.inputs.length).toBeGreaterThan(1);
    expect(p.token.outputs).toHaveLength(2);
    expect(replanCreation(kob, recordOf(p), MAKER_PK).ok).toBe(true);
  });

  it('a v1 record (no plan, a signed genesis) at another fee rate: the rate is read from the genesis fee', () => {
    const p = plan({}, [fund(10, 200n)], 3000n);
    const { genesisBuilt: _drop, ...v1 } = recordOf(p);
    const r = replanCreation(kob, { ...v1, genesis: p.built.tx } as CreationRecord, MAKER_PK);
    expect(r.ok && r.plan.built.tx.id).toBe(p.built.tx.id);
  });

  it('refuses a tampered record without throwing', () => {
    const p = plan();
    const cases: [string, (r: CreationRecord) => void][] = [
      ['funding amount', (r) => void (r.prep.inputs[0]!.utxo.amount = '1')],
      ['fee rate', (r) => void (r.genesisBuilt!.fee.feeRate = '999999')],
      ['supply', (r) => void (r.token.supply = '2000')],
      ['program', (r) => void (r.token.program = 'KCC20Ref')],
      ['declaration', (r) => void (r.prep.payload = r.prep.payload.slice(0, -2) + '20')],
      ['covenant id', (r) => void (r.covenantId = 'cc'.repeat(32))],
      ['garbage', (r) => void ((r as unknown as { token: unknown }).token = { outputs: 'x' })],
    ];
    for (const [what, edit] of cases) {
      const r = recordOf(p);
      edit(r);
      expect(replanCreation(kob, r, MAKER_PK).ok, what).toBe(false);
    }
  });

  it("another key's creation relabeled as this wallet's reproduces its ids but fails the checks of a fresh creation", () => {
    const p = plan({}, [fund(10, 200n, OTHER_PK)], undefined, OTHER_PK);
    const r = { ...recordOf(p), maker: MAKER_PK };
    expect(replanCreation(kob, r, MAKER_PK).ok).toBe(true);
    expect(() => checkSavedCreation(kob, r, MAKER_PK)).toThrow(expect.objectContaining({ code: CREATION_INVALID }));
    expect(checkSavedCreation(kob, recordOf(plan()), MAKER_PK).built.tx.id).toBe(plan().built.tx.id);
  });
});
