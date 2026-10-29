// Audit 2026-10-29 S2: a finding the decoder blocks signing on is never signed by the frame, whatever the page asks, and the frame names the finding.
// A hostile parent could otherwise send a transaction the app's own confirmation screen refuses (here a 5 KAS fee) and have it approved.
import { describe, expect, it } from 'vitest';
import { createSignerCore, type SignerCore } from './core';
import { SIGNER_PROTOCOL, parseAppMessage, type SignerToApp } from './protocol';
import { loadKaspaSdkNode } from '../data/kaspa-sdk.node';
import type { BuiltTx } from '../kob/types';
import { MAKER_PK, MAKER_SK } from '../testing/token-fixtures';
import { goldenRequest } from '../testing/golden';
import { assertConsensusValid, kob } from '../testing/wallet-fixtures';

const sdk = loadKaspaSdkNode();

function frame(sk: string) {
  const sent: SignerToApp[] = [];
  const core: SignerCore = createSignerCore({ sdk, kob, post: (m) => sent.push(structuredClone(m)) });
  const send = (m: Record<string, unknown>) => core.receive(parseAppMessage(structuredClone({ p: SIGNER_PROTOCOL, ...m }))!);
  core.importKey(sk);
  return { core, sent, send };
}

const cancelAsk = (mut: (r: Record<string, any>) => void = () => undefined): BuiltTx => {
  const r = goldenRequest('cancel.ask', MAKER_PK) as unknown as Record<string, any>;
  mut(r);
  return kob.build(r as never);
};

describe('the signer frame does not approve a transaction the decoder blocks', () => {
  it('a cancel paying 5 KAS in fee: the problem is counted and named, and approve refuses', () => {
    const dear = cancelAsk((r) => {
      r.fee = { feeRate: '50000', feeMode: 'relay' };
    });
    const { core, sent, send } = frame(MAKER_SK);
    const req = send({ type: 'sign', id: 'r1', built: dear, tokens: [] });
    expect(req!.summary.problems).toBeGreaterThan(0);
    expect(req!.summary.problemCodes).toContain('fee-excessive');
    expect(core.approve('r1')).toBe(false);
    expect(sent.some((m) => m.type === 'signed')).toBe(false);
  });

  it('the same transaction at the normal fee has no problem and is signed', () => {
    const honest = cancelAsk();
    const { core, sent, send } = frame(MAKER_SK);
    const req = send({ type: 'sign', id: 'r1', built: honest, tokens: [] });
    expect(req!.summary.problems).toBe(0);
    expect(req!.summary.problemCodes).toEqual([]);
    expect(core.approve('r1')).toBe(true);
    assertConsensusValid(honest, (sent.at(-1) as Extract<SignerToApp, { type: 'signed' }>).signatures);
  });
});
