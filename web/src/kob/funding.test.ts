import { describe, expect, it } from 'vitest';
import { InsufficientFunds, MAX_FUNDING_INPUTS, selectFunding, selectTokens } from './funding';
import { KAS, keyUtxo, market3x3, tokenUtxo } from '../testing/fixtures';

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

describe('selectTokens', () => {
  const m = market3x3();
  const t = (wholeTokens: bigint) => tokenUtxo(m, wholeTokens * m.scale);
  const units = (u: { state: { amount: string } }[]) => u.map((x) => BigInt(x.state.amount));

  it('prefers the smallest single UTXO that suffices (least change)', () => {
    const u = [t(50n), t(12n), t(30n)];
    expect(units(selectTokens(u, 10n * m.scale, 3))).toEqual([12n * m.scale]);
    expect(units(selectTokens(u, 12n * m.scale, 3))).toEqual([12n * m.scale]); // exact match
    expect(units(selectTokens(u, 13n * m.scale, 3))).toEqual([30n * m.scale]);
  });

  it('combines the largest UTXOs when no single one suffices, within the slot limit', () => {
    const u = [t(10n), t(8n), t(6n), t(1n)];
    expect(units(selectTokens(u, 15n * m.scale, 3))).toEqual([10n * m.scale, 8n * m.scale]);
    expect(units(selectTokens(u, 24n * m.scale, 3))).toEqual([10n * m.scale, 8n * m.scale, 6n * m.scale]);
  });

  it('throws tokens-shortfall or fragmented (balance enough, too many inputs)', () => {
    const u = [t(10n), t(8n), t(6n), t(1n)];
    try {
      selectTokens(u, 26n * m.scale, 8);
      expect.unreachable();
    } catch (e) {
      const err = e as InsufficientFunds;
      expect(err.kind).toBe('tokens');
      expect(err.shortfall).toBe(1n * m.scale);
    }
    try {
      selectTokens(u, 24n * m.scale, 2);
      expect.unreachable();
    } catch (e) {
      const err = e as InsufficientFunds;
      expect(err.kind).toBe('fragmented');
      expect(err.maxInputs).toBe(2);
    }
  });

  it('ignores custody-owned UTXOs and rejects a non-positive amount', () => {
    const custody = { ...t(100n), state: { ...t(100n).state, owner_scheme: 4 } };
    expect(() => selectTokens([custody], 1n, 3)).toThrow(InsufficientFunds);
    expect(() => selectTokens([t(1n)], 0n, 3)).toThrow(RangeError);
  });
});
