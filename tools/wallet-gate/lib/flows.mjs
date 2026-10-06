// Wallet-independent test flows T1/T2/T3 (used by the browser page and by the node self-check/e2e).
// A "test" object carries: plan, unsigned wasm tx, the redeem script, and how to assemble the final sigscript
// from a 65-byte (sig64 + sighash) signature returned by whatever signed input 0.
import * as S from './script.mjs';
import * as C from './contracts.mjs';
import * as T from './txbuild.mjs';

export const SIGHASH_ALL = 1; // wallet-facing sighash type (KasWare/Kaspire numbering: 1 = SIGHASH_ALL)
export const BUDGET = { p2pk: 15, cancel: 20, transfer: 30 }; // > measured 10 / 10 / 13 (harness), cheap headroom

/** Rebuilds redeem scripts from a setup file and checks they hash to the P2SH addresses recorded by setup. */
export function deriveContext(k, setup, kcc20Artifact, bidTemplate) {
  const tokenAbi = C.loadToken(kcc20Artifact);
  const tokenState = C.kcc20State({ amount: setup.tokenUnits, owner: setup.walletPubkey, ext: setup.extHex });
  const tokenRedeem = C.kcc20Redeem(tokenAbi, tokenState);
  const tokenSpk = C.p2shSpk(k, tokenRedeem);
  const bid = C.bidRedeem(bidTemplate, { maker: setup.walletPubkey, tokenCovId: setup.tokenCovId });
  const bidSpk = C.p2shSpk(k, bid.redeem);
  const addr = (spk) => k.addressFromScriptPublicKey(spk, 'testnet-10').toString();
  const checks = {
    tokenAddressMatches: setup.tokens.every((t) => t.address === addr(tokenSpk)),
    bidAddressMatches: setup.bids.every((b) => b.address === addr(bidSpk)),
  };
  return { setup, tokenAbi, tokenRedeem, tokenSpk, bid, bidSpk, checks };
}

const dummySig65 = () => new Uint8Array(65).fill(1);

/** T1: plain P2PK spend of `utxoEntry` (wallet's own UTXO) back to itself, v1 tx with computeBudget. */
export function buildT1(k, ctx, walletUtxo) {
  const inp = T.utxoToInput(walletUtxo, BUDGET.p2pk);
  const spk = C.p2pkScriptHex(ctx.setup.walletPubkey);
  const plan = { inputs: [inp], outputs: [{ value: 0n, spk }] };
  const fee = T.minFee(plan, [66]);
  plan.outputs[0].value = inp.amount - fee;
  return finishTest(k, { id: 'T1', name: 'P2PK spend, tx v1 + computeBudget', plan, fee, kind: 'p2pk', outAddress: ctx.setup.walletAddress, outIndex: 0 });
}

/** T2: BidOrder.cancel(sig) - covenant (P2SH) input signed by the wallet key; funds return to the wallet. */
export function buildT2(k, ctx, bidEntry, bidUtxo) {
  const inp = T.utxoToInput(bidUtxo, BUDGET.cancel);
  const redeem = ctx.bid.redeem;
  const spk = C.p2pkScriptHex(ctx.setup.walletPubkey);
  const plan = { inputs: [inp], outputs: [{ value: 0n, spk }] };
  const ssLen = C.bidCancelSigscript(ctx.bid, dummySig65()).length;
  const fee = T.minFee(plan, [ssLen]);
  plan.outputs[0].value = inp.amount - fee;
  return finishTest(k, {
    id: 'T2', name: 'BidOrder.cancel(sig): covenant input', plan, fee, kind: 'bid-cancel', redeem,
    outAddress: ctx.setup.walletAddress, outIndex: 0,
    assemble: (sig65) => S.hex(C.bidCancelSigscript(ctx.bid, sig65)),
    // Kaspire ordered-args template (wallet emits args + redeem itself)
    kaspire: {
      scriptHex: S.hex(redeem),
      signatureScript: { mode: 'ordered-args', args: [{ type: 'signature', prefixHex: '' }, { type: 'data', hex: S.hex(ctx.bid.abi.dispatchTag('cancel')) }] },
    },
  });
}

/** T3: KCC-20 transfer of a token UTXO owned by the wallet key to `recipientPubkey` (owner witness = 0x00 || sig). */
export function buildT3(k, ctx, tokenUtxo, recipientPubkey) {
  const inp = T.utxoToInput(tokenUtxo, BUDGET.transfer);
  if (!inp.covenantId) throw new Error('token UTXO has no covenantId (not a covenant UTXO?)');
  const amount = BigInt(ctx.setup.tokenUnits);
  const next = [C.kcc20State({ amount, owner: recipientPubkey, ext: ctx.setup.extHex })];
  const outRedeem = C.kcc20Redeem(ctx.tokenAbi, next[0]);
  const outSpk = C.p2shSpk(k, outRedeem);
  const plan = { inputs: [inp], outputs: [{ value: 0n, spk: outSpk.script, covenant: { auth: 0, id: inp.covenantId } }] };
  const ssLen = C.kcc20TransferSigscript(ctx.tokenAbi, ctx.tokenRedeem, next, dummySig65()).length;
  const fee = T.minFee(plan, [ssLen]);
  plan.outputs[0].value = inp.amount - fee;
  const abi = ctx.tokenAbi;
  const fields = abi.contract.runtime_state.fields;
  const hexOf = (b) => S.hex(b);
  return finishTest(k, {
    id: 'T3', name: 'KCC-20 transfer: owner witness on token input', plan, fee, kind: 'kcc20-transfer', redeem: ctx.tokenRedeem,
    outAddress: k.addressFromScriptPublicKey(outSpk, 'testnet-10').toString(), outIndex: 0,
    assemble: (sig65) => S.hex(C.kcc20TransferSigscript(abi, ctx.tokenRedeem, next, sig65)),
    kaspire: {
      scriptHex: S.hex(ctx.tokenRedeem),
      signatureScript: {
        mode: 'ordered-args',
        args: [
          // next_states pushed field-wise (one array payload per field), then witness = 0x00 || <sig>, then dispatch tag
          ...fields.map((f) => ({ type: 'data', hex: hexOf(fieldPayload(abi, f, next)) })),
          { type: 'signature', prefixHex: '00' },
          { type: 'data', hex: S.hex(abi.dispatchTag('transfer')) },
        ],
      },
    },
  });
}

function fieldPayload(abi, f, next) {
  // same bytes as Abi.pushArg pushes for `dynamic_array of f.type`, minus the push opcode
  const parts = next.map((st) => {
    const v = st[f.name];
    return payloadOf(f.type, v);
  });
  return S.concat(new Uint8Array(0), ...parts);
}
function payloadOf(ty, v) {
  switch (ty.kind) {
    case 'int': return S.serializeI64(v, 8);
    case 'byte': return Uint8Array.of(Number(v));
    case 'bool': return Uint8Array.of(v ? 1 : 0);
    case 'pubkey': case 'fixed_bytes': return typeof v === 'string' ? S.unhex(v) : v;
    default: throw new Error('payloadOf ' + ty.kind);
  }
}

function finishTest(k, t) {
  t.tx = T.toWasmTx(k, t.plan);
  t.inputIndex = 0;
  t.txJson = () => t.tx.serializeToSafeJSON();
  if (!t.assemble) t.assemble = (sig65) => S.hex(S.concat(Uint8Array.of(65), sig65)); // plain P2PK: push(sig65)
  return t;
}

/** Local (no wallet) signature over input 0: 65 bytes = sig64 + sighash byte. */
export function signLocal(k, t, privateKey) {
  const scriptHex = k.createInputSignature(t.tx, t.inputIndex, privateKey, k.SighashType.All);
  const b = S.unhex(scriptHex);
  if (b.length !== 66 || b[0] !== 0x41) throw new Error('unexpected createInputSignature output ' + scriptHex);
  return b.slice(1);
}

/** Puts the assembled sigscript into input 0 and returns the tx (ready for submitTransaction). */
export function finalize(t, sig65) {
  const ss = t.assemble(sig65);
  t.tx.inputs[t.inputIndex].signatureScript = ss;
  return { tx: t.tx, sigscript: ss };
}

/** Signature from a wallet-signed tx (safe JSON string): first 65-byte push, or 0x00||sig (66) for KCC-20 witnesses. */
export function sigFromSignedTx(signedTxJson, index = 0) {
  const obj = typeof signedTxJson === 'string' ? JSON.parse(signedTxJson) : signedTxJson;
  const ss = obj.inputs?.[index]?.signatureScript;
  if (!ss) throw new Error('wallet returned no signatureScript for input ' + index);
  const pushes = S.parsePushes(S.unhex(ss));
  const p65 = pushes.find((p) => p.length === 65);
  if (p65) return { sig65: p65, walletSigscript: ss };
  const p66 = pushes.find((p) => p.length === 66 && p[0] === 0x00);
  if (p66) return { sig65: p66.slice(1), walletSigscript: ss };
  throw new Error(`no signature push found in returned sigscript (pushes: ${pushes.map((p) => p.length).join(',')})`);
}
