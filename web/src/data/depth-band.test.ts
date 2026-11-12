import { describe, expect, it } from 'vitest';
import { depth2pctSides } from './depth-band';

const L = (price: number, amount: number) => ({ price: BigInt(price), amount: BigInt(amount) });

describe('depth2pctSides', () => {
  it('both sides: the reference is the mid, each side counts its levels inside the 2 % band', () => {
    // basis 1: value = amount x price; mid of 1000 and 1020 = 1010, band 989.8 .. 1030.2
    const r = depth2pctSides([L(1020, 3), L(1030, 11), L(1031, 100)], [L(1000, 5), L(990, 7), L(989, 100)], 1n);
    expect(r).toEqual({ ref: 1010n, bid: 5n * 1000n + 7n * 990n, ask: 3n * 1020n + 11n * 1030n });
  });
  it('one side: the reference is that side\'s best price and the other side is 0', () => {
    expect(depth2pctSides([], [L(1000, 1), L(980, 2), L(979, 4)], 1n)).toEqual({ ref: 1000n, bid: 1000n + 1960n, ask: 0n });
    expect(depth2pctSides([L(1000, 1), L(1020, 2), L(1021, 4)], [], 1n)).toEqual({ ref: 1000n, bid: 0n, ask: 1000n + 2040n });
  });
  it('an empty book (or a price that is not positive) has no reference', () => {
    expect(depth2pctSides([], [], 1n)).toBeNull();
    expect(depth2pctSides([L(0, 5)], [], 1n)).toBeNull();
  });
  it('exactly 2 % counts, one sompi beyond does not (reference 10 000)', () => {
    expect(depth2pctSides([L(10_200, 1), L(10_201, 1)], [L(10_000, 1)], 1n)).toMatchObject({ ref: 10_100n });
    expect(depth2pctSides([L(10_000, 1), L(10_200, 1), L(10_201, 1)], [], 1n)!.ask).toBe(20_200n);
    expect(depth2pctSides([], [L(10_000, 1), L(9_800, 1), L(9_799, 1)], 1n)!.bid).toBe(19_800n);
  });
  it('rounds each level down by the basis, like the indexer', () => {
    expect(depth2pctSides([L(250, 10)], [], 100n)).toEqual({ ref: 250n, bid: 0n, ask: 25n });
    expect(depth2pctSides([L(250, 3)], [], 100n)!.ask).toBe(7n);
  });
});
