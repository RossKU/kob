import { describe, expect, it } from 'vitest';
import { SIGNER_PROTOCOL, fromParent, fromSigner, isBuiltTx, parseAppMessage, parseSignerMessage } from './protocol';
import { buildCancel, buildSend } from '../testing/wallet-fixtures';
import { MAKER_PK } from '../testing/token-fixtures';

const P = SIGNER_PROTOCOL;
const clone = <T>(v: T): T => structuredClone(v);

describe('signer protocol: what the signer accepts from the app', () => {
  it('a sign request with a built transaction, token hints, cancel and forget', () => {
    const built = buildSend();
    const m = parseAppMessage(clone({ p: P, type: 'sign', id: 'r1', built, tokens: [{ covenantId: '70'.repeat(32), ticker: 'EXKCC', decimals: 8 }] }));
    expect(m?.type).toBe('sign');
    expect(parseAppMessage({ p: P, type: 'sign', id: 'r2', built: buildCancel(), tokens: [] })?.type).toBe('sign');
    // the orientation of the token's market is a label too (a boolean or absent)
    expect(parseAppMessage(clone({ p: P, type: 'sign', id: 'r3', built, tokens: [{ covenantId: '70'.repeat(32), ticker: 'EXKCC', decimals: 8, inverted: true }] }))?.type).toBe('sign');
    expect(parseAppMessage({ p: P, type: 'cancel', id: 'r1' })).toEqual({ p: P, type: 'cancel', id: 'r1' });
    expect(parseAppMessage({ p: P, type: 'forget' })).toEqual({ p: P, type: 'forget' });
  });

  it('drops everything else: other protocol, unknown type, extra keys, odd ids, non-objects', () => {
    for (const bad of [
      null, 'forget', 42, [], { type: 'forget' }, { p: 'kob-signer/0', type: 'forget' }, { p: P, type: 'approve', id: 'r1' }, { p: P, type: 'signed', id: 'r1', signatures: [] },
      { p: P, type: 'forget', key: 'x' }, { p: P, type: 'cancel', id: '' }, { p: P, type: 'cancel', id: 'a b' }, { p: P, type: 'cancel', id: 'x'.repeat(65) },
    ]) {
      expect(parseAppMessage(bad), JSON.stringify(bad)).toBeNull();
    }
  });

  it('refuses a malformed sign request: shapes, sighash type, duplicate or out-of-range inputs, bad hints', () => {
    const ok = () => ({ p: P, type: 'sign', id: 'r1', built: clone(buildSend()), tokens: [] as unknown[] });
    const cases: [string, (m: ReturnType<typeof ok>) => void][] = [
      ['extra key', (m) => Object.assign(m, { extra: 1 })],
      ['no built', (m) => Object.assign(m, { built: null })],
      ['sighash type 2', (m) => void (m.built.sign[0]!.sighashType = 2)],
      ['short digest', (m) => void (m.built.sign[0]!.sighash = 'ab')],
      ['upper-case pubkey', (m) => void (m.built.sign[0]!.pubkey = MAKER_PK.toUpperCase())],
      ['input out of range', (m) => void (m.built.sign[0]!.inputIndex = m.built.tx.inputs.length)],
      ['duplicate input', (m) => m.built.sign.push(clone(m.built.sign[0]!))],
      ['no signature requested', (m) => void (m.built.sign = [])],
      ['extra key in a request', (m) => Object.assign(m.built.sign[0]!, { key: 'x' })],
      ['plans do not match inputs', (m) => void m.built.plans.pop()],
      ['bad output value', (m) => void ((m.built.tx.outputs[0] as { value: unknown }).value = -1)],
      ['bad tx id', (m) => void (m.built.tx.id = 'zz')],
      ['bad hint', (m) => void m.tokens.push({ covenantId: '70'.repeat(32), ticker: '<script>'.repeat(4), decimals: 8 })],
      ['hint decimals', (m) => void m.tokens.push({ covenantId: '70'.repeat(32), ticker: 'X', decimals: 40 })],
      ['hint orientation', (m) => void m.tokens.push({ covenantId: '70'.repeat(32), ticker: 'X', decimals: 8, inverted: 'yes' })],
      ['oversized', (m) => void (m.built.tx.payload = '00'.repeat(1_100_000))],
    ];
    expect(parseAppMessage(ok())).not.toBeNull();
    for (const [name, spoil] of cases) {
      const m = ok();
      spoil(m);
      expect(parseAppMessage(m), name).toBeNull();
    }
    expect(isBuiltTx(buildCancel())).toBe(true);
  });
});

describe('signer protocol: what the app accepts from the signer', () => {
  const sig = { inputIndex: 2, signature: 'ab'.repeat(64) + '01' };
  it('ready, connected, signed, rejected, forgotten', () => {
    expect(parseSignerMessage({ p: P, type: 'ready' })).toEqual({ p: P, type: 'ready' });
    expect(parseSignerMessage({ p: P, type: 'connected', pubkey: MAKER_PK })).toEqual({ p: P, type: 'connected', pubkey: MAKER_PK });
    expect(parseSignerMessage({ p: P, type: 'signed', id: 'r1', signatures: [sig] })).toEqual({ p: P, type: 'signed', id: 'r1', signatures: [sig] });
    expect(parseSignerMessage({ p: P, type: 'rejected', id: 'r1', reason: 'user' })?.type).toBe('rejected');
    expect(parseSignerMessage({ p: P, type: 'forgotten' })?.type).toBe('forgotten');
  });
  it('drops malformed answers (a secret-looking field, bad keys, bad signatures, unknown reasons)', () => {
    for (const bad of [
      { p: P, type: 'connected', pubkey: MAKER_PK, secret: '01'.repeat(32) },
      { p: P, type: 'connected', pubkey: '02' + MAKER_PK },
      { p: P, type: 'signed', id: 'r1', signatures: [] },
      { p: P, type: 'signed', id: 'r1', signatures: [{ ...sig, signature: 'ab'.repeat(64) + '02' }] },
      { p: P, type: 'signed', id: 'r1', signatures: [{ ...sig, extra: 1 }] },
      { p: P, type: 'rejected', id: 'r1', reason: 'whatever' },
      { p: P, type: 'sign', id: 'r1' },
    ]) {
      expect(parseSignerMessage(bad), JSON.stringify(bad)).toBeNull();
    }
  });
});

describe('sender checks', () => {
  const parent = {};
  const frame = {};
  it('the signer listens only to its parent, from the app origin', () => {
    expect(fromParent({ source: parent, origin: 'https://app.example' }, parent, 'https://app.example')).toBe(true);
    expect(fromParent({ source: parent, origin: 'https://evil.example' }, parent, 'https://app.example')).toBe(false);
    expect(fromParent({ source: {}, origin: 'https://app.example' }, parent, 'https://app.example')).toBe(false);
    expect(fromParent({ source: null, origin: 'https://app.example' }, null, 'https://app.example')).toBe(false);
  });
  it('the app listens only to its signer frame, whose origin is opaque', () => {
    expect(fromSigner({ source: frame, origin: 'null' }, frame)).toBe(true);
    expect(fromSigner({ source: frame, origin: 'https://app.example' }, frame)).toBe(false); // a same-origin frame is not the sandboxed signer
    expect(fromSigner({ source: {}, origin: 'null' }, frame)).toBe(false);
    expect(fromSigner({ source: null, origin: 'null' }, null)).toBe(false);
  });
});
