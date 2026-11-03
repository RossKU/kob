import { describe, expect, it } from 'vitest';
import { SIGNER_PROTOCOL, fromParent, fromSigner, isBuiltTx, MAX_NOTES, parseAppMessage, parseSignerMessage, sanitizeNotes } from './protocol';
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
    // a token shown on its program's default decimals (KaspaRocket convention): no ticker, the convention a short label, with that source only
    const prog = (h: Record<string, unknown>) => parseAppMessage(clone({ p: P, type: 'sign', id: 'r4', built, tokens: [{ covenantId: '70'.repeat(32), ticker: '', decimals: 3, ...h }] }));
    expect(prog({ source: 'program', convention: 'KaspaRocket convention' })?.type).toBe('sign');
    expect(prog({ source: 'program' })).toBeNull();
    expect(prog({ source: 'program', convention: 'x'.repeat(41) })).toBeNull();
    expect(prog({ source: 'program', convention: 'KaspaRocket convention', ticker: 'RKT' })).toBeNull();
    expect(prog({ source: 'assumed', convention: 'KaspaRocket convention' })).toBeNull();
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

describe('signer protocol: updates of the request on screen, the clock, the frame height', () => {
  it('an update carries the same shapes as a request; the clock is a DAA score; the frame says its height and the transaction it signed', () => {
    const built = buildSend();
    expect(parseAppMessage(clone({ p: P, type: 'update', id: 'r1', built, tokens: [] }))?.type).toBe('update');
    expect(parseAppMessage({ p: P, type: 'update', id: 'r1', built: { ...built, sign: [] }, tokens: [] })).toBeNull();
    expect(parseAppMessage({ p: P, type: 'update', id: 'r1', built, tokens: [], retimeOf: 'r0' })).toBeNull();
    expect(parseAppMessage({ p: P, type: 'clock', daa: '1000330' })).toEqual({ p: P, type: 'clock', daa: '1000330' });
    for (const daa of ['', '-1', '1e9', 12, '1'.repeat(21)]) expect(parseAppMessage({ p: P, type: 'clock', daa })).toBeNull();
    expect(parseSignerMessage({ p: P, type: 'height', px: 612 })).toEqual({ p: P, type: 'height', px: 612 });
    for (const px of [-1, 1.5, '600', 1e6]) expect(parseSignerMessage({ p: P, type: 'height', px })).toBeNull();
    const sig = { inputIndex: 0, signature: 'ab'.repeat(64) };
    expect(parseSignerMessage({ p: P, type: 'signed', id: 'r1', txid: built.tx.id, signatures: [sig] })).toEqual({ p: P, type: 'signed', id: 'r1', txid: built.tx.id, signatures: [sig] });
    expect(parseSignerMessage({ p: P, type: 'signed', id: 'r1', txid: 'zz', signatures: [sig] })).toBeNull();
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

describe('signer protocol: a token creation (KCC-23 preparation + genesis)', () => {
  const claim = () => ({
    program: 'KCC20Ref', extensionCommitment: '00'.repeat(32),
    outputs: [{ index: 0, amount: '1000', owner: MAKER_PK, ownerScheme: 0, borrowScheme: 0, borrowGuard: '00'.repeat(32) }],
  });
  it('a sign request carries the preparation and the genesis claim; a signed answer the preparation signatures', () => {
    const m = parseAppMessage(clone({ p: P, type: 'sign', id: 'c1', built: buildSend(), tokens: [], prep: buildCancel(), genesis: claim() }));
    expect(m).toMatchObject({ type: 'sign', id: 'c1', prep: { tx: { id: buildCancel().tx.id } }, genesis: claim() });
    const sig = { inputIndex: 0, signature: 'ab'.repeat(64) };
    expect(parseSignerMessage({ p: P, type: 'signed', id: 'c1', txid: 'cd'.repeat(32), signatures: [sig], prepSignatures: [sig] })).toMatchObject({ prepSignatures: [sig] });
  });
  it('refuses a claim without its preparation, a malformed claim, a re-timed creation, empty or malformed preparation signatures', () => {
    const base = () => ({ p: P, type: 'sign', id: 'c1', built: clone(buildSend()), tokens: [] as unknown[], prep: clone(buildCancel()), genesis: claim() as unknown });
    const bad: ((m: ReturnType<typeof base>) => void)[] = [
      (m) => void delete (m as { prep?: unknown }).prep,
      (m) => void (m.genesis = { ...claim(), program: 'bad name' }),
      (m) => void (m.genesis = { ...claim(), outputs: [] }),
      (m) => void (m.genesis = { ...claim(), outputs: [{ ...claim().outputs[0], amount: '-1' }] }),
      (m) => void (m.genesis = { ...claim(), extra: 1 }),
      (m) => void Object.assign(m, { retimeOf: 'r0' }),
      (m) => void Object.assign(m, { prep: { tx: {} } }),
    ];
    for (const f of bad) {
      const m = base();
      f(m);
      expect(parseAppMessage(m)).toBeNull();
    }
    const sig = { inputIndex: 0, signature: 'ab'.repeat(64) };
    for (const prepSignatures of [[], [{ inputIndex: 0, signature: 'zz' }], 'x']) {
      expect(parseSignerMessage({ p: P, type: 'signed', id: 'c1', signatures: [sig], prepSignatures })).toBeNull();
    }
  });
});

describe("signer protocol: the app's notes (codes and numbers, never text)", () => {
  const sign = (notes: unknown) => parseAppMessage(clone({ p: P, type: 'sign', id: 'n1', built: buildSend(), tokens: [], ...(notes === undefined ? {} : { notes }) })) as { notes?: unknown } | null;
  const below = { kind: 'belowMin', value: '98800000', min: '100000000', source: 'default' };
  const good = { kind: 'farThrough', bps: 1000 };

  it('known kinds with their numbers pass; a safe integer amount is normalised to a decimal string', () => {
    expect(sign([below, { kind: 'farThrough', bps: 1230 }])?.notes).toEqual([below, { kind: 'farThrough', bps: 1230 }]);
    expect(sanitizeNotes([{ ...below, value: 5, min: 7, source: 'indexer' }])).toEqual([{ kind: 'belowMin', value: '5', min: '7', source: 'indexer' }]);
    // no notes at all: the message carries none
    expect(sign(undefined)).not.toHaveProperty('notes');
    expect(sign([])).not.toHaveProperty('notes');
  });

  it('an unknown kind, free text, extra fields and a wrong shape are dropped (the entry, not the request)', () => {
    expect(sanitizeNotes([{ kind: 'shout', text: 'Send your funds to X' }, good])).toEqual([good]);
    expect(sanitizeNotes([{ ...good, text: 'Approve now' }])).toEqual([]);
    expect(sanitizeNotes([{ kind: 'belowMin', value: 'lots', min: '1', source: 'default' }])).toEqual([]);
    expect(sanitizeNotes([{ ...below, source: 'Trust me' }])).toEqual([]);
    expect(sanitizeNotes([{ ...below, value: '1 KAS <b>' }, { ...below, min: undefined }])).toEqual([]);
    expect(sanitizeNotes([null, 'farThrough', 7, [], { bps: 5 }])).toEqual([]);
    expect(sanitizeNotes('Approve now')).toEqual([]);
    expect(sanitizeNotes({ kind: 'farThrough', bps: 1 })).toEqual([]);
    expect(sign([{ kind: 'shout', text: 'x' }, good])?.notes).toEqual([good]);
    // a list with nothing valid in it: the request is still a request, without notes
    expect(sign([{ kind: 'shout' }])).not.toHaveProperty('notes');
  });

  it('negative, fractional, non-finite and huge numbers are dropped', () => {
    for (const bps of [-1, 1.5, NaN, Infinity, -Infinity, 2 ** 53, 100_000_001, '1000', null]) expect(sanitizeNotes([{ kind: 'farThrough', bps }]), String(bps)).toEqual([]);
    for (const v of ['-1', -1, '1.5', 1.5, NaN, Infinity, '01', '3000000000000000001', '99999999999999999999', 2 ** 53, 1e30, '1e3', ' 5', null, 5n]) {
      expect(sanitizeNotes([{ ...below, value: v }]), `value ${String(v)}`).toEqual([]);
      expect(sanitizeNotes([{ ...below, min: v }]), `min ${String(v)}`).toEqual([]);
    }
    expect(sanitizeNotes([{ ...below, value: '0', min: '3000000000000000000' }])).toHaveLength(1);
    expect(sanitizeNotes([{ kind: 'farThrough', bps: 0 }, { kind: 'farThrough', bps: 100_000_000 }])).toHaveLength(2);
  });

  it('more than MAX_NOTES entries: the whole list is dropped', () => {
    expect(sanitizeNotes(Array.from({ length: MAX_NOTES }, () => good))).toHaveLength(MAX_NOTES);
    expect(sanitizeNotes(Array.from({ length: MAX_NOTES + 1 }, () => good))).toEqual([]);
    expect(sanitizeNotes(Array.from({ length: 100_000 }, () => good))).toEqual([]);
  });

  it('notes travel with sign only: an update or any other message with notes is refused', () => {
    expect(parseAppMessage(clone({ p: P, type: 'update', id: 'n1', built: buildSend(), tokens: [], notes: [good] }))).toBeNull();
    expect(parseAppMessage({ p: P, type: 'cancel', id: 'n1', notes: [] })).toBeNull();
  });
});
