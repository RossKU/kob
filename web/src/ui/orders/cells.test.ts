import { describe, expect, it } from 'vitest';
import { amountCell, isUnnamed, longDate, priceDpOf, tokenUnit } from './cells';

const ID = '83478d88aa11bb22cc33dd44ee55ff6600112233445566778899aabb6daacd64';

describe('table amounts (review M-03)', () => {
  it('show at most 4 fraction digits and keep the whole value in the title', () => {
    expect(amountCell(4_500_000_000_000n, 8)).toEqual({ text: '45,000', title: undefined });
    expect(amountCell(196_083_737_795n, 8)).toEqual({ text: '1,960.8374', title: '1,960.83737795' });
    expect(amountCell(150_000_000n, 8)).toEqual({ text: '1.5', title: undefined });
  });

  it('never reads a positive amount as zero, and never uses subscript zeros', () => {
    expect(amountCell(1n, 8).text).toBe('<0.0001');
    expect(amountCell(0n, 8)).toEqual({ text: '0', title: undefined });
    expect(amountCell(1n, 8).text).not.toMatch(/[₀-₉]/);
  });
});

describe('the unit of a token in a table', () => {
  it('is the ticker when there is one', () => {
    expect(tokenUnit('EXKCC', ID)).toEqual({ text: 'EXKCC', title: undefined });
  });

  it('is the short id (never the long hex) in the muted unit slot when the token has no name', () => {
    expect(tokenUnit(`${ID.slice(0, 8)}…${ID.slice(-8)}`, ID)).toEqual({ text: '8347…cd64', title: ID });
    expect(tokenUnit(undefined, ID)).toEqual({ text: '8347…cd64', title: ID });
    expect(isUnnamed('EXKCC', ID)).toBe(false);
  });
});

describe('a price column has one number of decimals', () => {
  const row = (price: bigint) => ({ price, decimals: 8, scale: 10n ** 8n });
  it('is the finest any price of the column needs, at most 6', () => {
    expect(priceDpOf([row(3_341_892_984n)])).toBe(3); // 33.4 KAS: 5 significant digits
    expect(priceDpOf([row(3_341_892_984n), row(2_500_000n)])).toBe(6); // 0.025 KAS needs 6
    expect(priceDpOf([row(1n)])).toBe(6); // capped
  });

  it('leaves out rows without a price', () => {
    expect(priceDpOf([{ price: null, decimals: 8, scale: 10n ** 8n }, row(3_341_892_984n)])).toBe(3);
    expect(priceDpOf([])).toBe(4);
  });
});

describe('a date in a sentence', () => {
  it('is 24-hour with the day always named', () => {
    expect(longDate(Math.floor(Date.now() / 1000))).toMatch(/^\d{4}-\d{2}-\d{2} \d{2}:\d{2}$/);
  });
});
