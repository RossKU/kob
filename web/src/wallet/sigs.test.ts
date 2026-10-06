import { describe, expect, it } from 'vitest';
import {
  bytesToHex, collectSignatures, errText, extractSignature, hexToBytes, isUserRejection, normalizePubkey, parsePushes, toWalletError, withTimeout,
} from './sigs';
import { WalletError } from './types';
import { pushData } from '../testing/fake-wallet-providers';
import type { BuiltTx } from '../kob/types';

const SIG64 = 'ab'.repeat(64);
const SIG65 = SIG64 + '01';
const txWith = (...scripts: string[]) => ({ version: 1, inputs: scripts.map((signatureScript) => ({ signatureScript })), outputs: [] });
const push = (hex: string) => pushData(hexToBytes(hex));

describe('parsePushes', () => {
  it('reads direct pushes, PUSHDATA1/2/4 and OP_0', () => {
    const big = '77'.repeat(300);
    const script = hexToBytes('00' + '01aa' + '4c02bbcc' + '4d0300ddeeff' + '4e02000000a1a2' + '51' + push(big));
    const p = parsePushes(script).map(bytesToHex);
    expect(p).toEqual(['', 'aa', 'bbcc', 'ddeeff', 'a1a2', big]);
  });

  it('skips non-push opcodes and never throws on truncated data', () => {
    expect(parsePushes(hexToBytes('51ac02aabb')).map(bytesToHex)).toEqual(['aabb']);
    expect(parsePushes(hexToBytes('05aabb'))).toEqual([]); // claims 5 bytes, has 2
    expect(parsePushes(hexToBytes('4c'))).toEqual([]);
    expect(parsePushes(hexToBytes('4d01'))).toEqual([]);
    expect(parsePushes(hexToBytes('4e010000'))).toEqual([]);
    expect(parsePushes(new Uint8Array(0))).toEqual([]);
  });

  it('a push of 0x4b bytes and one of 0x4c bytes round-trip through the encoder', () => {
    for (const n of [0x4b, 0x4c, 0xff, 0x100, 0x1234]) {
      const hex = '5a'.repeat(n);
      expect(parsePushes(hexToBytes(push(hex))).map(bytesToHex), String(n)).toEqual([hex]);
    }
  });
});

describe('extractSignature: every wallet shape of RESULTS.md', () => {
  it('KasWare / Kastle: push(sig65) = 66 bytes', () => {
    expect(extractSignature(JSON.stringify(txWith('41' + SIG65)), 0)).toBe(SIG65);
  });

  it('Kaspire wrap-signature: <sig65><redeem> (redeem is large and may contain 65-byte-looking data after the signature)', () => {
    const redeem = '11'.repeat(6000);
    expect(extractSignature(txWith(push(SIG65) + push(redeem)), 0)).toBe(SIG65);
  });

  it('Kaspire ordered-args: args, sig, tag, redeem: the first 65-byte push wins', () => {
    const script = push('01'.repeat(16)) + push('02'.repeat(32)) + push(SIG65) + push('a0893109') + push('33'.repeat(500));
    expect(extractSignature(txWith(script), 0)).toBe(SIG65);
  });

  it('KCC-20 owner witness 0x00 || sig65 (66-byte push)', () => {
    expect(extractSignature(txWith(push('00' + SIG65) + push('deadbeef')), 0)).toBe(SIG65);
  });

  it('a bare 64-byte signature', () => {
    expect(extractSignature(txWith(push(SIG64)), 0)).toBe(SIG64);
  });

  it('prefers a 65-byte push over the 66-byte witness form when both appear', () => {
    expect(extractSignature(txWith(push('00' + 'cd'.repeat(65)) + push(SIG65)), 0)).toBe(SIG65);
  });

  it('accepts the tx as string or object, wrapped or not, and picks the requested input', () => {
    const tx = txWith('', '41' + SIG65);
    expect(extractSignature(tx, 1)).toBe(SIG65);
    expect(extractSignature({ psktTransactionJson: JSON.stringify(tx) }, 1)).toBe(SIG65);
    expect(extractSignature({ result: { transaction: tx } }, 1)).toBe(SIG65);
  });

  it('no-signature errors: empty script (Kastle #353), unreadable, no signature-sized push, out of range, not a tx', () => {
    const cases: [unknown, number][] = [
      [txWith(''), 0], [txWith('zz'), 0], [txWith(push('aa'.repeat(10))), 0], [txWith('41' + SIG65), 3], ['not json', 0], [null, 0], [{}, 0], [{ inputs: [{}] }, 0],
    ];
    for (const [tx, i] of cases) {
      let err: unknown;
      try {
        extractSignature(tx, i);
      } catch (e) {
        err = e;
      }
      expect(err, JSON.stringify(tx)?.slice(0, 40)).toBeInstanceOf(WalletError);
      expect((err as WalletError).code).toBe('no-signature');
    }
  });
});

describe('collectSignatures', () => {
  const built = { sign: [{ inputIndex: 0 }, { inputIndex: 2 }] } as unknown as BuiltTx;

  it('returns one signature per sign request in order', () => {
    const tx = txWith('41' + SIG65, '', '41' + 'cd'.repeat(64) + '01');
    expect(collectSignatures(built, tx, 'W')).toEqual([{ inputIndex: 0, signature: SIG65 }, { inputIndex: 2, signature: 'cd'.repeat(64) + '01' }]);
  });

  it('names the wallet when a signature is missing', () => {
    expect(() => collectSignatures(built, txWith('41' + SIG65), 'Kastle')).toThrow(/Kastle: .*input 2/);
  });

  it('refuses a signature made with another hash type (it would sign different data than was confirmed)', () => {
    const tx = txWith('41' + SIG64 + '02', '', '41' + SIG65);
    expect(() => collectSignatures(built, tx, 'W')).toThrow(/hash type \(02\)/);
    // a 64-byte signature carries no hash type byte: accepted here, finalize checks it against the digest
    expect(collectSignatures({ sign: [{ inputIndex: 0 }] } as unknown as BuiltTx, txWith(push(SIG64)), 'W')[0]!.signature).toBe(SIG64);
  });
});

describe('errors', () => {
  it('errText renders {code, message} objects, Errors and strings', () => {
    expect(errText({ code: 4001, message: 'User rejected' })).toBe('code 4001: User rejected');
    expect(errText(new Error('boom'))).toBe('boom');
    expect(errText('plain')).toBe('plain');
    expect(errText({ message: 'only message' })).toBe('only message');
  });

  it('isUserRejection recognises codes and wording, and not unrelated failures', () => {
    for (const e of [
      { code: 4001, message: 'x' }, { code: 'ACTION_REJECTED' }, new Error('User rejected the request'), 'Request was rejected by user', new Error('Transaction declined'),
      'user denied', 'Cancelled by user', 'The user closed the window', new WalletError('rejected', 'x'),
    ]) expect(isUserRejection(e), errText(e)).toBe(true);
    for (const e of [new Error('network timeout'), 'insufficient funds', { code: -32603, message: 'Internal error' }, new WalletError('timeout', 'x')]) {
      expect(isUserRejection(e), errText(e)).toBe(false);
    }
  });

  it('toWalletError keeps WalletErrors, maps rejections and wraps the rest with the wallet name', () => {
    const w = new WalletError('timeout', 't');
    expect(toWalletError(w, 'KasWare')).toBe(w);
    expect(toWalletError({ code: 4001, message: 'no' }, 'KasWare')).toMatchObject({ code: 'rejected' });
    expect(toWalletError(new Error('kaboom'), 'Kaspire')).toMatchObject({ code: 'other', message: 'Kaspire: kaboom' });
  });

  it('withTimeout resolves in time, rejects with a timeout WalletError, and passes rejections through', async () => {
    expect(await withTimeout(Promise.resolve(7), 50, 'x')).toBe(7);
    await expect(withTimeout(new Promise(() => undefined), 15, 'KasWare.signPskt')).rejects.toMatchObject({ name: 'WalletError', code: 'timeout', message: expect.stringContaining('KasWare.signPskt') });
    await expect(withTimeout(Promise.reject(new Error('inner')), 50, 'x')).rejects.toThrow('inner');
  });
});

describe('normalizePubkey', () => {
  const X = '462779ad4aad39514614751a71085f2f10e1c7a593e4e030efb5b8721ce55b0b';
  it('accepts x-only, 33-byte compressed (Kastle: last 32 bytes) and 0x-prefixed keys, case-insensitively', () => {
    expect(normalizePubkey(X)).toBe(X);
    expect(normalizePubkey('02' + X)).toBe(X);
    expect(normalizePubkey('03' + X.toUpperCase())).toBe(X);
    expect(normalizePubkey('0x' + X)).toBe(X);
  });
  it('refuses anything else', () => {
    for (const bad of [undefined, 5, '', 'zz', X.slice(2), '02' + X + '00', 'gg' + X.slice(2)]) expect(() => normalizePubkey(bad), String(bad)).toThrow(WalletError);
  });
});
