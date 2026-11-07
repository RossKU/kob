// A pair order of tokens without a registry entry (the golden pair vectors, decoded from the built transaction): it is decoded against the registry plus
// BOTH open-list tokens, so each pin is compared and each token's identity is checked (audit A1-2), and the confirmation table names a quote that has no
// ticker by its covenant id, on the decimals of the scale its order names.
import { describe, expect, it } from 'vitest';
import type { Services } from '../../app/services';
import type { WalletSession } from '../../app/context';
import { signerTokenHints } from '../../app/token-hints';
import { loadKaspaSdkNode } from '../../data/kaspa-sdk.node';
import { spkStringToAddress } from '../../data/kaspa-sdk';
import { decodeSigning } from '../../kob/decode';
import { longId, parseRegistry, type ExtensionReference, type TokenInfo, type TokenRegistry } from '../../kob/registry';
import { loadKobNode } from '../../kob/wasm.node';
import type { ActionRequest, BuiltTx, Hex, TokenProgram } from '../../kob/types';
import { summarize } from '../../signer/summary';
import { MAKER, goldenRequest, nodeFactsOf, tradableRegistryJson } from '../../testing/chain-fixtures';
import { MockNode } from '../../testing/mock-node';
import type { WalletAdapter } from '../../wallet/types';
import { openTokenIdentity } from './confirm-model';
import { checkBeforeSigning, registryOfJob } from './send';

const NET = 'testnet-10';
const kob = loadKobNode();
const sdk = loadKaspaSdkNode();
const shipped: TokenRegistry = parseRegistry(tradableRegistryJson(), { kob });
/** a registry without tokens: every token of the transaction is an open-list one */
const empty: TokenRegistry = { ...shipped, tokens: [], byCovenantId: new Map() };
const built: BuiltTx = kob.build(goldenRequest<ActionRequest>('pair.create.ask', MAKER.pk));
const decode = (registry: TokenRegistry) => decodeSigning({ kob, built, maker: MAKER.pk, registry, nodeInputs: nodeFactsOf(built) });
const pair = decode(empty).orders[0]!.description.pair!;
const [A, B] = [pair.base.covId, pair.quote.covId] as [Hex, Hex];

/** The TokenInfo `synthesizeOpenToken` makes of one token of the pair: its pins as the order names them, its commitment picked by the indexer. */
function open(t: typeof pair.base, references: ExtensionReference[] = [], ext: Hex = t.ext!): TokenInfo {
  const tpl = kob.templates().find((x) => x.hash === t.tplHash)!;
  return {
    ticker: longId(t.covId), name: longId(t.covId), family: t.family!, covenantId: t.covId, templateId: 'kcc20-ref-8x8', templateHash: t.tplHash, extensionCommitment: ext, extensionClass: 'other', decimals: 6,
    tick: 1n, capabilities: [], display: null, openList: true, openScale: 'orders', program: tpl.name as TokenProgram, templateMatchesPinned: true, prefixLen: Number(t.prefixLen), suffixLen: Number(t.suffixLen),
    slots: { inputs: tpl.tokenSlots![0], outputs: tpl.tokenSlots![1] }, tradable: true, untradableReason: null, json: {} as TokenInfo['json'],
    extension: { commitment: ext, source: 'indexer', references, mismatch: references.filter((r) => r.commitment !== ext) },
  };
}
const OTHER = 'ab'.repeat(32);
const codes = (xs: { code: string }[]) => xs.map((x) => x.code);

const services = (registry: TokenRegistry): Services => {
  const node = new MockNode({ network: NET, addressOf: (spk) => spkStringToAddress(sdk, spk, NET) });
  node.seedFromInputs(built.tx);
  return { kob, sdk, node, registry, config: { network: NET } } as unknown as Services;
};
const wallet: Pick<WalletSession, 'info' | 'adapter' | 'networkMismatch'> = { info: { pubkey: MAKER.pk, network: NET, id: 'kasware' } as WalletSession['info'], adapter: {} as WalletAdapter, networkMismatch: false };

describe('a pair of open-list tokens is decoded against the registry plus both', () => {
  it('both tokens: each is known to the decoder, each has its identity row, nothing blocks', () => {
    const registry = registryOfJob(empty, { openTokens: [open(pair.base), open(pair.quote)] });
    const s = decode(registry);
    expect(s.blocking).toEqual([]);
    expect(codes(s.warnings)).not.toContain('token-unlisted');
    const id = openTokenIdentity(s, registry);
    expect(id.rows.map((r) => r.id).sort()).toEqual([`ext-${A}`, `ext-${B}`].sort());
    expect(id.blocking).toEqual([]);
  });

  it('only the base named (what the job carried before): the quote is unchecked, which is why the job names both', () => {
    const registry = registryOfJob(empty, { openTokens: [open(pair.base)] });
    const s = decode(registry);
    expect(codes(s.warnings)).toContain('token-unlisted');
    expect(openTokenIdentity(s, registry).rows.map((r) => r.id)).toEqual([`ext-${A}`]);
  });

  it('a registry token next to an open-list quote: the registry entry stays, the quote alone has a row', () => {
    const entry = { ...open(pair.base), openList: false } as TokenInfo;
    const reg: TokenRegistry = { ...empty, tokens: [entry], byCovenantId: new Map([[A, entry]]) };
    const registry = registryOfJob(reg, { openTokens: [open(pair.quote)] });
    expect(registry.byCovenantId.get(A)).toBe(entry);
    expect(openTokenIdentity(decode(registry), registry).rows.map((r) => r.id)).toEqual([`ext-${B}`]);
  });

  it('no open-list token: the registry itself', () => {
    expect(registryOfJob(empty, {})).toBe(empty);
    expect(registryOfJob(empty, { openTokens: [] })).toBe(empty);
  });

  for (const [name, side, other] of [['quote', pair.quote, pair.base], ['base', pair.base, pair.quote]] as const) {
    it(`a commitment known without the indexer that differs from the ${name}'s blocks the signing`, async () => {
      const job = { built, openTokens: [open(other), open(side, [{ commitment: OTHER, source: 'holdings' }])] };
      const registry = registryOfJob(empty, job);
      const id = openTokenIdentity(decode(registry), registry);
      expect(codes(id.blocking)).toEqual(['open-token-extension-mismatch']);
      expect(id.rows.find((r) => r.id === `ext-${side.covId}`)!.tone).toBe('bad');
      const r = await checkBeforeSigning(services(empty), wallet, job);
      expect('blocked' in r && r.blocked.code).toBe('open-token-extension-mismatch');
    });

    it(`the ${name}'s pin against another commitment than the page's: blocking`, async () => {
      const job = { built, openTokens: [open(other), open(side, [], OTHER)] };
      const r = await checkBeforeSigning(services(empty), wallet, job);
      expect('blocked' in r && r.blocked.code).toBe('pair-token-mismatch');
    });
  }

  it('a clean job passes the checks before signing', async () => {
    const r = await checkBeforeSigning(services(empty), wallet, { built, openTokens: [open(pair.base), open(pair.quote)] });
    expect('job' in r).toBe(true);
  });
});

describe('the confirmation table of a pair whose tokens have no ticker', () => {
  const rowOf = (hints: ReturnType<typeof signerTokenHints>, id: string) => summarize(kob, built, MAKER.pk, hints).orders[0]!.rows.find((r) => r.id === id)!;

  it('names the quote by its covenant id, never an empty name', () => {
    const hints = signerTokenHints(kob, built, MAKER.pk, empty, NET);
    expect(hints.find((h) => h.covenantId === B)).toMatchObject({ ticker: '', source: 'assumed' });
    const value = rowOf(hints, 'pairPair').value;
    expect(value).toContain(longId(A));
    expect(value).toContain(longId(B));
    expect(rowOf(hints, 'pairTotal').value).toContain(longId(B));
  });

  it('reads each token on the decimals of the scale the order names, still marked: nothing on chain states them', () => {
    // the vector's scales are not the assumed 10^8: the token would read 100 times off
    const six = kob.build(goldenRequest<ActionRequest>('pair.create.ask', MAKER.pk));
    const scale = (id: Hex) => (id === A ? pair.base.scale : pair.quote.scale);
    for (const id of [A, B]) {
      const hint = signerTokenHints(kob, six, MAKER.pk, empty, NET).find((h) => h.covenantId === id)!;
      expect(10n ** BigInt(hint.decimals)).toBe(scale(id));
      expect(hint).toMatchObject({ ticker: '', source: 'assumed' });
    }
  });
});
