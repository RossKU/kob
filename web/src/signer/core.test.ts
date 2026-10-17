import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { createSignerCore, SignerKeyError } from './core';
import { SIGNER_PROTOCOL, parseAppMessage, parseSignerMessage, type SignerToApp } from './protocol';
import { summarize, summaryLines } from './summary';
import { loadKaspaSdkNode } from '../data/kaspa-sdk.node';
import { MAKER_PK, MAKER_SK, OTHER_SK, TOKEN_COV_ID } from '../testing/token-fixtures';
import { assertConsensusValid, buildCancel, buildSend, kob } from '../testing/wallet-fixtures';
import { decodeSigning } from '../kob/decode';
import type { BuiltTx } from '../kob/types';

const sdk = loadKaspaSdkNode();
const P = SIGNER_PROTOCOL;

function setup() {
  const sent: SignerToApp[] = [];
  const core = createSignerCore({ sdk, kob, post: (m) => sent.push(structuredClone(m)) });
  // every message crosses a structured clone and the app's parser, as through postMessage
  const app = () => sent.map((m) => parseSignerMessage(m));
  const send = (m: Record<string, unknown>) => core.receive(parseAppMessage(structuredClone({ p: P, ...m }))!);
  return { core, sent, app, send };
}

const signReq = (built: BuiltTx, id = 'r1') => ({ type: 'sign' as const, id, built, tokens: [{ covenantId: TOKEN_COV_ID, ticker: 'EXKCC', decimals: 8 }] });

describe('signer core: the key', () => {
  it('derives the x-only public key and announces only that', () => {
    const { core, sent } = setup();
    expect(core.importKey(MAKER_SK)).toBe(MAKER_PK);
    expect(core.pubkey()).toBe(MAKER_PK);
    expect(sent).toEqual([{ p: P, type: 'connected', pubkey: MAKER_PK }]);
    // 0x prefix, upper case and surrounding blanks are accepted
    const b = setup();
    expect(b.core.importKey(`  0x${MAKER_SK.toUpperCase()}\n`)).toBe(MAKER_PK);
  });

  it('refuses a key that does not parse, without echoing it', () => {
    for (const bad of ['', 'zz'.repeat(32), MAKER_SK.slice(2), MAKER_SK + '00', '00'.repeat(32), 'ff'.repeat(32), `${MAKER_SK} ${MAKER_SK}`]) {
      const { core, sent } = setup();
      let err: unknown;
      try {
        core.importKey(bad);
      } catch (e) {
        err = e;
      }
      expect(err, bad).toBeInstanceOf(SignerKeyError);
      if (bad.length >= 8) expect((err as Error).message).not.toContain(bad.slice(2, 10));
      expect(core.hasKey()).toBe(false);
      expect(sent).toEqual([]);
    }
  });

  it('forget frees the key: no signature afterwards, the app is told', () => {
    const { core, sent, send } = setup();
    core.importKey(MAKER_SK);
    send({ type: 'forget' });
    expect(core.hasKey()).toBe(false);
    expect(core.pubkey()).toBeNull();
    expect(sent.at(-1)).toEqual({ p: P, type: 'forgotten' });
    expect(send(signReq(buildSend()))).toBeNull();
    expect(sent.at(-1)).toEqual({ p: P, type: 'rejected', id: 'r1', reason: 'no-key' });
  });
});

describe('signer core: signing', () => {
  it('a request alone signs nothing; the approval signs exactly the requested inputs and the result is consensus-valid', () => {
    for (const build of [buildSend, buildCancel]) {
      const { core, sent, app, send } = setup();
      core.importKey(MAKER_SK);
      const built = build();
      const req = send(signReq(built));
      expect(req?.id).toBe('r1');
      expect(req!.summary.lines.length).toBeGreaterThan(0);
      expect(sent.filter((m) => m.type === 'signed')).toEqual([]); // nothing without the user's click
      expect(core.approve('other-id')).toBe(false);
      expect(core.approve('r1')).toBe(true);
      const signed = app().at(-1)!;
      expect(signed.type).toBe('signed');
      const sigs = (signed as Extract<SignerToApp, { type: 'signed' }>).signatures;
      expect(sigs.map((s) => s.inputIndex)).toEqual(built.sign.map((s) => s.inputIndex));
      assertConsensusValid(built, sigs); // finalize verifies each against kob-wasm's digest, the script engine runs the result
      expect(core.approve('r1')).toBe(false); // one approval, one answer
    }
  });

  it('refuses a request for another key, a second request while one is open, and a forged digest', () => {
    const { core, sent, send } = setup();
    core.importKey(OTHER_SK);
    expect(send(signReq(buildSend()))).toBeNull();
    expect(sent.at(-1)).toEqual({ p: P, type: 'rejected', id: 'r1', reason: 'invalid' });

    const b = setup();
    b.core.importKey(MAKER_SK);
    expect(b.send(signReq(buildSend(), 'a'))).not.toBeNull();
    expect(b.send(signReq(buildSend(), 'b'))).toBeNull();
    expect(b.sent.at(-1)).toEqual({ p: P, type: 'rejected', id: 'b', reason: 'busy' });

    // the app claims another digest than the transaction has: the signature over the real one does not verify against it, nothing is returned
    const c = setup();
    c.core.importKey(MAKER_SK);
    const forged = buildSend();
    forged.sign[0]!.sighash = '11'.repeat(32);
    c.send(signReq(forged));
    expect(c.core.approve('r1')).toBe(false);
    expect(c.sent.at(-1)).toEqual({ p: P, type: 'rejected', id: 'r1', reason: 'invalid' });
  });

  it('reject and cancel drop the request with a neutral answer', () => {
    const { core, sent, send } = setup();
    core.importKey(MAKER_SK);
    send(signReq(buildSend()));
    core.reject('r1');
    expect(sent.at(-1)).toEqual({ p: P, type: 'rejected', id: 'r1', reason: 'user' });
    expect(core.approve('r1')).toBe(false);
    send(signReq(buildSend(), 'r2'));
    send({ type: 'cancel', id: 'r2' });
    expect(sent.at(-1)).toEqual({ p: P, type: 'rejected', id: 'r2', reason: 'cancelled' });
    expect(core.pending()).toBeNull();
  });
});

describe('signer core: no storage, no key in any message', () => {
  const touched: string[] = [];
  const fakeStorage = (name: string) =>
    new Proxy({}, { get: (_t, p) => (touched.push(`${name}.${String(p)}`), () => null), set: (_t, p) => (touched.push(`${name}.${String(p)}=`), true) });
  const saved: Record<string, PropertyDescriptor | undefined> = {};
  const names = ['localStorage', 'sessionStorage', 'indexedDB', 'caches', 'document'] as const;
  beforeEach(() => {
    touched.length = 0;
    for (const n of names) saved[n] = Object.getOwnPropertyDescriptor(globalThis, n);
    for (const n of ['localStorage', 'sessionStorage', 'indexedDB', 'caches'] as const) Object.defineProperty(globalThis, n, { configurable: true, get: () => fakeStorage(n) });
    Object.defineProperty(globalThis, 'document', {
      configurable: true,
      value: { get cookie() { touched.push('document.cookie'); return ''; }, set cookie(_v: string) { touched.push('document.cookie='); } },
    });
  });
  afterEach(() => {
    for (const n of names) {
      if (saved[n]) Object.defineProperty(globalThis, n, saved[n]!);
      else Reflect.deleteProperty(globalThis, n);
    }
  });

  it('a whole session (key, request, approval, forget) touches no storage and never puts the key in a message', () => {
    const spy = vi.spyOn(console, 'log');
    const { core, sent, send } = setup();
    core.importKey(MAKER_SK);
    send(signReq(buildSend()));
    core.approve('r1');
    try {
      core.importKey('not a key ' + MAKER_SK.slice(0, 20));
    } catch {
      /* refused */
    }
    send({ type: 'forget' });
    expect(touched).toEqual([]);
    const wire = JSON.stringify(sent);
    expect(wire).not.toContain(MAKER_SK);
    expect(wire).not.toContain(MAKER_SK.slice(0, 16));
    expect(spy).not.toHaveBeenCalled();
    spy.mockRestore();
  });
});

describe('signer summary: decoded in the frame, net effect, order terms and fee', () => {
  it('a token send: the tokens leave this key, KAS pays the fee', () => {
    const s = summarize(kob, buildSend(), MAKER_PK, [{ covenantId: TOKEN_COV_ID, ticker: 'EXKCC', decimals: 2 }]);
    expect(s.title).toBe('Send');
    const token = s.lines.find((l) => l.label.startsWith('EXKCC'))!;
    expect(token.value).toBe('−90'); // 9000 base units at 2 decimals
    expect(token.dir).toBe('out');
    expect(s.lines.find((l) => l.label === 'EXKCC (70707070…70707070) to other keys')?.value).toBe('90');
    expect(s.fee).toMatch(/ KAS$/);
  });

  it('without a hint a token is named by its covenant id and counted in base units', () => {
    const d = decodeSigning({ kob, built: buildSend(), maker: MAKER_PK, registry: null });
    const s = summaryLines(d, []);
    expect(s.lines.some((l) => l.label === 'Token 70707070…70707070' && l.value === '−9000 base units')).toBe(true);
  });

  it('a cancel', () => {
    const s = summarize(kob, buildCancel(), MAKER_PK, []);
    expect(s.title).toBe('Cancel order');
    expect(s.lines[0]!.label).toBe('KAS');
  });
});

describe('signer core: a request re-timed after the user confirmed it', () => {
  const retimeSetup = (now: { t: number }) => {
    const sent: SignerToApp[] = [];
    const core = createSignerCore({ sdk, kob, post: (m) => sent.push(structuredClone(m)), now: () => now.t });
    const send = (m: Record<string, unknown>) => core.receive(parseAppMessage(structuredClone({ p: P, ...m }))!);
    core.importKey(MAKER_SK);
    return { core, sent, send };
  };

  it('is signed at once when it re-times the confirmed one with nothing worse; anything else is shown', () => {
    const now = { t: 1_000 };
    const { core, sent, send } = retimeSetup(now);
    expect(send(signReq(buildSend(), 'r1'))).not.toBeNull();
    expect(core.approve('r1')).toBe(true);
    // the same transaction again, naming the confirmed request: no prompt, a signature
    expect(send({ ...signReq(buildSend(), 'r2'), retimeOf: 'r1' })).toBeNull();
    expect(sent.at(-1)).toMatchObject({ type: 'signed', id: 'r2' });
    expect(core.pending()).toBeNull();
    // another transaction (a cancel instead of the send): shown and waiting for the user
    expect(send({ ...signReq(buildCancel(), 'r3'), retimeOf: 'r2' })).not.toBeNull();
    core.reject('r3');
    // naming a request that was not the confirmed one, or too late: shown
    expect(send({ ...signReq(buildSend(), 'r4'), retimeOf: 'r9' })).not.toBeNull();
    core.reject('r4');
    now.t += 121_000;
    expect(send({ ...signReq(buildSend(), 'r5'), retimeOf: 'r2' })).not.toBeNull();
  });
});
