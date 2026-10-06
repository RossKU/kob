// A stub `KobWasm` returning canned payloads (the real one is wasm-node.ts). It builds structurally valid artifacts
// whose transaction id is a deterministic hash, so the SDK's orchestration can be tested without any Rust.

import { sha256Hex } from '../../src/canonical.ts';
import { BINDING_SWAP, AUTH_VERSION_PAYLOAD, AUTH_VERSION_SIGNED, PAYLOAD_EXACT_TX } from '../../src/types.ts';
import type { InputSignature, KobWasm, PayRequest, PayResult, PreflightResult } from '../../src/wasm.ts';
import type { PaymentPayload } from '../../src/types.ts';

type SignReq = { inputIndex: number; pubkey: string; sighashType: number; sighash: string; redeemScript: string | null };

export interface StubWasm extends KobWasm {
  calls: { method: string; arg: unknown }[];
  /** Set to make `preflight` answer this. */
  preflightResult: PreflightResult;
  /** Set to make the pay* functions throw. */
  failWith: string | undefined;
  /** Mutates the built payload (to simulate a misbehaving builder). */
  tamper: ((p: PayResult) => void) | undefined;
}

function firstOutpoint(v: unknown): { txid: string; index: number } | undefined {
  if (Array.isArray(v)) for (const x of v) { const r = firstOutpoint(x); if (r) return r; }
  else if (v !== null && typeof v === 'object') {
    const r = v as Record<string, unknown>;
    if (typeof r.transactionId === 'string' && typeof r.index === 'number') return { txid: r.transactionId, index: r.index };
    for (const x of Object.values(r)) { const f = firstOutpoint(x); if (f) return f; }
  }
  return undefined;
}

export const spkOf = (address: string): string => '000020' + sha256Hex('spk:' + address) + 'ac';

export function stubWasm(): StubWasm {
  const calls: StubWasm['calls'] = [];
  let counter = 0;

  const canned = (kind: 'native' | 'kcc20' | 'swap', req: PayRequest): PayResult => {
    const txid = sha256Hex(`tx:${kind}:${req.paymentId}:${req.requestHash}:${++counter}`);
    const expiresAtMs = req.nowMs + Math.min(req.requirements.maxTimeoutSeconds, 60) * 1000;
    const tok = (req.tokenUtxos?.[0] as { transactionId: string; index: number } | undefined);
    const order = firstOutpoint(req.quote);
    const inputs: { txid: string; index: number }[] = [];
    if (order) inputs.push(order);
    if (tok && kind !== 'native') inputs.push({ txid: tok.transactionId, index: tok.index });
    if (kind === 'native' || kind === 'kcc20' || inputs.length === 0 || req.payAsset === 'KAS') inputs.push({ txid: req.utxos[0]!.txid, index: req.utxos[0]!.index });
    const transaction = JSON.stringify({ id: txid, version: kind === 'native' ? 0 : 1, inputs, outputs: [{ value: req.requirements.amount }], storageMass: '1' });
    const auth =
      kind === 'native'
        ? { version: AUTH_VERSION_SIGNED, inputIndex: 0, expiresAt: new Date(expiresAtMs).toISOString(), digest: sha256Hex('digest' + txid), signature: '11'.repeat(64) }
        : { version: AUTH_VERSION_PAYLOAD, expiresAt: new Date(expiresAtMs).toISOString(), digest: sha256Hex('digest' + txid) };
    const paymentPayload: PaymentPayload = {
      x402Version: 2,
      accepted: req.requirements,
      payload: {
        type: PAYLOAD_EXACT_TX,
        profile: req.requirements.extra.profile,
        payerAddress: req.payerAddress,
        transaction,
        transactionEncoding: req.requirements.extra.transactionEncoding,
        paymentOutputIndex: 0,
        requestHash: req.requestHash,
        authorization: auth,
        ...(kind === 'swap' ? { route: { binding: BINDING_SWAP, payAsset: req.payAsset ?? '', orders: [{ txid: '99'.repeat(32), index: 0 }] } } : {}),
      },
      ...(req.resource ? { resource: req.resource } : {}),
      ...(req.extensions ? { extensions: req.extensions } : {}),
    };
    return { paymentPayload, transactionId: txid, consumed: inputs, feeSompi: '2000', expiresAtMs };
  };

  const wasm: StubWasm = {
    calls,
    preflightResult: { ok: true },
    failWith: undefined,
    tamper: undefined,
    version: () => 'stub',
    addressToScriptPublicKey(address) {
      calls.push({ method: 'addressToScriptPublicKey', arg: address });
      return spkOf(address);
    },
    resolveToken(_network, asset) {
      return { templateHash: sha256Hex('tpl:' + asset), extensionCommitment: sha256Hex('ext:' + asset) };
    },
    tokenOffer(req) {
      calls.push({ method: 'tokenOffer', arg: req });
      const t: ReturnType<KobWasm['tokenOffer']> = {
        family: 'kcc20',
        templateHash: req.templateHash ?? sha256Hex('tpl:' + req.asset),
        extensionCommitment: req.extensionCommitment ?? sha256Hex('ext:' + req.asset),
        custody: req.custody,
        carrier: req.carrier ?? '100000000',
        tokenScriptPublicKey: '0000aa20' + sha256Hex('tok:' + req.asset + req.payTo) + '87',
      };
      if (req.ticker) t.ticker = req.ticker;
      if (req.decimals !== undefined) t.decimals = req.decimals;
      return t;
    },
    buildKcc20Unsigned: (req) => unsigned('buildKcc20Unsigned', req),
    finishKcc20: (u, sigs) => finish('finishKcc20', 'kcc20', u, sigs),
    prepareSwap: (req) => ({ ...unsigned('prepareSwap', req), payAsset: req.payAsset ?? '', payerSpent: '1', warnings: [] }),
    finishSwap: (p, sigs) => finish('finishSwap', 'swap', p, sigs),
    payNative: (req) => run('payNative', 'native', req),
    payKcc20: (req) => run('payKcc20', 'kcc20', req),
    paySwap: (req) => run('paySwap', 'swap', req),
    preflight(req) {
      calls.push({ method: 'preflight', arg: req });
      return wasm.preflightResult;
    },
    revoke(req) {
      calls.push({ method: 'revoke', arg: req });
      if (wasm.failWith) throw new Error(wasm.failWith);
      const tx = JSON.parse(req.paymentPayload.payload.transaction) as { inputs: { txid: string; index: number }[] };
      const spent = tx.inputs[0]!;
      const id = sha256Hex('revoke:' + spent.txid);
      return { transaction: JSON.stringify({ id, inputs: [spent] }), transactionId: id, spent };
    },
    prepareRevoke(req) {
      calls.push({ method: 'prepareRevoke', arg: req });
      if (wasm.failWith) throw new Error(wasm.failWith);
      const tx = JSON.parse(req.paymentPayload.payload.transaction) as { inputs: { txid: string; index: number }[] };
      const spent = tx.inputs[0]!;
      return { built: { sign: [{ inputIndex: 0, pubkey: req.payerPublicKey, sighashType: 1, sighash: sha256Hex('revoke-sighash' + spent.txid), redeemScript: null }] }, spent };
    },
    finishRevoke(prepared, sigs) {
      calls.push({ method: 'finishRevoke', arg: { prepared, sigs } });
      if (sigs.length !== prepared.built.sign.length) throw new Error('one signature per request');
      const id = sha256Hex('revoke:' + prepared.spent.txid);
      return { transaction: JSON.stringify({ id, inputs: [prepared.spent] }), transactionId: id, spent: prepared.spent };
    },
  };

  function unsigned(method: string, req: PayRequest): { built: { sign: SignReq[]; _req: PayRequest }; template: unknown } {
    calls.push({ method, arg: req });
    if (wasm.failWith) throw new Error(wasm.failWith);
    if (!req.payerPublicKey) throw new Error('wallet flow needs payerPublicKey');
    return { built: { sign: [{ inputIndex: 0, pubkey: req.payerPublicKey, sighashType: 1, sighash: sha256Hex('sighash' + req.paymentId), redeemScript: null }], _req: req }, template: {} };
  }

  function finish(method: string, kind: 'kcc20' | 'swap', u: { built: unknown }, sigs: InputSignature[]): PayResult {
    calls.push({ method, arg: sigs });
    const built = u.built as { sign: SignReq[]; _req: PayRequest };
    if (sigs.length !== built.sign.length) throw new Error('signature count differs from the sign requests');
    const r = canned(kind, built._req);
    wasm.tamper?.(r);
    return r;
  }

  function run(method: string, kind: 'native' | 'kcc20' | 'swap', req: PayRequest): PayResult {
    calls.push({ method, arg: req });
    if (wasm.failWith) throw new Error(wasm.failWith);
    const r = canned(kind, req);
    wasm.tamper?.(r);
    return r;
  }
  return wasm;
}
