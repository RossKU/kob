// Regression test found by the real-wallet TN10 run: the official kaspa-wasm SDK identifies its JS classes by their `name` when it converts
// request objects (rpc.submitTransaction with a covenant output failed with "Error converting property `covenant`" in the MINIFIED browser
// build, while the same SDK worked unminified and in node). The production build must therefore keep class names.
// The build runs in memory (`write: false`): dist/ and dist-real/ are never touched.
import { describe, expect, it } from 'vitest';
import { build } from 'vite';
import { fileURLToPath } from 'node:url';

const root = fileURLToPath(new URL('..', import.meta.url));

describe('production build', () => {
  it('keeps the class names the kaspa SDK relies on (TransactionOutput, Hash ...)', async () => {
    const out = (await build({ root, logLevel: 'silent', build: { write: false, minify: true, sourcemap: false } })) as unknown as
      | { output: { type: string; fileName: string; code?: string }[] }
      | { output: { type: string; fileName: string; code?: string }[] }[];
    const outputs = (Array.isArray(out) ? out : [out]).flatMap((o) => o.output);
    const sdk = outputs.find((o) => o.type === 'chunk' && /class TransactionOutput\b/.test(o.code ?? ''));
    expect(sdk, 'no chunk defines `class TransactionOutput`: a minifier renamed the SDK classes').toBeTruthy();
    const code = sdk!.code!;
    for (const name of ['Transaction', 'TransactionInput', 'Hash', 'ScriptPublicKey', 'RpcClient']) {
      expect(new RegExp(`class ${name}\\b`).test(code), `class ${name} was renamed by the minifier`).toBe(true);
    }
  });
});
