import { createHash, randomBytes } from 'node:crypto';
import { describe, expect, it } from 'vitest';
import { sha256Hex } from './sha256';

describe('sha256Hex', () => {
  it('matches the standard vectors', () => {
    expect(sha256Hex(new Uint8Array())).toBe('e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855');
    expect(sha256Hex(new TextEncoder().encode('abc'))).toBe('ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad');
  });
  it('matches node:crypto for every length around the block boundaries and for big inputs', () => {
    for (const n of [1, 54, 55, 56, 57, 63, 64, 65, 119, 120, 127, 128, 1000, 100_000]) {
      const b = new Uint8Array(randomBytes(n));
      expect(sha256Hex(b), `len ${n}`).toBe(createHash('sha256').update(b).digest('hex'));
    }
  });
});
