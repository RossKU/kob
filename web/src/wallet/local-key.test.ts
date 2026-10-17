import { describe, expect, it } from 'vitest';
import { createLocalKeyAdapter, SignerHost } from './local-key';
import { signAndSubmit, SignFlowError } from './sign';
import { WalletError } from './types';
import { createSignerCore, type PendingRequest } from '../signer/core';
import { parseAppMessage } from '../signer/protocol';
import { loadKaspaSdkNode } from '../data/kaspa-sdk.node';
import { pubkeyToAddress, spkStringToAddress } from '../data/kaspa-sdk';
import { MockNode } from '../testing/mock-node';
import { MAKER_PK, MAKER_SK, OTHER_SK } from '../testing/token-fixtures';
import { buildCancel, buildSend, kob } from '../testing/wallet-fixtures';
import { addressOfSpk, p2pkSpk } from '../../mock/address.mjs';
import type { BuiltTx } from '../kob/types';

const sdk = loadKaspaSdkNode();
const NET = 'testnet-10';

/**
 * The app's host and the frame's core, wired as postMessage would (structured clone + each side's parser). `user` decides what the person at the
 * frame does with a request: approve, reject, or nothing (the request stays open).
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
    await connect();
    const built = buildSend();
    await host.sign(built, []);
    expect(prompts).toBe(1);
    expect(log.filter((l) => l === 'request true')).toHaveLength(1);
    // the same transaction again as the re-timing of the one just signed: signed in the frame at once, the frame never shown, no prompt
    expect(await host.sign(built, [], undefined, true)).toHaveLength(built.sign.length);
    expect(prompts).toBe(1);
    expect(log.filter((l) => l === 'request true')).toHaveLength(1);
  });
});
