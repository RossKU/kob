// Which inputs are read on the node before signing (node-verify.ts). The wallet's own plain KAS coins
// are not: their signature commits their values. Every covenant or token input still is.
import { describe, expect, it } from 'vitest';
import { buildBid, buildCancel, buildSend } from '../testing/wallet-fixtures';
import { confirmInputsOnNode, ownPlainInputs } from './node-verify';
import type { BuiltTx } from './types';

const clone = <T>(v: T): T => JSON.parse(JSON.stringify(v)) as T;
const toAddress = (spk: string) => `addr:${spk}`;

/** A node that lists exactly the given inputs, and records what it was asked. */
function nodeOf(built: BuiltTx, listed: (k: number) => boolean = () => true) {
  const asked: string[][] = [];
  return {
    asked,
    getUtxosByAddresses: async (addresses: string[]) => {
      asked.push(addresses);
      return built.tx.inputs.flatMap((i, k) => (listed(k) ? [{ address: toAddress(i.utxo.scriptPublicKey), transactionId: i.transactionId, index: i.index, amount: i.utxo.amount, scriptPublicKey: i.utxo.scriptPublicKey, covenantId: i.utxo.covenantId ?? null, blockDaaScore: '1', isCoinbase: false }] : []));
    },
  };
}

describe('ownPlainInputs', () => {
  it("takes a signed P2PK coin of the wallet, never a covenant or token input", () => {
    expect([...ownPlainInputs(buildBid())]).toEqual([0]);
    const send = buildSend();
    expect([...ownPlainInputs(send)]).toEqual([send.tx.inputs.findIndex((i) => !i.utxo.covenantId)]);
    // a cancel spends the order and its custody: both are covenant inputs
    expect([...ownPlainInputs(buildCancel())]).toEqual([]);
  });

  it('does not take a P2PK input that is not signed, is bound to a covenant, has another script or a redeem script', () => {
    const base = buildBid();
    const unsigned = clone(base);
    unsigned.sign = [];
    expect(ownPlainInputs(unsigned).size).toBe(0);
    const bound = clone(base);
    bound.tx.inputs[0]!.utxo.covenantId = 'cd'.repeat(32);
    expect(ownPlainInputs(bound).size).toBe(0);
    const other = clone(base);
    other.tx.inputs[0]!.utxo.scriptPublicKey = `000020${'ee'.repeat(32)}ac`;
    expect(ownPlainInputs(other).size).toBe(0);
    const p2sh = clone(base);
    p2sh.sign[0]!.redeemScript = 'ab';
    expect(ownPlainInputs(p2sh).size).toBe(0);
    const otherKey = clone(base);
    otherKey.sign[0]!.pubkey = 'ee'.repeat(32);
    expect(ownPlainInputs(otherKey).size).toBe(0);
  });
});

describe('confirmInputsOnNode with the own coins skipped', () => {
  it('does not ask the node when every input is an own plain coin', async () => {
    const built = buildBid();
    const down = { getUtxosByAddresses: () => Promise.reject(new Error('node down')) };
    const r = await confirmInputsOnNode(down, built.tx.inputs, toAddress, ownPlainInputs(built));
    expect(r.mismatches).toEqual([]);
  });

  it('still reads and compares every covenant input, and throws when the node cannot be asked', async () => {
    const built = buildSend();
    const skip = ownPlainInputs(built);
    const own = [...skip][0]!;
    // the own coin is missing on the node: not a mismatch
    const n = nodeOf(built, (k) => k !== own);
    const ok = await confirmInputsOnNode(n, built.tx.inputs, toAddress, skip);
    expect(ok.mismatches).toEqual([]);
    expect(n.asked.flat()).not.toContain(toAddress(built.tx.inputs[own]!.utxo.scriptPublicKey));
    // a token input is missing: a mismatch
    const bad = await confirmInputsOnNode(nodeOf(built, (k) => k === own), built.tx.inputs, toAddress, skip);
    expect(bad.mismatches.map((m) => m.reason)).toContain('missing');
    expect(bad.mismatches.every((m) => m.index !== own)).toBe(true);
    // an understated token input: a mismatch
    const low = clone(built);
    const cov = low.tx.inputs.findIndex((i) => i.utxo.covenantId);
    low.tx.inputs[cov]!.utxo.amount = (BigInt(low.tx.inputs[cov]!.utxo.amount) - 1n).toString();
    const lowR = await confirmInputsOnNode(nodeOf(built), low.tx.inputs, toAddress, ownPlainInputs(low));
    expect(lowR.mismatches).toMatchObject([{ index: cov, reason: 'amount' }]);
    const down = { getUtxosByAddresses: () => Promise.reject(new Error('node down')) };
    await expect(confirmInputsOnNode(down, built.tx.inputs, toAddress, skip)).rejects.toThrow('node down');
  });
});
