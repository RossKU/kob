import { describe, expect, it } from 'vitest';
import { createLocalKeyAdapter, SignerHost } from './local-key';
import { signAndSubmit, SignFlowError } from './sign';
import { WalletError, type LiveSigning } from './types';
import { createSignerCore, type PendingRequest } from '../signer/core';
import { parseAppMessage } from '../signer/protocol';
import { loadKaspaSdkNode } from '../data/kaspa-sdk.node';
import { pubkeyToAddress, spkStringToAddress } from '../data/kaspa-sdk';
import { MockNode } from '../testing/mock-node';
import { MAKER_PK, MAKER_SK, OTHER_SK } from '../testing/token-fixtures';
import { buildCancel, buildSend, kob } from '../testing/wallet-fixtures';
import { addressOfSpk, p2pkSpk } from '../../mock/address.mjs';
import { planOrder } from '../kob/plan';
import { MAKER_SK as TICKET_SK } from '../testing/fixtures';
import { buildIntent } from '../ui/ticket/form-state';
import { CTX, form, ticketEnv } from '../ui/ticket/ticket-fixtures';
import type { BuiltTx } from '../kob/types';
import { emptyIssueForm, planIssue } from '../kob/issue';
import { formatKas } from '../kob/units';
import { pubkeyOf } from '../testing/local-signer';
import { signAndSubmitCreation } from './create';
import { planMerge, type MergeToken } from '../kob/merge';
import { MAKER as CHAIN_MAKER, TOKEN as CHAIN_TOKEN, goldenRequest as chainGoldenRequest, keyUtxo, tokenUtxo as chainTokenUtxo } from '../testing/chain-fixtures';

const sdk = loadKaspaSdkNode();
const NET = 'testnet-10';

/**
 * The app's host and the frame's core, wired as postMessage would (structured clone + each side's
 * parser). `user` decides what the person at the frame does with a request: approve, reject, or
 * nothing (the request stays open).
 */
function pair(user: (req: PendingRequest) => 'approve' | 'reject' | 'ignore' = () => 'approve') {
  const log: string[] = [];
  let host!: SignerHost;
  const core = createSignerCore({ sdk, kob, post: (m) => queueMicrotask(() => host.receive(structuredClone(m))) });
  host = new SignerHost(
    (m) =>
      queueMicrotask(() => {
        const parsed = parseAppMessage(structuredClone(m));
        if (!parsed) return;
        const req = core.receive(parsed);
        if (!req) return;
        const what = user(req);
        if (what === 'approve') core.approve(req.id);
        else if (what === 'reject') core.reject(req.id);
      }),
    { onConnected: (pk) => log.push(`connected ${pk}`), onRequest: (a) => log.push(`request ${a}`), onForgotten: () => log.push('forgotten') },
  );
  const adapter = createLocalKeyAdapter({ host: () => host, sdk });
  const connect = async (sk = MAKER_SK) => {
    core.importKey(sk);
    await new Promise((r) => setTimeout(r, 0));
  };
  return { core, host, adapter, log, connect };
}

const seeded = (built: BuiltTx) => {
  const n = new MockNode({ network: NET, addressOf: (spk) => spkStringToAddress(sdk, spk, NET) });
  n.seedFromInputs(built.tx);
  return n;
};

describe('private-key adapter (the app side of the signer frame)', () => {
  it('derives the address of the configured network from the key the frame announces', async () => {
    const { adapter, connect, log } = pair();
    await expect(adapter.connect(NET)).rejects.toBeInstanceOf(WalletError); // no key in the frame yet
    await connect();
    expect(log).toContain(`connected ${MAKER_PK}`);
    const independent = (prefix: string) => addressOfSpk(prefix, p2pkSpk(MAKER_PK));
    const t10 = await adapter.connect('testnet-10');
    expect(t10).toMatchObject({ id: 'local-key', label: 'Private key', pubkey: MAKER_PK, network: 'testnet-10' });
    expect(t10.address).toBe(independent('kaspatest'));
    expect(t10.address).toBe(pubkeyToAddress(sdk, MAKER_PK, 'testnet-10'));
    const main = await adapter.connect('kaspa_mainnet');
    expect(main.network).toBe('mainnet');
    expect(main.address).toBe(independent('kaspa'));
    expect((await adapter.refresh!()).address).toBe(main.address);
    expect(adapter.detect()).toBe(true);
    expect(adapter.covenantSigning).toBe('proven');
  });

  it('runs the whole pipeline (build -> frame signs after approval -> finalize -> validate -> submit) for a token send and a cancel', async () => {
    for (const build of [buildSend, buildCancel]) {
      const { adapter, connect, log } = pair();
      await connect();
      await adapter.connect(NET);
      const built = build();
      const node = seeded(built);
      const res = await signAndSubmit({ kob, node, adapter, built, network: NET });
      expect(res.txid).toBe(built.tx.id);
      expect(node.submissions).toHaveLength(1);
      expect(log.filter((l) => l.startsWith('request'))).toEqual(['request true', 'request false']);
    }
  });

  it('an update of the request shown in the frame is what Approve signs and what is sent (the open confirmation follows the market)', async () => {
    let shownId = '';
    const { adapter, connect, core } = pair((req) => ((shownId = req.id), 'ignore'));
    await connect();
    await adapter.connect(NET);
    const first = buildSend();
    const second = buildCancel();
    const node = seeded(first);
    node.seedFromInputs(second.tx);
    let replace: ((b: BuiltTx) => void) | null = null;
    const live: LiveSigning = { attach: (to) => void ((replace = to.replace), to.clock(1000n)) };
    const sending = signAndSubmit({ kob, node, adapter, built: first, network: NET, live: { signing: live, submitByOf: () => undefined } });
    await new Promise((r) => setTimeout(r, 0));
    expect(shownId).not.toBe('');
    replace!(second);
    await new Promise((r) => setTimeout(r, 0));
    expect(core.pending()?.built.tx.id).toBe(second.tx.id);
    core.approve(shownId);
    const res = await sending;
    expect(res.built).toBe(second);
    expect(res.txid).toBe(second.tx.id);
    expect(node.submissions).toHaveLength(1);
    expect(live.signed).toBe(second);
  });

  it('the node clock goes to the frame BEFORE the request, so its first view already says every time from it', async () => {
    const sent: string[] = [];
    let host!: SignerHost;
    const core = createSignerCore({ sdk, kob, post: (m) => queueMicrotask(() => host.receive(structuredClone(m))) });
    host = new SignerHost(
      (m) =>
        queueMicrotask(() => {
          sent.push(m.type === 'clock' ? `clock ${m.daa}` : m.type);
          const parsed = parseAppMessage(structuredClone(m));
          const req = parsed ? core.receive(parsed) : null;
          if (req) core.approve(req.id);
        }),
      { onConnected: () => undefined, onRequest: () => undefined, onForgotten: () => undefined },
    );
    const adapter = createLocalKeyAdapter({ host: () => host, sdk });
    core.importKey(MAKER_SK);
    await new Promise((r) => setTimeout(r, 0));
    await adapter.connect(NET);
    const built = buildSend();
    const node = seeded(built);
    await signAndSubmit({ kob, node, adapter, built, network: NET, clockDaa: 685_000_000n });
    expect(sent.indexOf('clock 685000000')).toBeGreaterThanOrEqual(0);
    expect(sent.indexOf('clock 685000000')).toBeLessThan(sent.indexOf('sign'));
  });

  it('a rejection in the frame is the neutral "rejected by user" outcome; nothing is submitted', async () => {
    const { adapter, connect } = pair(() => 'reject');
    await connect();
    await adapter.connect(NET);
    const built = buildSend();
    const node = seeded(built);
    const err = await signAndSubmit({ kob, node, adapter, built, network: NET }).catch((e) => e);
    expect(err).toBeInstanceOf(SignFlowError);
    expect((err as SignFlowError).rejectedByUser).toBe(true);
    expect(node.submissions).toHaveLength(0);
  });

  it('a request nobody approves never completes (it times out, the frame drops it); nothing is submitted', async () => {
    const seen: string[] = [];
    const { adapter, connect, core } = pair((req) => (seen.push(req.id), 'ignore'));
    await connect();
    await adapter.connect(NET);
    const built = buildSend();
    const node = seeded(built);
    const err = await signAndSubmit({ kob, node, adapter, built, network: NET, timeoutMs: 50 }).catch((e) => e);
    expect((err as SignFlowError).code).toBe('timeout');
    expect(seen).toHaveLength(1);
    await new Promise((r) => setTimeout(r, 0));
    expect(core.pending()).toBeNull(); // the app's cancel reached the frame
    expect(node.submissions).toHaveLength(0);
  });

  it('refuses to forward a transaction that asks for another key', async () => {
    const { adapter, connect } = pair();
    await connect(OTHER_SK);
    await adapter.connect(NET);
    await expect(adapter.signTx(buildSend())).rejects.toThrow(/another key/);
  });

  it('forget (disconnect) clears the key in the frame and on this side', async () => {
    const { adapter, connect, core, host, log } = pair();
    await connect();
    await adapter.connect(NET);
    host.forget();
    await new Promise((r) => setTimeout(r, 0));
    expect(core.hasKey()).toBe(false);
    expect(host.pubkey()).toBeNull();
    expect(log).toContain('forgotten');
    await expect(adapter.refresh!()).rejects.toBeInstanceOf(WalletError);
    await expect(adapter.signTx(buildSend())).rejects.toBeInstanceOf(WalletError);
  });

  it('drops answers that are not protocol, or that answer another request', async () => {
    const { adapter, connect, host } = pair(() => 'ignore');
    await connect();
    await adapter.connect(NET);
    const pending = adapter.signTx(buildSend(), { timeoutMs: 100 });
    host.receive({ p: 'kob-signer/1', type: 'signed', id: 'someone-else', signatures: [{ inputIndex: 0, signature: 'ab'.repeat(64) + '01' }] });
    host.receive({ type: 'signed' });
    host.receive('signed');
    await expect(pending).rejects.toMatchObject({ code: 'timeout' });
  });

  it('a re-timed request the frame accepts is signed without the frame being shown (no second prompt after a slow approve)', async () => {
    let prompts = 0;
    const { host, connect, log } = pair(() => {
      prompts++;
      return 'approve';
    });
    await connect(TICKET_SK);
    // what the app re-times: an auction (a market buy), planned again by the ticket on the same
    // environment with a later clock (OrderTicket.tsx `retimer`), so it spends the same inputs
    const env = ticketEnv();
    const market = buildIntent(form('market', 'buy', { amount: '2' }), CTX).intent!;
    const plan = (daa: bigint) => planOrder({ ...env, clock: { ...env.clock, daa: env.clock.daa + daa, unixSeconds: env.clock.unixSeconds + daa / 10n } }, market).built!;
    await host.sign(plan(0n), []);
    expect(prompts).toBe(1);
    expect(log.filter((l) => l === 'request true')).toHaveLength(1);
    // the order planned 15 s later as the re-timing of the one just signed: signed in the frame at
    // once, the frame never shown, no prompt
    const retimed = plan(150n);
    expect(await host.sign(retimed, [], undefined, true)).toHaveLength(retimed.sign.length);
    expect(prompts).toBe(1);
    expect(log.filter((l) => l === 'request true')).toHaveLength(1);
  });
});

describe('private-key adapter: a token creation (KCC-23) on one confirmation', () => {
  const creation = () =>
    planIssue(kob, { ...emptyIssueForm(), name: 'Frame Coin', ticker: 'FRAME', decimals: 3, supply: '5000' }, {
      funding: [{ transactionId: '0b'.repeat(32), index: 1, amount: (100n * 100_000_000n).toString(), blockDaaScore: '7', pubkey: MAKER_PK }],
      maker: MAKER_PK,
      network: NET,
    });

  it('the frame shows the preparation and the genesis as ONE request, signs both on one approval, and the creation goes through', async () => {
    const seen: PendingRequest[] = [];
    const { adapter, connect } = pair((req) => {
      seen.push(req);
      return 'approve';
    });
    await connect();
    await adapter.connect(NET);
    const plan = creation();
    const n = seeded(plan.prep);
    const res = await signAndSubmitCreation({
      kob, node: n, adapter, prep: plan.prep, built: plan.built, network: NET, maker: MAKER_PK, storage: null, addressOf: (spk) => spkStringToAddress(sdk, spk, NET),
      record: { token: plan.token, declaration: plan.declaration, docs: plan.docs },
    });
    expect(seen).toHaveLength(1);
    const s = seen[0]!.summary;
    expect(s.title).toBe('Create token');
    const line = (label: RegExp) => s.lines.find((l) => label.test(l.label))?.value;
    expect([line(/^Name/), line(/^Ticker/), line(/^Decimals/)]).toEqual(['Frame Coin', 'FRAME', '3']);
    // the genesis outputs are checked against the claimed states: the supply and what this key
    // receives are named, nothing is unknown
    expect([line(/^Total supply/), line(/^To you/)]).toEqual(['5000 FRAME', '+5000 FRAME']);
    // the program and the extension commitment the outputs were checked against are lines of the
    // frame
    expect(line(/^Token program/)).toMatch(/^[A-Za-z0-9_]+$/);
    expect(line(/^Extension commitment/)).toMatch(/^[0-9a-f]{8}…[0-9a-f]{8}$/);
    expect(s.problems).toBe(0);
    // the fee and the KAS line cover both transactions
    expect(s.fee).toBe(`${formatKas(BigInt(plan.prep.fee.fee) + BigInt(plan.built.fee.fee))} KAS`);
    expect(n.submissions.map((t) => t.id)).toEqual([plan.prep.tx.id, plan.built.tx.id]);
    expect(res.txid).toBe(plan.built.tx.id);
  });

  it('the frame refuses a creation whose preparation pays another key, or whose genesis does not spend it', async () => {
    const { core, adapter, connect } = pair();
    await connect();
    await adapter.connect(NET);
    const plan = creation();
    const away = JSON.parse(JSON.stringify(plan.prep)) as BuiltTx;
    away.tx.outputs[1]!.scriptPublicKey = p2pkSpk(pubkeyOf(OTHER_SK));
    await expect(adapter.signChain!([away, plan.built])).rejects.toMatchObject({ code: 'other' });
    const unchained = JSON.parse(JSON.stringify(plan.built)) as BuiltTx;
    unchained.tx.inputs[0]!.transactionId = 'ee'.repeat(32);
    await expect(adapter.signChain!([plan.prep, unchained])).rejects.toMatchObject({ code: 'other' });
    expect(core.pending()).toBeNull();
    // a re-timed creation is not a message the frame takes
    expect(parseAppMessage({ p: 'kob-signer/1', type: 'sign', id: 'x1', built: plan.built, tokens: [], prep: plan.prep, retimeOf: 'r0' })).toBeNull();
  });
});

describe('private-key adapter: the registry table (audit 2026-10-30 G7)', () => {
  it('goes with every request and every update to the frame, and only when the app has one', async () => {
    const table = [{ covenantId: 'a'.repeat(64), ticker: 'AAA', decimals: 8 }];
    const posted: Record<string, unknown>[] = [];
    let host!: SignerHost;
    const core = createSignerCore({ sdk, kob, post: (m) => queueMicrotask(() => host.receive(structuredClone(m))) });
    host = new SignerHost(
      (m) =>
        queueMicrotask(() => {
          posted.push(structuredClone(m) as unknown as Record<string, unknown>);
          const parsed = parseAppMessage(structuredClone(m));
          const req = parsed ? core.receive(parsed) : null;
          if (req) core.approve(req.id);
        }),
      { onConnected: () => undefined, onRequest: () => undefined, onForgotten: () => undefined },
    );
    core.importKey(MAKER_SK);
    await new Promise((r) => setTimeout(r, 0));
    const withTable = createLocalKeyAdapter({ host: () => host, sdk, registryTable: () => table });
    await withTable.signTx(buildSend(), {});
    expect(posted.find((m) => m.type === 'sign')!.registry).toEqual(table);
    posted.length = 0;
    host.registryTable = undefined;
    await createLocalKeyAdapter({ host: () => host, sdk }).signTx(buildSend(), {});
    expect('registry' in posted.find((m) => m.type === 'sign')!).toBe(false);
  });
});

describe('private-key adapter: a one-approval merge session (signer/session.ts)', () => {
  const KAS = 100_000_000n;
  const STD: MergeToken = { covenantId: CHAIN_TOKEN.covenantId, program: CHAIN_TOKEN.program, templateHash: CHAIN_TOKEN.templateHash, extensionCommitment: CHAIN_TOKEN.ext, slots: { inputs: 3 } };
  const merge = () => {
    const set = Array.from({ length: 5 }, (_, i) => chainTokenUtxo(BigInt(i + 1) * KAS, CHAIN_MAKER.pk, 0x40 + i, 2n * KAS));
    const plan = planMerge({ kob, maker: CHAIN_MAKER.pk, funding: [keyUtxo(CHAIN_MAKER.pk, 50n * KAS, 201)] }, STD, set);
    expect(plan.ok).toBe(true);
    return plan;
  };
  const hello = (host: SignerHost) => host.receive({ p: 'kob-signer/1', type: 'hello', caps: ['merge-session', 'something-new'] });

  it('is offered only once the frame says it holds sessions; before that the run asks per transaction', async () => {
    const { adapter, connect, host } = pair();
    await connect(CHAIN_MAKER.sk);
    await adapter.connect(NET);
    expect(adapter.canOpenSession!()).toBe(false);
    expect(await adapter.openSession!(merge().links.map((l) => l.built))).toBeNull();
    hello(host);
    expect(adapter.canOpenSession!()).toBe(true);
  });

  it('one approval in the frame, then the steps are signed without showing the frame; an off-plan step shows it at once', async () => {
    let prompts = 0;
    const { adapter, connect, host, log } = pair(() => (prompts++, 'approve'));
    await connect(CHAIN_MAKER.sk);
    await adapter.connect(NET);
    hello(host);
    const plan = merge();
    const s = await adapter.openSession!(plan.links.map((l) => l.built));
    expect(s).toMatchObject({ txs: plan.links.length, sid: expect.stringMatching(/^[0-9a-f]{32}$/) });
    expect(prompts).toBe(1);
    const shown = log.filter((l) => l === 'request true').length;
    const sigs = await adapter.signTx(plan.links[0]!.built, { session: s!.sid });
    expect(sigs).toHaveLength(plan.links[0]!.built.sign.length);
    expect(prompts).toBe(1);
    expect(log.filter((l) => l === 'request true')).toHaveLength(shown);
    expect(adapter.sessionEnded!(s!.sid)).toBe(false);
    // the same link again: not the next one of the plan, so the frame ends the session and asks
    await adapter.signTx(plan.links[0]!.built, { session: s!.sid });
    expect(prompts).toBe(2);
    // the run sees that the session is over (its card says the later steps are confirmed one by
    // one)
    expect(adapter.sessionEnded!(s!.sid)).toBe(true);
    expect(log.filter((l) => l === 'request true')).toHaveLength(shown + 1);
  });

  it('a session the frame closes because its plan is through has not ended early', async () => {
    const { adapter, connect, host } = pair(() => 'approve');
    await connect(CHAIN_MAKER.sk);
    await adapter.connect(NET);
    hello(host);
    const plan = merge();
    const s = await adapter.openSession!(plan.links.map((l) => l.built));
    for (const l of plan.links) await adapter.signTx(l.built, { session: s!.sid });
    expect(host.endedEarly(s!.sid)).toBe(false);
    expect(adapter.sessionEnded!(s!.sid)).toBe(false);
  });

  it('a session the host no longer waits for (timed out, cancelled) is closed in the frame at once', () => {
    const posted: unknown[] = [];
    const host = new SignerHost((m) => posted.push(m));
    host.receive({ p: 'kob-signer/1', type: 'session-opened', id: 'late', sid: 'cd'.repeat(16), txs: 3, feeMax: '1000', expiresInMs: 150_000 });
    expect(posted).toEqual([{ p: 'kob-signer/1', type: 'session-close', sid: 'cd'.repeat(16) }]);
  });

  it('a rejected plan ends the run (rejected); a plan the frame refuses is null, so the run asks per transaction', async () => {
    const a = pair(() => 'reject');
    await a.connect(CHAIN_MAKER.sk);
    await a.adapter.connect(NET);
    hello(a.host);
    await expect(a.adapter.openSession!(merge().links.map((l) => l.built))).rejects.toMatchObject({ code: 'rejected' });
    const b = pair();
    await b.connect(CHAIN_MAKER.sk);
    await b.adapter.connect(NET);
    hello(b.host);
    const notMerge = kob.build(chainGoldenRequest('cancel.ask', CHAIN_MAKER.pk));
    expect(await b.adapter.openSession!([notMerge])).toBeNull();
  });
});
