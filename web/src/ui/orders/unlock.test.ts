// The Unlock press before the node's clock reaches the lock time: refused with its own sentence,
// not the generic "time terms" one.
import { describe, expect, it } from 'vitest';
import type { Services } from '../../app/services';
import { loadKobNode } from '../../kob/wasm.node';
import { MAKER } from '../../testing/chain-fixtures';
import { describeActionError, rawActionError } from './actions';
import type { LockedHolding } from './balances-data';
import { planUnlock } from './unlock';

const kob = loadKobNode();
const UNTIL = 1_800_000_000_000n;
const holding: LockedHolding = { token: 'ab'.repeat(32), program: 'KCC20PublicMint', until: UNTIL, key: MAKER.pk, authority: 'cd'.repeat(32), utxos: [], amount: 1000n, kind: 'manual', coin: null };
const services = (median: bigint | null) => ({ kob, config: { network: 'testnet-10' }, node: { getPastMedianTime: async () => median } }) as unknown as Services;

describe('planUnlock before the node clock is past the lock time', () => {
  it('says how far the node clock is, as the banner text, with nothing behind Details', async () => {
    const err = await planUnlock(services(UNTIL - 125_000n), MAKER.pk, holding, 'LOCK').catch((e: unknown) => e);
    expect(describeActionError(err)).toBe('Not open yet on the network: the node clock is 3 min before the lock time. Try again then.');
    expect(rawActionError(err)).toBeNull();
  });

  it('refuses at a node clock equal to the lock time too', async () => {
    const err = await planUnlock(services(UNTIL), MAKER.pk, holding, 'LOCK').catch((e: unknown) => e);
    expect(describeActionError(err)).toMatch(/^Not open yet on the network: the node clock is 1 min/);
  });

  it('another key: its own sentence', async () => {
    const err = await planUnlock(services(UNTIL + 1n), 'ef'.repeat(32), holding, 'LOCK').catch((e: unknown) => e);
    expect(describeActionError(err)).toBe('This creator lock is not of your wallet key.');
  });
});
