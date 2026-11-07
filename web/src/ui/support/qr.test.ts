import { describe, expect, it } from 'vitest';
import { QR_QUIET_ZONE, qrPath } from './qr';

/** The path back into a grid of modules. */
function grid(size: number, path: string): boolean[][] {
  const g = Array.from({ length: size }, () => new Array<boolean>(size).fill(false));
  for (const m of path.matchAll(/M(\d+) (\d+)h(\d+)v1h-(\d+)z/g)) {
    const [x, y, w, back] = m.slice(1).map(Number) as [number, number, number, number];
    expect(back).toBe(w);
    for (let i = 0; i < w; i++) g[y]![x + i] = true;
  }
  return g;
}

// made-up, of the right shape: not a real address
const ADDRESS = `kaspa:${'q'.repeat(61)}`;

describe('qrPath', () => {
  it('the path is nothing but runs of whole modules', () => {
    expect(qrPath(ADDRESS).path).toMatch(/^(M\d+ \d+h\d+v1h-\d+z)+$/);
  });

  it('a mainnet address fits version 5, the longer form too: 37 modules and the quiet zone on each side', () => {
    expect(qrPath(ADDRESS).size).toBe(37 + 2 * QR_QUIET_ZONE);
    expect(qrPath(`kaspa:${'q'.repeat(63)}`).size).toBe(37 + 2 * QR_QUIET_ZONE);
  });

  it('keeps the 4-module quiet zone empty on every side', () => {
    expect(QR_QUIET_ZONE).toBe(4);
    const { size, path } = qrPath(ADDRESS);
    const g = grid(size, path);
    for (let y = 0; y < size; y++) {
      for (let x = 0; x < size; x++) {
        if (x < 4 || y < 4 || x >= size - 4 || y >= size - 4) expect(g[y]![x], `${x},${y}`).toBe(false);
      }
    }
  });

  it('has the three finder patterns where a reader looks for them', () => {
    const { size, path } = qrPath(ADDRESS);
    const g = grid(size, path);
    const q = QR_QUIET_ZONE;
    for (const [ox, oy] of [[q, q], [size - q - 7, q], [q, size - q - 7]] as const) {
      for (let y = 0; y < 7; y++) {
        for (let x = 0; x < 7; x++) {
          const ring = x === 0 || y === 0 || x === 6 || y === 6;
          const centre = x >= 2 && x <= 4 && y >= 2 && y <= 4;
          expect(g[oy + y]![ox + x], `${ox + x},${oy + y}`).toBe(ring || centre);
        }
      }
    }
  });

  it('different text, different code', () => {
    expect(qrPath(ADDRESS).path).not.toBe(qrPath(`kaspa:${'p'.repeat(61)}`).path);
  });
});
