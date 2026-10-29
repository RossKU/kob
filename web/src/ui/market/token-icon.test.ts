// The colour of a token's letter icon comes from its covenant id, never from its ticker or name: two tokens with one ticker look different, and a token
// keeps its colour.
import { describe, expect, it } from 'vitest';
import { avatarHue, avatarLetter } from './TokenIcon';

const A = '3165ab67'.padEnd(64, '9');
const B = 'c7d2e0a1'.padEnd(64, '5');

describe('letter icon', () => {
  it('the hue is a function of the covenant id alone: the same id gives the same hue, another id another one', () => {
    expect(avatarHue(A)).toBe(avatarHue(A));
    expect(avatarHue(A)).toBeGreaterThanOrEqual(0);
    expect(avatarHue(A)).toBeLessThan(360);
    expect(avatarHue(A)).not.toBe(avatarHue(B));
    // the ticker is not an input at all
    expect(avatarHue.length).toBe(1);
  });

  it('two tokens with the same ticker have the same letter and different colours', () => {
    expect(avatarLetter('TUSD', A)).toBe(avatarLetter('TUSD', B));
    expect(avatarHue(A)).not.toBe(avatarHue(B));
  });

  it('the letter is the first letter or digit of the ticker, else of the id', () => {
    expect(avatarLetter('tusd', A)).toBe('T');
    expect(avatarLetter('$1x', A)).toBe('1');
    expect(avatarLetter('', A)).toBe('3');
  });
});
