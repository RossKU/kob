// KOB wallet-gate contract helpers: KCC-20 reference token + BidOrder (SilverScript v1.0.0).
// Pure JS except where a kaspa-wasm namespace `k` is passed in (P2SH script hashing).
import { Abi, concat, eq, hex, unhex, kcc20Redeem, kcc20State, OWNER_P2PK_SCHNORR } from './script.mjs';

export const KAS = 100_000_000n;
/** Extension commitment (pinned token sub-type) of the gate token. */
export const EXT_HEX = 'ee'.repeat(32);
export const SENTINEL_MAKER = '03'.repeat(32);
export const SENTINEL_TOKCOV = '70'.repeat(32);

/** Constants baked into the BidOrder template (only maker + tokenCovId are patched at runtime). */
export const BID_PARAMS = {
  lotSize: 1000n,
  lotPrice: 25_000_000n, // 0.25 KAS per lot
  reserve: 100_000_000n, // 1 KAS never spent on fills
  deliveryCarrier: 200_000_000n, // 2 KAS on each delivered token UTXO
  expiryDaa: 900_000_000n, // ~ Sep 2027 on TN10 at 10 bps
  refundTip: 10_000_000n,
};

/** silverc constructor-args JSON for BidOrder (order = contract parameter order). */
export function bidCtorArgs({ maker, tokenCovId, tplHash, kcc20 }) {
  const b = (h) => ({ kind: 'bytes', value: Array.from(unhex(h)) });
  const i = (v) => ({ kind: 'int', value: Number(v) });
  const bc = kcc20.compiled;
  const prefixLen = bc.state_span.offset;
  const suffixLen = bc.bytecode.length - bc.state_span.offset - bc.state_span.len;
  return [
    b(maker), b(tokenCovId), b(tplHash), i(prefixLen), i(suffixLen), b(EXT_HEX),
    i(BID_PARAMS.lotSize), i(BID_PARAMS.lotPrice), i(BID_PARAMS.reserve), i(BID_PARAMS.deliveryCarrier),
    i(BID_PARAMS.expiryDaa), i(BID_PARAMS.refundTip),
  ];
}

function replaceAll(bytes, from, to) {
  if (from.length !== to.length) throw new Error('patch must keep length');
  const out = bytes.slice();
  let n = 0;
  for (let i = 0; i + from.length <= out.length; i++) {
    let m = true;
    for (let j = 0; j < from.length; j++) if (out[i + j] !== from[j]) { m = false; break; }
    if (m) { out.set(to, i); i += from.length - 1; n++; }
  }
  return { out, n };
}

/** Loads the KCC-20 reference artifact; verifies the state span really holds the ctor state. */
export function loadToken(kcc20Artifact) {
  const abi = new Abi(kcc20Artifact, 'KCC20');
  const span = abi.stateSpan;
  if (span.len !== 112) throw new Error('unexpected KCC20 state span ' + JSON.stringify(span));
  return abi;
}

/** BidOrder redeem script for `maker` (x-only pubkey hex) bidding on token `tokenCovId` (hex). */
export function bidRedeem(bidTemplateArtifact, { maker, tokenCovId }) {
  const abi = new Abi(bidTemplateArtifact, 'BidOrder');
  let bc = abi.bytecode;
  for (const [from, to, what] of [[SENTINEL_MAKER, maker, 'maker'], [SENTINEL_TOKCOV, tokenCovId, 'tokenCovId']]) {
    if (unhex(to).length !== 32) throw new Error(what + ' must be 32 bytes');
    const r = replaceAll(bc, unhex(from), unhex(to));
    if (r.n < 1) throw new Error('sentinel for ' + what + ' not found in template');
    bc = r.out;
  }
  return { abi, redeem: bc };
}

/** ScriptPublicKey (wasm) for P2SH of a redeem script. */
export const p2shSpk = (k, redeem) => k.payToScriptHashScript(hex(redeem));
/** Standard Schnorr P2PK script public key hex: OP_DATA32 <pk> OP_CHECKSIG. */
export const p2pkScriptHex = (pubkeyHex) => '20' + pubkeyHex + 'ac';
export const p2pkSpk = (k, pubkeyHex) => new k.ScriptPublicKey(0, p2pkScriptHex(pubkeyHex));
export const addressOfPubkey = (k, pubkeyHex, network = 'testnet') =>
  k.addressFromScriptPublicKey(new k.ScriptPublicKey(0, p2pkScriptHex(pubkeyHex)), network).toString();
/** x-only pubkey hex of a Schnorr P2PK kaspa address. */
export function pubkeyOfAddress(k, address) {
  const a = new k.Address(address);
  if (a.version !== 'PubKey') throw new Error('not a Schnorr P2PK address: ' + address);
  return k.payToAddressScript(a).script.slice(2, 66);
}

export { kcc20Redeem, kcc20State, OWNER_P2PK_SCHNORR, concat, eq, hex, unhex };

/** Sigscript builders -------------------------------------------------------------------------*/

/** BidOrder.cancel(sig s): <sig65> <tag> <redeem>. sig65 = 64B schnorr + sighash byte. */
export function bidCancelSigscript(bid, sig65) {
  return bid.abi.sigscript('cancel', [sig65], bid.redeem);
}
/** Prefix (args + tag) only, for wallets that append the redeem script themselves. */
export function bidCancelArgsAndTag(bid, sig65) {
  return bid.abi.argsAndTag('cancel', [sig65]);
}

/** KCC-20 `transfer(State[] next_states, bytes witness)` with owner scheme 0x00: witness = 0x00 || sig65. */
export function kcc20TransferArgs(nextStates, sig65) {
  return [nextStates, concat(Uint8Array.of(0x00), sig65)];
}
export function kcc20TransferSigscript(tokenAbi, redeem, nextStates, sig65) {
  return tokenAbi.sigscript('transfer', kcc20TransferArgs(nextStates, sig65), redeem);
}
