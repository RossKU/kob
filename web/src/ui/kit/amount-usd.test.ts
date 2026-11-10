import type { VNode } from 'preact';
import { describe, expect, it } from 'vitest';
import { rateOf, type Fiat } from '../../app/fiat';
import { Amount, type AmountProps } from './Amount';

const fiat: Fiat = { rate: rateOf(0.0298)!, quote: { usd: 0.0298, at: 1_800_000_000, source: 'coingecko' }, testnet: false };
const RATE = 'USD estimate at $0.0298 per KAS';
const node = (p: AmountProps): VNode<Record<string, unknown>> => Amount(p) as VNode<Record<string, unknown>>;
const textOf = (v: VNode<Record<string, unknown>>): string => [v.props.children].flat(3).filter((c) => typeof c === 'string').join('');

describe('Amount with the USD display', () => {
  it('KAS stays KAS without a fiat', () => {
    const v = node({ kind: 'kas', value: 3_580_000_000_000n, group: true });
    expect(textOf(v)).toBe('35,800');
    expect(v.props.title).toBe('35800');
  });

  it('a KAS value is dollars, with the exact KAS figure and the rate in the title, and no unit', () => {
    const v = node({ kind: 'kas', value: 3_580_000_000_000n, group: true, fiat });
    expect(textOf(v)).toBe('$1,066.84');
    expect(v.props.title).toBe(`35,800 KAS · ${RATE}`);
    expect(v.props['data-currency']).toBe('usd');
    expect(JSON.stringify(v.props.children)).not.toContain('KAS');
  });

  it('a signed delta keeps its sign, a missing value stays a dash', () => {
    expect(textOf(node({ kind: 'kas', value: 100_000_000n, signed: true, fiat }))).toBe('+$0.0298');
    expect(textOf(node({ kind: 'kas', value: -100_000_000n, signed: true, fiat }))).toBe('-$0.0298');
    expect(textOf(node({ kind: 'kas', value: null, fiat }))).toBe('—');
  });

  it('a price (sompi per whole token) is dollars per whole token; the title keeps the KAS price with the unit it was given', () => {
    // 2,510,000 sompi per whole token of 8 decimals = 0.0251 KAS per token = $0.00074798
    const v = node({ kind: 'price', value: 2_510_000n, decimals: 8, scale: 100_000_000n, unit: 'KAS per EXKCC', fiat });
    expect(textOf(v)).toBe('$0.000748');
    expect(v.props.title).toBe(`0.0251 KAS per EXKCC · ${RATE}`);
  });

  it('a compact width turns a large amount into $56.61M and puts the exact dollars first in the title', () => {
    // 1,899,000,000 KAS x 0.0298 = $56,590,200
    const v = node({ kind: 'kas', value: 189_900_000_000_000_000n, fiat, width: 9 });
    expect(textOf(v)).toBe('$56.59M');
    expect(String(v.props.title).startsWith('$56,590,200.00 · ')).toBe(true);
  });

  it('a token amount is never converted', () => {
    const v = node({ kind: 'token', value: 150_000_000n, decimals: 8, unit: 'EXKCC', fiat });
    expect(textOf(v)).toBe('1.5');
    expect(v.props['data-currency']).toBeUndefined();
  });
});
