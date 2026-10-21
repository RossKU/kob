import { describe, expect, it } from 'vitest';
import { InsufficientFunds, MAX_FUNDING_INPUTS, MAX_SPARE_TOKEN_INPUTS, coverTokens, selectFunding, selectTokens } from './funding';
import { KAS, keyUtxo, market3x3, market8x8, marketKron, tokenUtxo } from '../testing/fixtures';

const amounts = (u: { amount: string }[]): bigint[] => u.map((x) => BigInt(x.amount));

describe('selectFunding', () => {
  it('takes the largest UTXOs first: fewest inputs', () => {
    const u = [keyUtxo(5n * KAS), keyUtxo(50n * KAS), keyUtxo(20n * KAS), keyUtxo(1n * KAS)];
    expect(amounts(selectFunding(u, 10n * KAS, 0n))).toEqual([50n * KAS]);
    expect(amounts(selectFunding(u, 60n * KAS, 0n))).toEqual([50n * KAS, 20n * KAS]);
    expect(amounts(selectFunding(u, 75n * KAS, 0n))).toEqual([50n * KAS, 20n * KAS, 5n * KAS]);
  });

  it('adds the fee reserve to the need, exactly at the boundary', () => {
    const u = [keyUtxo(10n * KAS)];
    expect(selectFunding(u, 10n * KAS - 5_000_000n, 5_000_000n)).toHaveLength(1);
    expect(() => selectFunding(u, 10n * KAS - 4_999_999n, 5_000_000n)).toThrow(InsufficientFunds);
    // default reserve is 0.05 KAS
    expect(() => selectFunding(u, 10n * KAS)).toThrow(InsufficientFunds);
    expect(selectFunding(u, 10n * KAS - 5_000_000n)).toHaveLength(1);
  });

  it('reports a clear shortfall', () => {
    const u = [keyUtxo(3n * KAS), keyUtxo(2n * KAS)];
    try {
      selectFunding(u, 10n * KAS, 0n);
      expect.unreachable();
    } catch (e) {
      const err = e as InsufficientFunds;
      expect(err).toBeInstanceOf(InsufficientFunds);
      expect(err.kind).toBe('kas');
      expect(err.needed).toBe(10n * KAS);
      expect(err.have).toBe(5n * KAS);
      expect(err.shortfall).toBe(5n * KAS);
      expect(err.message).toMatch(/short by 500000000/);
    }
    expect(() => selectFunding([], 1n, 0n)).toThrow(InsufficientFunds);
  });

  it('always returns an input even when nothing is needed (it authorises the genesis)', () => {
    expect(selectFunding([keyUtxo(KAS)], 0n, 0n)).toHaveLength(1);
  });

  it('never spends covenant outputs as funding and is deterministic on ties', () => {
    const cov = { ...keyUtxo(100n * KAS), covenantId: 'ab'.repeat(32) };
    const a = keyUtxo(4n * KAS);
    const b = keyUtxo(4n * KAS);
    expect(() => selectFunding([cov], KAS, 0n)).toThrow(InsufficientFunds);
    expect(selectFunding([b, cov, a], KAS, 0n)[0]).toBe(a); // smaller txid first
    expect(selectFunding([a, b], KAS, 0n)[0]).toBe(a);
  });

  it('a balance spread over too many UTXOs is fragmented, not short', () => {
    const many = Array.from({ length: MAX_FUNDING_INPUTS + 1 }, () => keyUtxo(KAS));
    try {
      selectFunding(many, BigInt(MAX_FUNDING_INPUTS + 1) * KAS, 0n);
      expect.unreachable();
    } catch (e) {
      expect((e as InsufficientFunds).kind).toBe('fragmented');
    }
    expect(selectFunding(many, BigInt(MAX_FUNDING_INPUTS) * KAS, 0n)).toHaveLength(MAX_FUNDING_INPUTS);
  });
});

describe('coverTokens (the covering set selectTokens starts from)', () => {
  const m = market3x3();
  const t = (wholeTokens: bigint) => tokenUtxo(m, wholeTokens * m.scale);
  const units = (u: { state: { amount: string } }[]) => u.map((x) => BigInt(x.state.amount));

  it('prefers the smallest single UTXO that suffices (least change)', () => {
    const u = [t(50n), t(12n), t(30n)];
    expect(units(coverTokens(u, 10n * m.scale, 3))).toEqual([12n * m.scale]);
    expect(units(coverTokens(u, 12n * m.scale, 3))).toEqual([12n * m.scale]); // exact match
    expect(units(coverTokens(u, 13n * m.scale, 3))).toEqual([30n * m.scale]);
  });

  it('combines the largest UTXOs when no single one suffices, within the slot limit', () => {
    const u = [t(10n), t(8n), t(6n), t(1n)];
    expect(units(coverTokens(u, 15n * m.scale, 3))).toEqual([10n * m.scale, 8n * m.scale]);
    expect(units(coverTokens(u, 24n * m.scale, 3))).toEqual([10n * m.scale, 8n * m.scale, 6n * m.scale]);
  });

  it('throws tokens-shortfall or fragmented (balance enough, too many inputs)', () => {
    const u = [t(10n), t(8n), t(6n), t(1n)];
    try {
      coverTokens(u, 26n * m.scale, 8);
      expect.unreachable();
    } catch (e) {
      const err = e as InsufficientFunds;
      expect(err.kind).toBe('tokens');
      expect(err.shortfall).toBe(1n * m.scale);
    }
    try {
      coverTokens(u, 24n * m.scale, 2);
      expect.unreachable();
    } catch (e) {
      const err = e as InsufficientFunds;
      expect(err.kind).toBe('fragmented');
      expect(err.maxInputs).toBe(2);
    }
  });

  it('ignores custody-owned UTXOs and rejects a non-positive amount', () => {
    const custody = { ...t(100n), state: { ...t(100n).state, owner_scheme: 4 } };
    expect(() => coverTokens([custody], 1n, 3)).toThrow(InsufficientFunds);
    expect(() => coverTokens([t(1n)], 0n, 3)).toThrow(RangeError);
  });
});

describe('selectTokens: the covering set, then the spare slots take the smallest other UTXOs', () => {
  const m = market3x3();
  const t = (wholeTokens: bigint) => tokenUtxo(m, wholeTokens * m.scale);
  const units = (u: { state: { amount: string } }[]) => u.map((x) => BigInt(x.state.amount) / m.scale);

  it('need 100 of [1, 1, 1, 200]: still the 200 (what placed before places), the two spare slots take two of the 1s', () => {
    const u = [t(1n), t(200n), t(1n), t(1n)];
    expect(units(selectTokens(u, 100n * m.scale, 3))).toEqual([200n, 1n, 1n]);
    expect(units(selectTokens(u, 100n * m.scale, 3, null, { spare: false }))).toEqual([200n]);
  });

  it('the covering set first, then the smallest others ascending (ties by outpoint)', () => {
    const u = [t(50n), t(12n), t(30n), t(5n)];
    // 10: cover [12], then 5, 30 (not 50)
    expect(units(selectTokens(u, 10n * m.scale, 3))).toEqual([12n, 5n, 30n]);
    // 60: cover [50, 30] (largest first), then the 5
    expect(units(selectTokens(u, 60n * m.scale, 3))).toEqual([50n, 30n, 5n]);
    const [x, y] = [t(2n), t(2n)];
    const first = x.transactionId < y.transactionId ? x : y;
    expect(selectTokens([t(9n), y, x], 8n * m.scale, 3)[1]).toBe(first);
  });

  it('already at the slot limit: nothing added', () => {
    const u = [t(10n), t(8n), t(6n), t(1n)];
    expect(units(selectTokens(u, 24n * m.scale, 3))).toEqual([10n, 8n, 6n]);
    expect(units(selectTokens(u, 10n * m.scale, 1))).toEqual([10n]);
  });

  it('one extra next to an exact cover is not taken (it would come back as the same UTXO); two are', () => {
    expect(units(selectTokens([t(12n), t(5n)], 12n * m.scale, 3))).toEqual([12n]);
    expect(units(selectTokens([t(12n), t(5n), t(7n)], 12n * m.scale, 3))).toEqual([12n, 5n, 7n]);
    // a cover with change takes a single extra (it joins the change)
    expect(units(selectTokens([t(12n), t(5n)], 11n * m.scale, 3))).toEqual([12n, 5n]);
  });

  it('only usable UTXOs are added (no custody), the fill stops at MAX_SPARE_TOKEN_INPUTS even on a larger program', () => {
    const custody = { ...t(1n), state: { ...t(1n).state, owner_scheme: 4 } };
    expect(units(selectTokens([t(12n), custody, t(3n)], 10n * m.scale, 3))).toEqual([12n, 3n]);
    const m8 = market8x8();
    const many = Array.from({ length: 8 }, (_, i) => tokenUtxo(m8, BigInt(i + 1) * m8.scale));
    expect(MAX_SPARE_TOKEN_INPUTS).toBe(3);
    expect(selectTokens(many, 8n * m8.scale, 8)).toHaveLength(3);
    // the covering set itself may still use every slot: 33 = 8 + 7 + 6 + 5 + 4 + 3
    expect(selectTokens(many, 33n * m8.scale, 8)).toHaveLength(6);
  });

  it('the change stays under a cap (KRON: one output holds at most 10^9 base units)', () => {
    const k = marketKron();
    const u = [tokenUtxo(k, 600_000_000n), tokenUtxo(k, 500_000_000n), tokenUtxo(k, 300_000_000n)];
    // 100M: the cover is the 300M (change 200M); + 500M = 700M fits, + 600M would be 1.3 * 10^9
    expect(selectTokens(u, 100_000_000n, 3, k.program).map((x) => x.state.amount)).toEqual(['300000000', '500000000']);
    expect(selectTokens(u, 100_000_000n, 3, null, { changeCap: 400_000_000n }).map((x) => x.state.amount)).toEqual(['300000000']);
  });
});
