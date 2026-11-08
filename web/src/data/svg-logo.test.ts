import { describe, expect, it } from 'vitest';
import { isSvg } from './svg-logo';

const enc = (s: string) => new TextEncoder().encode(s);

describe('isSvg', () => {
  it('an SVG document: after a BOM, an XML declaration, comments and a DOCTYPE', () => {
    for (const s of [
      '<svg xmlns="http://www.w3.org/2000/svg"/>',
      '﻿<svg viewBox="0 0 1 1"></svg>',
      '<?xml version="1.0" encoding="UTF-8"?>\n<!-- made by hand -->\n<!DOCTYPE svg PUBLIC "-//W3C//DTD SVG 1.1//EN" "x">\n<SVG>',
      '  \n<svg\n  width="1">',
    ]) {
      expect(isSvg(enc(s)), s).toBe(true);
    }
  });

  it('anything else: HTML, text, a raster file, an element that only starts with svg', () => {
    for (const s of ['<html><svg/></html>', 'hello', '<svgx/>', '<!-- open', '<?xml version="1.0"?><html/>', '']) expect(isSvg(enc(s)), s).toBe(false);
    expect(isSvg(new Uint8Array([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]))).toBe(false);
  });
});
