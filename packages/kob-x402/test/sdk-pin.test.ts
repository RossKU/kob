// The vendored kaspa-wasm build is hashed when it is loaded and refused unless it is the pinned SDK: a file changed in the vendor copy
// after `fetch-sdk` verified it is never loaded.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { existsSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { loadKaspaNodeSdk, sdkTreeSha256 } from '../src/kaspa.ts';
import { KobX402Error } from '../src/errors.ts';

test('a vendor copy that is not the pinned SDK is refused before it is loaded', () => {
  const dir = mkdtempSync(join(tmpdir(), 'kob-sdk-'));
  try {
    writeFileSync(join(dir, 'kaspa.js'), 'globalThis.__kobSdkLoaded = true; module.exports = {};\n');
    assert.throws(() => loadKaspaNodeSdk(dir), (e: unknown) => e instanceof KobX402Error && /not the pinned SDK/.test(e.message));
    assert.equal((globalThis as { __kobSdkLoaded?: boolean }).__kobSdkLoaded, undefined, 'its code never ran');
    // a build the caller pins explicitly loads; so does an explicitly unpinned one
    assert.deepEqual(loadKaspaNodeSdk(dir, { expectedTreeSha256: sdkTreeSha256(dir) }), {});
    writeFileSync(join(dir, 'kaspa.js'), 'module.exports = { changed: true };\n');
    assert.throws(() => loadKaspaNodeSdk(dir, { expectedTreeSha256: 'ab'.repeat(32) }), KobX402Error);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

const vendored = [
  fileURLToPath(new URL('../../../tools/wallet-gate/vendor/kaspa-node', import.meta.url)),
  fileURLToPath(new URL('../../../web/vendor/kaspa-node', import.meta.url)),
].find((d) => existsSync(join(d, 'kaspa.js')));

test('the pinned SDK fetched by fetch-sdk matches the pin', { skip: vendored ? false : 'no vendored kaspa-node (run fetch-sdk)' }, () => {
  const sdk = loadKaspaNodeSdk(vendored!);
  assert.equal(typeof sdk.RpcClient, 'function');
});
