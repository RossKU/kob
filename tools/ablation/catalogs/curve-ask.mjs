// KobCurveAsk (docs/design/curve-ask-spec.md): contracts/orders, test binary kob_curve_ask_tests.
//
// There is one entry per require of the contract, plus the walk's level step, its rounding and the
// unbound path.
//
// Checks without a witness attack (not in the catalog):
// - Cancel `require(sb.length == 65)`. As in KobAsk, `sb[64]` of a shorter signature fails on its
//   own, and a longer one is no Schnorr signature for checkSig.
// - `require(levelSize > 0)`. A size of 0 fails the division. A negative size walks away from
//   `end`, and `pos == end` refuses it.
// - `require(n <= amountLeft)`. A larger n only pays for units the custody does not hold, because
//   the token program keeps the amounts. A curve is no trigger evidence.
// - `cid = OpInputCovenantId(tokenIn)` (the unbound path). Without it `cid` stays zero and every
//   unbound fill fails.
//
// Defence in depth: the custody's owner and scheme checks both refuse a key-owned custody. Each
// holds it (NK6), both together flip it (K6x).
import { del, rep } from '../lib/edits.mjs';

export const suite = {
  name: 'curve-ask',
  family: 'KCC-20 curve',
  testBin: 'kob_curve_ask_tests',
  srcDir: 'contracts/orders',
  templateMarker: null,
  expectedTemplates: 1,
};

const T_NEG = 'curve_negatives';
const T_CUS = 'curve_custody_negatives';
const T_SLOT = 'curve_stray_guard_slots_8x8';
const T_CAN = 'curve_cancel_negatives';
const T_GEN = 'curve_created_in_the_token_genesis';
const F = 'KobCurveAsk';

const slot = k => ({
  id: `K-S${k}`,
  file: F,
  run: { [T_SLOT]: [`NS${k}`] },
  note: `noStrays scan slot ${k}: a stray of the curve at token slot ${k} rides to the taker`,
  edits: [rep(`require(byte[32](OpTxInputScriptSigSubstr(x${k}, e${k}, e${k} + 32)) != me);`, 'require(true);')],
});

export const mutations = [
  { id: 'K1', file: F, run: { [T_CUS]: ['NK2'] }, note: 'tplRs: the custody input is the P2SH of the holder script it pushes (a planted UTXO)', edits: [del('        require(tx.inputs[idx].scriptPubKey == byte[](new ScriptPubKeyP2SHFromRedeemScript(rs)));\n')] },
  { id: 'K2', file: F, run: { [T_CUS]: ['NK1'] }, note: "tplRs: the custody's template is the token's (a look-alike holder)", edits: [del('        require(blake3(byte[](pre as byte[8]) + rs.slice(0, pre) + byte[](suf as byte[8]) + rs.slice(pre + stLen, size)) == tpl);\n')] },
  { id: 'K3', file: F, run: { [T_SLOT]: ['NS8'] }, inputOnly: ['NS8'], note: 'noStrays bound: at most 8 token inputs (the 8 / 8 program refuses a 9th on its own)', edits: [del('        require(cnt <= MAX_TOK_IN);\n')] },
  ...[0, 1, 2, 3, 4, 5, 6, 7].map(slot),
  { id: 'K4', file: F, run: { [T_NEG]: ['NC23'] }, note: "contSpk: the bytes rsLen reads are this input's redeem script (a wrong rsLen continues another script)", edits: [del('        require(tx.inputs[self].scriptPubKey == byte[](new ScriptPubKeyP2SHFromRedeemScript(rs)));\n')] },
  { id: 'K5', file: F, run: { [T_CUS]: ['NK3'] }, note: 'the curve is the only input of its covenant id', edits: [del('        require(OpCovInputCount(selfId) == 1);\n')] },
  { id: 'K6', file: F, run: { [T_GEN]: ['NB1'] }, note: "unbound: the custody comes from the order UTXO's own transaction", edits: [del('            require(tx.inputs[tokenIn].outpointTxId == tx.inputs[self].outpointTxId);\n')] },
  { id: 'K7', file: F, run: { [T_GEN]: ['NB3'] }, note: 'only an unbound curve (tokenCovId zero) takes its token from the custody', edits: [rep('        if (cid == byte[32](z)) {', '        if (true) {')] },
  { id: 'K8', file: F, run: { [T_CUS]: ['NK4'] }, note: "the custody carries the curve's token (NB3, a bound curve's other-token custody, is also refused by the rest's covenant binding)", edits: [del('        require(OpInputCovenantId(tokenIn) == cid);\n')] },
  { id: 'K9', file: F, run: { [T_CUS]: ['NK5'] }, hold: { [T_CUS]: ['NK6'] }, note: "the custody is owned by the curve's covenant id", edits: [del('        require(byte[32](tin.slice(10, 42)) == selfId);\n')] },
  { id: 'K10', file: F, run: { [T_CUS]: ['NK7'] }, hold: { [T_CUS]: ['NK6'] }, note: 'the custody has owner scheme 0x04 and borrowing disabled', edits: [del('        require(tin.slice(43, 46) == byte[](0x040100));\n')] },
  { id: 'K10x', file: F, run: { [T_CUS]: ['NK6'] }, note: 'owner and scheme removed together: a key-owned custody', edits: [del('        require(byte[32](tin.slice(10, 42)) == selfId);\n'), del('        require(tin.slice(43, 46) == byte[](0x040100));\n')] },
  { id: 'K11', file: F, run: { [T_CUS]: ['NK8'] }, note: "the custody carries the curve's extension commitment", edits: [del('        require(byte[32](tin.slice(80, 112)) == extensionCommitment);\n')] },
  { id: 'K12', file: F, run: { [T_CUS]: ['NK9'] }, note: 'the custody holds exactly amountLeft', edits: [del('        require(int(byte[8](tin.slice(1, 9))) == amountLeft);\n')] },
  { id: 'K13', file: F, run: { [T_NEG]: ['NC17'] }, note: 'no fill before activeFrom', edits: [del('        require(tx.daa >= activeFrom);\n')] },
  { id: 'K14', file: F, run: { [T_NEG]: ['NC19'] }, note: 'n > 0 (a fill of 0 under minFill 0 re-spends the order)', edits: [del('        require(n > 0);\n')] },
  { id: 'K15', file: F, run: { [T_NEG]: ['NC20'] }, note: 'scale > 0 (hostile field: a negative scale sells for nothing)', edits: [del('        require(scale > 0);\n')] },
  { id: 'K16', file: F, run: { [T_NEG]: ['NC24'] }, note: 'tip >= 0 (hostile field)', edits: [del('        require(tip >= 0);\n')] },
  { id: 'K17', file: F, run: { [T_NEG]: ['NC21'] }, note: 'amountLeft <= total (else the walk starts before level 0 at its price)', edits: [del('        require(amountLeft <= total);\n')] },
  { id: 'K18', file: F, run: { [T_NEG]: ['NC18'] }, note: 'minFill unless the fill takes all', edits: [del('        require(n >= minFill || n == amountLeft);\n')] },
  { id: 'K19', file: F, run: { [T_NEG]: ['NC22'] }, note: 'every level price covers the tip', edits: [del('                require(p >= tip);\n')] },
  { id: 'K20', file: F, run: { [T_NEG]: ['NC8'] }, note: 'a fill ends within 8 levels (the 9th level free)', edits: [del('        require(pos == end);\n')] },
  { id: 'K21', file: F, run: { [T_NEG]: ['NC5'] }, note: 'the walk stops at every level boundary', edits: [rep('int upto = (lvl + 1) * levelSize;', 'int upto = (lvl + 2) * levelSize;')] },
  { id: 'K22', file: F, run: { [T_NEG]: ['NC4'] }, note: 'each level is rounded up for the maker', edits: [rep('allIn = allIn + quoteOf(upto - pos, p - tip, scale - 1);', 'allIn = allIn + quoteOf(upto - pos, p - tip, 0);')] },
  { id: 'K23', file: F, run: { [T_NEG]: ['NC16'] }, note: "positional payout: output i pays P2PK(maker)", edits: [del('        require(tx.outputs[self].scriptPubKey == byte[](new ScriptPubKeyP2PK(maker)));\n')] },
  { id: 'K24', file: F, run: { [T_NEG]: ['NC14'] }, note: "a partial fill: exactly one output of the curve's id", edits: [del('            require(OpCovOutputCount(selfId) == 1);\n')] },
  { id: 'K25', file: F, run: { [T_NEG]: ['NC6'], [T_GEN]: ['NB2'] }, note: 'the continuation is this script with amountLeft - n (and the token written)', edits: [del('            require(tx.outputs[selfOut].scriptPubKey == contSpk(rsLen, cid, rest));\n')] },
  { id: 'K25b', file: F, run: { [T_GEN]: ['NB2'] }, note: 'the first fill of an unbound curve writes its token into the continuation', edits: [rep('rs.slice(0, 35) + byte[](cid) + rs.slice(67, 173)', 'rs.slice(0, 35) + rs.slice(35, 67) + rs.slice(67, 173)')] },
  { id: 'K26', file: F, run: { [T_NEG]: ['NC13'] }, note: "the continuation keeps the order's carrier", edits: [del('            require(tx.outputs[selfOut].value >= tx.inputs[self].value);\n')] },
  { id: 'K27', file: F, run: { [T_NEG]: ['NC12'] }, note: "the rest custody keeps the custody's carrier", edits: [del('            require(tx.outputs[tokOut].value >= tx.inputs[tokenIn].value);\n')] },
  { id: 'K28', file: F, run: { [T_NEG]: ['NC1a', 'NC1b', 'NC1c', 'NC3', 'NC4', 'NC5', 'NC7'] }, note: 'a partial fill pays the maker the walk', edits: [del('            require(tx.outputs[self].value >= allIn);\n')] },
  { id: 'K29', file: F, run: { [T_NEG]: ['NC10'] }, note: 'the rest custody is pinned: the custody template with amountLeft - n owned by the curve', edits: [del('            require(tx.outputs[tokOut].scriptPubKey == byte[](new ScriptPubKeyP2SHFromRedeemScript(\n                trs.slice(0, tplPrefixLen) + byte[](0x08) + byte[](rest as byte[8]) + byte[](0x20) + byte[](selfId)\n                    + byte[](0x01) + byte[](SCHEME_COVID) + byte[](0x010020) + tin.slice(47, 79)\n                    + byte[](0x20) + tin.slice(80, 112) + trs.slice(tplPrefixLen + 112, trs.length)\n            )));\n')] },
  { id: 'K30', file: F, run: { [T_NEG]: ['NC11'] }, inputOnly: ['NC11'], note: "the rest custody is bound to the token's covenant", edits: [del('            require(OpOutputCovenantId(tokOut) == cid);\n')] },
  { id: 'K31', file: F, run: { [T_NEG]: ['NC15'] }, note: "a sell-out leaves no output of the curve's id", edits: [del('            require(OpCovOutputCount(selfId) == 0);\n')] },
  { id: 'K32', file: F, run: { [T_NEG]: ['NC2'] }, note: 'a sell-out pays the walk and both carriers', edits: [del('            require(tx.outputs[self].value >= allIn + tx.inputs[self].value + tx.inputs[tokenIn].value);\n')] },
  { id: 'K33', file: F, run: { [T_CAN]: ['NX2'] }, note: 'cancel: SIGHASH_ALL only', edits: [del('        require(sb[64] == SIGHASH_ALL);\n')] },
  { id: 'K34', file: F, run: { [T_CAN]: ['NX1'] }, note: "cancel: the maker's signature", edits: [del('        require(checkSig(s, maker));\n')] },
];
