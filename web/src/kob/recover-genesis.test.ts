// An order is only recovered (kob-wasm `recoverOrders`, which the pre-sign decoder, the
// placement check and the backup import all go through) when its genesis group is the order output alone. A sibling bound
// to the order's covenant id would carry that id forever and could unlock the order's custody outside the order's rules.
import { blake2b } from '@noble/hashes/blake2.js';
import { describe, expect, it } from 'vitest';
import { decodeSigning } from './decode';
import type { BuiltTx, Hex, TokenProgram, TxJson } from './types';
import { loadKobNode } from './wasm.node';
import { MAKER, goldenTx } from '../testing/chain-fixtures';

const kob = loadKobNode();
const clone = <T>(v: T): T => JSON.parse(JSON.stringify(v)) as T;

const hexBytes = (h: string): Uint8Array => Uint8Array.from(h.match(/../g) ?? [], (b) => parseInt(b, 16));
const toHex = (b: Uint8Array): Hex => Array.from(b, (x) => x.toString(16).padStart(2, '0')).join('');
const le = (v: bigint, n: number): number[] => Array.from({ length: n }, (_, i) => Number((v >> BigInt(8 * i)) & 0xffn));

/** Consensus genesis covenant id (rusty-kaspa `covenant_id`): keyed blake2b-256 over the authorising outpoint and the group. */
function genesisId(tx: TxJson, auth: number, outputs: number[]): Hex {
  const inp = tx.inputs[auth];
  const bytes: number[] = [...hexBytes(inp.transactionId), ...le(BigInt(inp.index), 4), ...le(BigInt(outputs.length), 8)];
  for (const k of outputs) {
    const o = tx.outputs[k];
    const spk = hexBytes(o.scriptPublicKey);
    const version = (spk[0] << 8) | spk[1];
    const script = spk.slice(2);
    bytes.push(...le(BigInt(k), 4), ...le(BigInt(o.value), 8), ...le(BigInt(version), 2), ...le(BigInt(script.length), 8), ...script);
  }
  return toHex(blake2b(Uint8Array.from(bytes), { dkLen: 32, key: new TextEncoder().encode('CovenantID') }));
}

/** The golden placement with a sibling in the order's genesis group: ids re-derived, order, sibling and custody rebound. */
function withSibling(name: string): { built: BuiltTx; honestId: Hex } {
  const built = clone(goldenTx(name).built);
  const [rec] = kob.recoverOrders(built.tx);
  const tx = built.tx;
  const order = tx.outputs[rec.output];
  const binding = order.covenant!;
  // the helper reproduces consensus on the honest placement first
  expect(genesisId(tx, binding.authorizingInput, [rec.output])).toBe(rec.covenantId);
  tx.outputs.push({ value: '50000000', scriptPublicKey: order.scriptPublicKey, covenant: { ...binding } });
  const group = tx.outputs.flatMap((o, k) => (o.covenant?.authorizingInput === binding.authorizingInput && o.covenant.covenantId === binding.covenantId ? [k] : []));
  expect(group.length).toBe(2);
  const id = genesisId(tx, binding.authorizingInput, group);
  for (const k of group) tx.outputs[k].covenant = { authorizingInput: binding.authorizingInput, covenantId: id };
  if (rec.custody) {
    const c = rec.custody;
    // the custody's token program: the one whose P2SH of the recovered state is the honest custody output
    const programs = Object.keys(kob.keeperTips()) as TokenProgram[];
    const spkOf = (p: TokenProgram): string | null => {
      try {
        return kob.tokenScriptPublicKey(p, c.state);
      } catch {
        return null; // a program of the other family
      }
    };
    const program = programs.find((p) => spkOf(p) === tx.outputs[c.output].scriptPublicKey);
    expect(program).toBeDefined();
    tx.outputs[c.output].scriptPublicKey = kob.tokenScriptPublicKey(program!, { ...c.state, owner: id });
  }
  return { built, honestId: rec.covenantId };
}

describe('order genesis group', () => {
  for (const name of ['create.ask', 'create.bid', 'kron.create.ask', 'kron.create.ifdAsk.repeat', 'pair.create.ask', 'pair.create.ifdAsk', 'pair.kron-kron.create.bid', 'pair.kcc20-kron.create.ifdAsk']) {
    it(`${name}: a sibling in the order's genesis group refuses the placement`, () => {
      expect(kob.recoverOrders(goldenTx(name).built.tx)).toHaveLength(1);
      const { built } = withSibling(name);
      expect(() => kob.recoverOrders(built.tx)).toThrow(/genesis group has other outputs/);
      // the pre-sign decoder blocks it (nothing it could show as the maker's order)
      const s = decodeSigning({ kob, built, maker: MAKER.pk, registry: null });
      expect(s.blocking.map((i) => i.code)).toContain('recover-failed');
    });
  }

  it('an output bound to the order id under another authorising input refuses the placement', () => {
    const built = clone(goldenTx('create.ask').built);
    const [rec] = kob.recoverOrders(built.tx);
    const tx = built.tx;
    const binding = tx.outputs[rec.output].covenant!;
    const other = tx.inputs.findIndex((_, i) => i !== binding.authorizingInput);
    expect(other).toBeGreaterThanOrEqual(0);
    tx.outputs.push({ value: '50000000', scriptPublicKey: tx.outputs[rec.output].scriptPublicKey, covenant: { authorizingInput: other, covenantId: binding.covenantId } });
    expect(() => kob.recoverOrders(tx)).toThrow(/genesis group has other outputs/);
  });
});
