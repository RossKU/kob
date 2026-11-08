// The Create page's picture file: checked like the app checks pictures elsewhere, then hashed (ui/issue/image-file.ts).
import { createHash } from 'node:crypto';
import { describe, expect, it } from 'vitest';
import { hashImageFile } from './image-file';

const PNG = new Uint8Array([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 1, 2, 3, 4]);
const file = (bytes: Uint8Array | string, name = 'a.png', type = 'image/png') => Object.assign(new Blob([bytes as BlobPart], { type }), { name });

describe('hashImageFile', () => {
  it('returns the SHA-256 of the exact bytes as 64 lowercase hex', async () => {
    const r = await hashImageFile(file(PNG));
    expect(r).toEqual({ ok: true, hash: createHash('sha256').update(PNG).digest('hex'), bytes: PNG.length });
    expect(r.ok && /^[0-9a-f]{64}$/.test(r.hash)).toBe(true);
  });

  it('accepts JPEG, WebP and GIF', async () => {
    for (const bytes of [
      new Uint8Array([0xff, 0xd8, 0xff, 0xe0, 0]),
      new Uint8Array([0x52, 0x49, 0x46, 0x46, 0, 0, 0, 0, 0x57, 0x45, 0x42, 0x50, 0]),
      new TextEncoder().encode('GIF89a....'),
    ]) expect((await hashImageFile(file(bytes, 'x.bin', ''))).ok).toBe(true);
  });

  it('accepts a plain SVG and the other formats the app draws; the hash is that of the file as picked', async () => {
    const svg = new TextEncoder().encode('<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 2 2"><circle cx="1" cy="1" r="1" fill="#0a6"/></svg>');
    expect(await hashImageFile(file(svg, 'a.svg', 'image/svg+xml'))).toEqual({ ok: true, hash: createHash('sha256').update(svg).digest('hex'), bytes: svg.length });
    const avif = new Uint8Array([0, 0, 0, 16, 0x66, 0x74, 0x79, 0x70, 0x61, 0x76, 0x69, 0x66, 0, 0, 0, 0]);
    expect((await hashImageFile(file(avif, 'a.avif', 'image/avif'))).ok).toBe(true);
  });

  it('refuses a picture that carries code: an SVG with a script or a link out, a raster file with HTML in it', async () => {
    for (const s of ['<svg><script>alert(1)</script></svg>', '<svg onload="x()"/>', '<svg><image href="https://e.example/a.png"/></svg>', '<svg><a href="javascript:x()"/></svg>', '<!DOCTYPE svg [<!ENTITY a "b">]><svg/>']) {
      expect(await hashImageFile(file(s, 'a.svg', 'image/svg+xml')), s).toEqual({ ok: false, reason: 'code' });
    }
    expect(await hashImageFile(file(new Uint8Array([...PNG, ...new TextEncoder().encode('<html><script>x</script>')])))).toEqual({ ok: false, reason: 'code' });
  });

  it('refuses an empty file, a larger one and a file that is not a picture', async () => {
    expect(await hashImageFile(file(new Uint8Array(), 'a.png'))).toEqual({ ok: false, reason: 'empty' });
    expect(await hashImageFile(file(new Uint8Array(1_500_001).fill(0x89)))).toEqual({ ok: false, reason: 'size' });
    expect(await hashImageFile(file('plain text', 'a.txt', 'text/plain'))).toEqual({ ok: false, reason: 'type' });
  });
});
