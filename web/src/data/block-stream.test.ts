// Block notifications -> the transactions kob-wasm reads (standalone mode). The wRPC shape below is what a public testnet-10 node (rusty-kaspa 2.1.0,
// JSON encoding) sent on 2026-11-05: 64-bit values as bigint, script public keys as `version + script` hex, covenant bindings on outputs, the id in
// `verboseData`.
import { describe, expect, it } from 'vitest';
import { isCoinbaseTx, streamBlockOf, txJsonOfBlockTx } from './block-stream';

const SPK = '000020a39404c7f7a004b5eb51966981abeab40df55abf6f631c48399a42066bf182faac';
const COV = 'd4b9fdf0cf057aecb9e7851da838171f3c4d71e14f82d89a5cb4565969151d9e';
const rpcTx = {
  version: 1,
  inputs: [{ previousOutpoint: { transactionId: '146624ed39d1dbc92a2b6d2b09f31fbb73da88838894d9ccc66b6341c9825b71', index: 1 }, signatureScript: '41b8', sequence: 0n, sigOpCount: 0, computeBudget: 10 }],
  outputs: [
    { value: 2570689007763n, scriptPublicKey: SPK, verboseData: { scriptPublicKeyType: 'PubKey', scriptPublicKeyAddress: 'kaspatest:qz3' }, covenant: { authorizingInput: 0, covenantId: COV } },
    { value: 5n, scriptPublicKey: { version: 0, script: 'AB' } },
  ],
  lockTime: 0n, subnetworkId: '0000000000000000000000000000000000000000', gas: 0n, payload: '', storageMass: 2n, mass: 2n,
  verboseData: { transactionId: '8f498f3ec93aa3d6a3e41817e9d42772468dbb57bd7c72f67a3b291cfc44b0ac', hash: '96e6', computeMass: 2106n, blockHash: 'a0a2', blockTime: 1793884825311n },
};
const coinbase = { ...rpcTx, inputs: [], subnetworkId: '0100000000000000000000000000000000000000', payload: 'f143', verboseData: { ...rpcTx.verboseData, transactionId: 'c4'.repeat(32) } };
const block = {
  header: { hash: '0d2408f5a9280ae694d9e3f233d4b8accb105680ac289750df83988072df6f2b', version: 2, daaScore: 603866097n, timestamp: 1793884824880n },
  transactions: [coinbase, rpcTx],
  verboseData: { hash: '0d2408f5a9280ae694d9e3f233d4b8accb105680ac289750df83988072df6f2b', isChainBlock: false },
};

describe('txJsonOfBlockTx', () => {
  it('reads a wRPC transaction: decimal strings, covenant bindings, empty input entries, the id from verboseData', () => {
    const tx = txJsonOfBlockTx(rpcTx)!;
    expect(tx.id).toBe('8f498f3ec93aa3d6a3e41817e9d42772468dbb57bd7c72f67a3b291cfc44b0ac');
    expect(tx.inputs[0]).toEqual({
      transactionId: '146624ed39d1dbc92a2b6d2b09f31fbb73da88838894d9ccc66b6341c9825b71', index: 1, sequence: '0', sigOpCount: 0, computeBudget: 10, signatureScript: '41b8',
      utxo: { address: null, amount: '0', scriptPublicKey: '', blockDaaScore: '0', isCoinbase: false, covenantId: null },
    });
    expect(tx.outputs[0]).toEqual({ value: '2570689007763', scriptPublicKey: SPK, covenant: { authorizingInput: 0, covenantId: COV } });
    expect(tx.outputs[1]).toEqual({ value: '5', scriptPublicKey: '0000ab', covenant: null });
    expect(tx.storageMass).toBe('2');
  });

  it('reads the mock node layout (64-bit values as strings) the same way, and an SDK Hash object as a covenant id', () => {
    const asStrings = JSON.parse(JSON.stringify(rpcTx, (_k, v) => (typeof v === 'bigint' ? v.toString() : v)));
    expect(txJsonOfBlockTx(asStrings)).toEqual(txJsonOfBlockTx(rpcTx));
    const hashObj = { ...rpcTx, outputs: [{ ...rpcTx.outputs[0], covenant: { authorizingInput: 0, covenantId: { toString: () => COV.toUpperCase() } } }] };
    expect(txJsonOfBlockTx(hashObj)!.outputs[0]!.covenant).toEqual({ authorizingInput: 0, covenantId: COV });
  });

  it('skips a transaction it cannot read whole (no id, a broken output, a bad covenant id)', () => {
    expect(txJsonOfBlockTx({ ...rpcTx, verboseData: undefined })).toBeNull();
    expect(txJsonOfBlockTx({ ...rpcTx, outputs: [{ value: -1n, scriptPublicKey: SPK }] })).toBeNull();
    expect(txJsonOfBlockTx({ ...rpcTx, outputs: [{ value: 1n, scriptPublicKey: SPK, covenant: { authorizingInput: 0, covenantId: 'zz' } }] })).toBeNull();
    expect(txJsonOfBlockTx({ ...rpcTx, payload: 'not hex' })).toBeNull();
  });
});

describe('streamBlockOf', () => {
  it('keeps the header facts and every transaction but the coinbase', () => {
    const b = streamBlockOf(block)!;
    expect(b.hash).toBe(block.header.hash);
    expect(b.daaScore).toBe(603866097n);
    expect(b.timestampMs).toBe(1793884824880);
    expect(b.transactions.map((t) => t.id)).toEqual([rpcTx.verboseData.transactionId]);
    expect(isCoinbaseTx(txJsonOfBlockTx(coinbase)!)).toBe(true);
  });

  it('refuses a notification without a readable header', () => {
    expect(streamBlockOf({ transactions: [] })).toBeNull();
    expect(streamBlockOf({ header: { hash: 'x', daaScore: 1n }, transactions: [] })).toBeNull();
    expect(streamBlockOf(null)).toBeNull();
  });
});
