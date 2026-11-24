import { describe, expect, it } from 'vitest';
import { loadKobNode } from './wasm.node';
import { placeGolden } from '../testing/chain-fixtures';
import { isGenesisBinding } from './covenant-id';

const kob = loadKobNode();

describe('the genesis covenant id, computed from bound data (R9FB2-1)', () => {
  for (const name of ['create.ask', 'create.bid', 'create.condAsk', 'pair.create.ask', 'create.curve']) {
    it(`matches the id the consensus hash gives a real placement (${name})`, () => {
      const g = placeGolden(kob, name);
      const tx = g.built.tx;
      const placed = kob.recoverOrders(tx);
      expect(placed.length).toBeGreaterThan(0);
      for (const r of placed) {
        const out = tx.outputs.find((o) => o.covenant?.covenantId === r.covenantId)!;
        expect(isGenesisBinding(tx, out.covenant!.authorizingInput, r.covenantId)).toBe(true);
        // and not for another id
        expect(isGenesisBinding(tx, out.covenant!.authorizingInput, 'ab'.repeat(32))).toBe(false);
      }
    });
  }
});
