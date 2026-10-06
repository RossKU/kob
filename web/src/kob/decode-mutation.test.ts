// Regression: mutation sweep of decodeSigning over EVERY golden transaction (all order kinds, both token families, cancels,
// refunds, sends). Each operator makes the wallet sign something else than the plan; NO mutant may survive (a mutant is compared with the node
// facts of the ORIGINAL transaction, as the confirm screen does with what the node holds).
import { describe, expect, it } from 'vitest';
import { decodeSigning } from './decode';
import { parseRegistry } from './registry';
import type { ActionRequest, BuiltTx, Hex } from './types';
import { loadKobNode } from './wasm.node';
import { MAKER, OTHER, golden, goldenRequest, nodeFactsOf, tradableRegistryJson } from '../testing/chain-fixtures';

const kob = loadKobNode();
const registry = parseRegistry(tradableRegistryJson(), { kob });
const clone = <T>(v: T): T => JSON.parse(JSON.stringify(v)) as T;
const p2pkSpk = (pk: Hex) => `000020${pk}ac`;
const names = golden().transactions.map((t) => t.name);

function buildAll(): { name: string; built: BuiltTx }[] {
  const out: { name: string; built: BuiltTx }[] = [];
  for (const name of names) {
    try {
      out.push({ name, built: kob.build(goldenRequest<ActionRequest>(name, MAKER.pk)) });
    } catch {
      /* some golden requests need a non-maker key (send.tokens as OTHER): skipped, covered by decode.test.ts */
    }
  }
  return out;
}

type Op = { id: string; apply: (b: BuiltTx) => boolean };
const ops: Op[] = [
  {
    id: 'redirect the first maker-P2PK output to OTHER',
    apply: (b) => {
      const i = b.tx.outputs.findIndex((o) => o.scriptPublicKey === p2pkSpk(MAKER.pk) && !o.covenant);
      if (i < 0) return false;
      b.tx.outputs[i].scriptPublicKey = p2pkSpk(OTHER.pk);
      return true;
    },
  },
  {
    id: 'append an output to OTHER funded by shaving the last output (fee kept consistent)',
    apply: (b) => {
      const last = b.tx.outputs[b.tx.outputs.length - 1];
      if (last.covenant || BigInt(last.value) < 300_000_000n) return false;
      last.value = (BigInt(last.value) - 200_000_000n).toString();
      b.tx.outputs.push({ value: '200000000', scriptPublicKey: p2pkSpk(OTHER.pk), covenant: null });
      return true;
    },
  },
  {
    id: 'move 1 KAS from a maker output into the fee, fee report updated (consistent skim: above the absolute fee ceiling)',
    apply: (b) => {
      const i = b.tx.outputs.findIndex((o) => o.scriptPublicKey === p2pkSpk(MAKER.pk) && !o.covenant && BigInt(o.value) > 200_000_000n);
      if (i < 0) return false;
      b.tx.outputs[i].value = (BigInt(b.tx.outputs[i].value) - 100_000_000n).toString();
      b.fee.fee = (BigInt(b.fee.fee) + 100_000_000n).toString();
      return true;
    },
  },
  {
    id: 'sighash type 0x83 (SINGLE|ANYONECANPAY) on every request',
    apply: (b) => {
      if (!b.sign.length) return false;
      b.sign.forEach((s) => (s.sighashType = 0x83));
      return true;
    },
  },
  {
    id: 'sign request for OTHER key',
    apply: (b) => {
      if (!b.sign.length) return false;
      b.sign[0].pubkey = OTHER.pk;
      return true;
    },
  },
  {
    id: 'extra sign request on an input that needs no signature',
    apply: (b) => {
      const i = b.plans.findIndex((p, k) => p.kind !== 'p2pk' && !b.sign.some((s) => s.inputIndex === k) && (p.kind === 'tokenDelegator' || p.kind === 'entry'));
      if (i < 0) return false;
      b.sign.push({ inputIndex: i, pubkey: MAKER.pk, sighashType: 1, sighash: '00'.repeat(32), redeemScript: null });
      return true;
    },
  },
  {
    id: 'strip the payload (order becomes invisible)',
    apply: (b) => {
      if (!b.tx.payload) return false;
      b.tx.payload = '';
      return true;
    },
  },
  {
    id: 'rebind an order output to another covenant id',
    apply: (b) => {
      const c = b.covenants.find((x) => x.template);
      if (!c) return false;
      const o = b.tx.outputs[c.outputs[0]];
      o.covenant = { authorizingInput: o.covenant!.authorizingInput, covenantId: 'ab'.repeat(32) };
      return true;
    },
  },
  {
    id: 'inflate an unsigned covenant input amount (fee under-reported to the user)',
    apply: (b) => {
      const i = b.plans.findIndex((p, k) => p.kind !== 'p2pk' && !b.sign.some((s) => s.inputIndex === k));
      if (i < 0) return false;
      b.tx.inputs[i].utxo.amount = (BigInt(b.tx.inputs[i].utxo.amount) + 5_000_000_000n).toString();
      return true;
    },
  },
  {
    id: 'swap the scripts of two outputs',
    apply: (b) => {
      if (b.tx.outputs.length < 2) return false;
      const [a, c] = [b.tx.outputs[0], b.tx.outputs[b.tx.outputs.length - 1]];
      if (a.scriptPublicKey === c.scriptPublicKey) return false;
      [a.scriptPublicKey, c.scriptPublicKey] = [c.scriptPublicKey, a.scriptPublicKey];
      return true;
    },
  },
  {
    id: 'change the maker inside a plan-declared token next state',
    apply: (b) => {
      const i = b.plans.findIndex((p) => p.kind === 'tokenLeader' && p.nextStates.some((s) => s.owner_scheme === 0));
      if (i < 0) return false;
      const p = b.plans[i] as Extract<BuiltTx['plans'][number], { kind: 'tokenLeader' }>;
      const k = p.nextStates.findIndex((s) => s.owner_scheme === 0);
      p.nextStates[k].owner = OTHER.pk; // plan says the tokens go to OTHER, outputs untouched
      return true;
    },
  },
];

describe('mutation sweep over all golden transactions', () => {
  const all = buildAll();
  it('builds a healthy corpus', () => {
    expect(all.length).toBeGreaterThan(40);
  });
  for (const op of ops) {
    it(op.id, () => {
      const survivors: string[] = [];
      let applied = 0;
      for (const { name, built } of all) {
        const b = clone(built);
        if (!op.apply(b)) continue;
        applied++;
        let ok: boolean;
        try {
          ok = decodeSigning({ kob, built: b, maker: MAKER.pk, registry, nodeInputs: nodeFactsOf(built) }).ok;
        } catch {
          ok = false; // throwing = the confirm screen crashes = nothing is signed
        }
        if (ok) survivors.push(name);
      }
      expect(applied, 'operator applied to something').toBeGreaterThan(0);
      expect(survivors).toEqual([]);
    });
  }
});
