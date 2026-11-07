import { describe, expect, it } from 'vitest';
import { contrast, luminance, MIN_TEXT_CONTRAST, over, parseColor, textContrast } from './contrast';

const black = { r: 0, g: 0, b: 0, a: 1 };
const white = { r: 255, g: 255, b: 255, a: 1 };

describe('parseColor', () => {
  it('reads what a stylesheet and getComputedStyle write', () => {
    expect(parseColor('#fff')).toEqual(white);
    expect(parseColor('#000000')).toEqual(black);
    expect(parseColor('#00000080')?.a).toBeCloseTo(0.502, 3);
    expect(parseColor('rgb(1, 2, 3)')).toEqual({ r: 1, g: 2, b: 3, a: 1 });
    expect(parseColor('rgba(1, 2, 3, 0.5)')).toEqual({ r: 1, g: 2, b: 3, a: 0.5 });
    expect(parseColor('rgb(1 2 3 / 50%)')).toEqual({ r: 1, g: 2, b: 3, a: 0.5 });
    expect(parseColor('transparent')?.a).toBe(0);
  });

  it('reads the colour a browser computes a color-mix() to', () => {
    expect(parseColor('color(srgb 1 0 0.5)')).toEqual({ r: 255, g: 0, b: 127.5, a: 1 });
    expect(parseColor('color(srgb 0 0 0 / 0.25)')?.a).toBe(0.25);
  });

  it('says nothing about a colour it cannot read', () => {
    for (const s of ['', 'red', '#12', 'hsl(0 0% 0%)', 'color(display-p3 1 0 0)', 'rgb(1, 2)', 'rgb(a, b, c)']) expect(parseColor(s), s).toBeNull();
  });
});

describe('contrast', () => {
  it('follows WCAG 2: the luminance of black and white, 21:1 between them, either way round', () => {
    expect(luminance(black)).toBe(0);
    expect(luminance(white)).toBeCloseTo(1, 10);
    expect(contrast(black, white)).toBeCloseTo(21, 10);
    expect(contrast(white, black)).toBeCloseTo(21, 10);
    expect(contrast(white, white)).toBe(1);
  });

  it('agrees with the published figures: #767676 is the lightest gray that passes on white, #777777 does not', () => {
    expect(contrast(parseColor('#767676')!, white)).toBeCloseTo(4.54, 2);
    expect(contrast(parseColor('#767676')!, white)).toBeGreaterThanOrEqual(MIN_TEXT_CONTRAST);
    expect(contrast(parseColor('#777777')!, white)).toBeLessThan(MIN_TEXT_CONTRAST);
  });

  it('paints a see-through colour over what is under it', () => {
    expect(over({ r: 0, g: 0, b: 0, a: 0.5 }, white)).toEqual({ r: 127.5, g: 127.5, b: 127.5, a: 1 });
    expect(over({ r: 9, g: 9, b: 9, a: 0 }, white)).toEqual(white);
    expect(over({ r: 9, g: 9, b: 9, a: 0 }, { r: 1, g: 1, b: 1, a: 0 }).a).toBe(0);
  });
});

describe('textContrast', () => {
  it('stacks the backgrounds, nearest first, on a white canvas', () => {
    expect(textContrast('#000', [])).toBeCloseTo(21, 10);
    expect(textContrast('#fff', ['rgba(0, 0, 0, 0)', 'rgb(0, 0, 0)'])).toBeCloseTo(21, 10);
    expect(textContrast('#000', ['rgba(0, 0, 0, 0.5)', '#fff'])).toBeCloseTo(contrast(black, { r: 127.5, g: 127.5, b: 127.5, a: 1 }), 10);
  });

  it('counts a see-through text as what it paints', () => {
    expect(textContrast('rgba(0, 0, 0, 0)', ['#fff'])).toBe(1);
  });

  it('finds the bug of the Rows control: the checked text on the hover background, in both themes', () => {
    expect(textContrast('#03211d', ['#162322'])).toBeLessThan(2);
    expect(textContrast('#ffffff', ['#e2e1d9'])).toBeLessThan(2);
  });

  it('gives NaN, which no check passes, for a colour it cannot read', () => {
    expect(textContrast('hsl(0 0% 0%)', ['#fff'])).toBeNaN();
    expect(textContrast('#000', ['nope'])).toBeNaN();
    expect(textContrast('#000', ['nope']) >= MIN_TEXT_CONTRAST).toBe(false);
  });
});
