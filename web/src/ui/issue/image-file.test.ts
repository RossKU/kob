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

  it('refuses an SVG (by type, by name and by content), an empty file, a larger one and a file that is not a picture', async () => {
    const svg = '<svg xmlns="http://www.w3.org/2000/svg"/>';
    expect(await hashImageFile(file(svg, 'a.svg', 'image/svg+xml'))).toEqual({ ok: false, reason: 'svg' });
    expect(await hashImageFile(file(PNG, 'a.SVG', 'image/png'))).toEqual({ ok: false, reason: 'svg' });
    expect(await hashImageFile(file(svg, 'a.png', 'image/png'))).toEqual({ ok: false, reason: 'type' });
    expect(await hashImageFile(file(new Uint8Array(), 'a.png'))).toEqual({ ok: false, reason: 'empty' });
    expect(await hashImageFile(file(new Uint8Array(1_500_001).fill(0x89)))).toEqual({ ok: false, reason: 'size' });
    expect(await hashImageFile(file('plain text', 'a.txt', 'text/plain'))).toEqual({ ok: false, reason: 'type' });
  });
});
