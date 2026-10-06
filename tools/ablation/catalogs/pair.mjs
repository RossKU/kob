// KobPair (contracts/v2/KobPair.sil): the plain pair order, one template for both sides and both token families.
// Test binary: crates/kob-tests/tests/kob_pair_tests.rs (scenario ids NP.., run on the two mixed program pairs
// KCC20Ref_8x8 + KronToken2433 and KronToken2433 + KCC20Ref_8x8). Ids with `sk` / `sr` / `tk` / `tr` exist only where the
// custody S / the bought token T is KCC-20 / KRON. The C1 fake-quote mutations (Q4 / Q5 / Q6) are witnessed by the
// KobPair fake-quote scenarios directly AND by the same fake quotes read as the trigger evidence of a KobCondPair stop
// (kob_cond_pair_tests::cond_pair_evidence_fake_quotes, NC40..NC43, fill and arm update): the conditional reads only the
// pair order's state, so with the KobPair check removed the fake quote fills and arms the stop (every input accepts).
//
// Every `require` of KobPair.sil is an entry below (expect or hold) or one of the checks listed here. The unrolled
// noStrays scan is one function for S and T: its eight slot lines (GS0..GS7) are witnessed by a T stray at each slot
// (NP34t<k>, on the side whose T is the KCC-20 token: 8 inputs per token), its bound (GB) by a 9th input.
//
// Checks without a witness attack (not in the catalog):
//  - cancel `require(sb.length == 65)`: `sb[64]` of a shorter signature fails on its own, a longer one is no Schnorr
//    signature for checkSig.
//  - the KRON marker byte 0x08 of `tin.slice(33, 36) == 0x010208` and the KCC-20 push bytes of the custody state: every
//    genuine token UTXO of the program has them (only the id_type / scheme / borrow bytes are attacker-relevant: NP21).
//  - the refund `require(refundTip >= 0)`: its only use is `tx.outputs[self].value + refundTip >= carriers`, so a
//    negative refundTip raises the KAS the maker must receive (the keeper may take at most refundTip). A negative value
//    only tightens the keeper's payout, never loosens it, so no transaction is accepted when the check is removed.
import { del, rep, rx } from '../lib/edits.mjs';

export const suite = {
  name: 'pair',
  family: 'pair orders: KobPair',
  testBin: 'kob_pair_tests',
  srcDir: 'contracts/v2',
  templateMarker: null,
  expectedTemplates: 1,
};

const F = 'KobPair';
const SET = 'pair_settlement';
const FQ = 'pair_fake_quotes';
const CUS = 'pair_custody';
const STR = 'pair_strays';
const ALI = 'pair_aliasing';
const PAR = 'pair_params';
const LIF = 'pair_lifecycle';
const OUT = 'pair_outputs';
const CAR = 'pair_carriers';
// the KobCondPair stops armed by a fake pair quote (mode-1 evidence; KobCondPair is recompiled against the mutated KobPair)
const CFQ = 'kob_cond_pair_tests::cond_pair_evidence_fake_quotes';

const both = id => [`${id}a`, `${id}b`];
const T_PIN = `            require(tx.outputs[self].scriptPubKey == byte[](new ScriptPubKeyP2SHFromRedeemScript(
                tpre + tokenState(tFamily, tOut, byte[32](maker), false, byte[](0 as byte[8]) + byte[](0 as byte[8]) + byte[](0 as byte[8]) + byte[](0 as byte[8]), byte[](tExt)) + tsuf
            )));
`;
const S_PIN = `            require(tx.outputs[outIdx].scriptPubKey == byte[](new ScriptPubKeyP2SHFromRedeemScript(
                trs.slice(0, sPre) + tokenState(sFamily, outAmount, outOwner, rest, guard, ext) + trs.slice(sPre + sLen, trs.length)
            )));
`;
const T_STRAYS = '            noStrays(tCovId, selfId, -1, tOff);\n';

export const mutations = [
  // ---- identity of the order and of its custody
  { id: 'S1', file: F, test: ALI, expect: both('NP45'), note: 'the order is the only input of its covenant id (a sibling UTXO of the id)', edits: [del('        require(OpCovInputCount(selfId) == 1);\n')] },
  { id: 'C0', file: F, test: CUS, expect: both('NP24'), note: 'the custody input carries the S covenant (not a UTXO of another token owned by the order)', edits: [del('        require(OpInputCovenantId(custIn) == sCovId);\n')] },
  { id: 'C1', file: F, test: CUS, expect: both('NP25'), note: 'the custody input is a P2SH spend of its redeem script (a planted UTXO of the S covenant id)', edits: [del('        require(tx.inputs[idx].scriptPubKey == byte[](new ScriptPubKeyP2SHFromRedeemScript(rs)));\n')] },
  { id: 'C2', file: F, test: CUS, expect: both('NP26'), note: "the custody's template is S's (a planted P2SH UTXO of a look-alike template)", edits: [del('        require(blake3(byte[](pre as byte[8]) + rs.slice(0, pre) + byte[](suf as byte[8]) + rs.slice(pre + stLen, size)) == tpl);\n')] },
  { id: 'C3r', file: F, test: CUS, expect: ['NP20sr'], note: 'KRON custody owned by this order (two orders naming one custody)', edits: [del('            require(byte[32](tin.slice(1, 33)) == selfId);\n')] },
  { id: 'C3k', file: F, test: CUS, expect: ['NP20sk'], note: 'KCC-20 custody owned by this order (two orders naming one custody)', edits: [del('            require(byte[32](tin.slice(10, 42)) == selfId);\n')] },
  { id: 'C4r', file: F, test: CUS, expect: ['NP21sr'], inputOnly: ['NP21sr'], note: 'KRON custody of owner type 2 (covenant id; id_type 3 is also refused by the token program)', edits: [del('            require(tin.slice(33, 36) == byte[](0x010208));\n')] },
  { id: 'C5r', file: F, test: CUS, expect: ['NP22sr'], note: 'KRON custody not a minter', edits: [del('            require(tin.slice(44, 46) == byte[](0x0100));\n')] },
  { id: 'C4k', file: F, test: CUS, expect: ['NP21sk'], note: 'KCC-20 custody of owner scheme 0x04 with borrowing disabled', edits: [del('            require(tin.slice(43, 46) == byte[](0x040100));\n')] },
  { id: 'C6k', file: F, test: CUS, expect: ['NP22sk'], note: 'sFamily is 1 or 2 (hostile field read as KCC-20)', edits: [del('            require(sFamily == FAM_KCC20);\n')] },
  { id: 'C7', file: F, test: CUS, expect: both('NP27'), inputOnly: both('NP27'), note: 'a positive custody: a zero custody leaves output i of a refund unpinned (a KRON program also refuses the empty token output)', edits: [del('        require(custody > 0);\n')] },
  { id: 'C9s', file: F, test: PAR, expect: ['NP64a'], note: 'sScale > 0 (hostile field: an ask at a negative scale of A sells for one unit of B)', edits: [del('        require(sScale > 0);\n')] },
  { id: 'C9t', file: F, test: PAR, expect: ['NP64b'], note: 'tScale > 0 (hostile field: a bid at a negative scale of A pays a negative quote)', edits: [del('        require(tScale > 0);\n')] },
  { id: 'C8', file: F, test: SET, expect: ['NP06', 'NP07'], note: 'exact custody: held == custody (an extra unit to the matcher)', edits: [del('        require(held == custody);\n')] },
  // ---- strays of both tokens
  { id: 'G1', file: F, run: { [STR]: ['NP30a0', 'NP30a1', 'NP30a2', 'NP30b0', 'NP30b1', 'NP30b2', ...both('NP32')] }, note: 'no S input owned by this id but the custody (rest, IOC, close, refund)', edits: [del('        noStrays(sCovId, selfId, custIn, sOff);\n')] },
  { id: 'G2', file: F, run: { [STR]: ['NP31a0', 'NP31a1', 'NP31a2', 'NP31b0', 'NP31b1', 'NP31b2'] }, note: 'no T input owned by this id in a fill (rest, IOC, close)', edits: [del(T_STRAYS)] },
  ...[0, 1, 2, 3, 4, 5, 6, 7].map(k => ({ id: `GS${k}`, file: F, test: STR, expect: [`NP34t${k}`], note: `noStrays scan slot ${k}: a stray of the order at the input of slot ${k} of its token`, edits: [rep(`require(byte[32](OpTxInputScriptSigSubstr(x${k}, e${k}, e${k} + 32)) != me);`, 'require(true);')] })),
  { id: 'GB', file: F, test: STR, expect: ['NP35t'], inputOnly: ['NP35t'], note: 'noStrays bound: at most MAX_TOK_IN = 8 inputs of the token (a 9th is not scanned; the KCC-20 program refuses it on its own input)', edits: [del('        require(cnt <= MAX_TOK_IN);\n')] },
  { id: 'G3', file: F, test: STR, expect: both('NP33'), note: 'a refund carries no T input (T strays stay put)', edits: [del('            require(OpCovInputCount(tCovId) == 0);\n')] },
  // ---- refund
  { id: 'E1', file: F, test: LIF, expect: [...both('NP70'), ...both('NP71'), ...both('NP72'), ...both('NP73')], note: 'refund only once due (expiry, 90 days idle, IOC / FOK kill)', edits: [del('            require(tx.daa >= idleEnd);\n')] },
  { id: 'E1a', file: F, test: LIF, expect: both('NP70'), note: 'refund from expiryDaa, not one DAA earlier', edits: [rep('            if (expiryDaa < idleEnd) { idleEnd = expiryDaa; }', '            if (expiryDaa - 1 < idleEnd) { idleEnd = expiryDaa - 1; }')] },
  { id: 'E1b', file: F, test: LIF, expect: both('NP71'), note: 'the idle bound is 90 days of DAA', edits: [rep('    int constant MAX_IDLE = 77760000;', '    int constant MAX_IDLE = 77759999;')] },
  { id: 'E1c', file: F, test: LIF, expect: both('NP72'), note: 'the IOC / FOK kill is 600 DAA', edits: [rep('    int constant IOC_LIFE = 600;', '    int constant IOC_LIFE = 599;')] },
  { id: 'E1d', file: F, test: LIF, expect: both('NP73'), note: 'the kill counts from max(UTXO DAA, activeFrom)', edits: [del('                if (activeFrom > kill) { kill = activeFrom; }\n')] },
  { id: 'E3', file: F, test: LIF, expect: both('NP77'), note: 'a refund leaves no output bound to the order id (a forged owner of its strays)', edits: [rx('            require(refundTip >= 0);', /\n\s*require\(OpCovOutputCount\(selfId\) == 0\);/, '')] },
  { id: 'E4', file: F, test: LIF, expect: both('NP74'), note: 'a refund pays the maker everything but refundTip', edits: [del('            require(tx.outputs[self].value + refundTip >= carriers);\n')] },
  // ---- fill: quantity, time, decay
  { id: 'N1', file: F, test: SET, expect: ['NP08'], hold: { [SET]: ['NP09'] }, note: 'n <= amountLeft (a bid overfilled from escrow slack; an ask is held by its custody)', edits: [del('            require(n <= amountLeft);\n')] },
  { id: 'N2', file: F, hold: { [PAR]: ['NP55'] }, note: 'n > 0: a negative n is refused by sOut == n / tOut == n and the token programs (no negative amounts)', edits: [del('            require(n > 0);\n')] },
  { id: 'N3', file: F, test: PAR, expect: ['NP50'], note: 'minimum fill', edits: [del('            require(n >= minFill || n == amountLeft);\n')] },
  { id: 'N4', file: F, test: PAR, expect: ['NP54'], note: 'no fill before activeFrom', edits: [del('            require(tx.daa >= activeFrom);\n')] },
  { id: 'N5', file: F, test: PAR, expect: ['NP61'], note: 'tip >= 0 (hostile field)', edits: [del('            require(tip >= 0);\n')] },
  { id: 'N6', file: F, test: PAR, expect: ['NP62'], note: 'deliveryCarrier >= 0 (hostile field)', edits: [del('            require(deliveryCarrier >= 0);\n')] },
  { id: 'N7', file: F, test: PAR, expect: ['NP52'], note: 'TWAP / DCA interval (CSV)', edits: [del('                require(this.ageDaa >= interval);\n')] },
  { id: 'N8', file: F, test: PAR, expect: ['NP51'], note: 'maxFill', edits: [del('            require(maxFill == 0 || n <= maxFill);\n')] },
  { id: 'N9', file: F, test: SET, expect: ['NP02'], note: 'an ask releases exactly n', edits: [del('                require(sOut == n);\n')] },
  { id: 'D1', file: F, test: PAR, expect: ['NP58'], note: 'decay slope > 0 (hostile field)', edits: [del('                require(slope > 0);\n')] },
  { id: 'D2', file: F, test: PAR, expect: ['NP59'], note: 'decayStep > 0 (hostile field; 0 also fails the division)', edits: [del('                require(decayStep > 0);\n')] },
  { id: 'D3', file: F, test: PAR, expect: ['NP56'], note: 'decay time t proven by CLTV (an overstated t: a lower price)', edits: [del('                require(tx.daa >= t);\n')] },
  { id: 'D4', file: F, test: PAR, expect: ['NP57'], note: 'decay time t not before the origin', edits: [del('                require(t >= origin);\n')] },
  { id: 'D5', file: F, test: PAR, expect: ['NP65'], note: 'the decay origin of a TWAP slice is its opening (UTXO DAA + interval)', edits: [del('                    if (slice > origin) { origin = slice; }\n')] },
  { id: 'D6', file: F, test: PAR, expect: ['NP66'], note: 'a Dutch ask stops at priceEnd', edits: [del('                    if (p < priceEnd) { p = priceEnd; }\n')] },
  { id: 'D7', file: F, test: PAR, expect: ['NP67'], note: 'a rising bid stops at priceEnd', edits: [del('                    if (p > priceEnd) { p = priceEnd; }\n')] },
  { id: 'D8', file: F, test: PAR, expect: ['NP60'], note: 'the quote is positive (a bid at 0 takes A for nothing)', edits: [del('            require(p > 0);\n')] },
  { id: 'D9', file: F, test: PAR, expect: ['NP63'], note: 'side is 1 or 2 (hostile field)', edits: [del('                require(side == SIDE_BID);\n')] },
  // ---- settlement and the C1 fake quotes
  { id: 'Q1', file: F, test: SET, expect: ['NP01'], note: 'an ask receives at least the ceil', edits: [del('                require(tOut >= quoteOf(n, p, sScale, sScale - 1));\n')] },
  { id: 'Q1r', file: F, test: SET, expect: ['NP01'], note: "an ask's quote is rounded up (the floor pays it one unit short)", edits: [rep('                require(tOut >= quoteOf(n, p, sScale, sScale - 1));', '                require(tOut >= quoteOf(n, p, sScale, 0));')] },
  { id: 'Q2', file: F, test: SET, expect: ['NP05'], note: 'a bid receives exactly n', edits: [del('                require(tOut == n);\n')] },
  { id: 'Q3', file: F, test: SET, expect: ['NP03', 'NP04'], note: 'a bid pays exactly its floor', edits: [del('                require(sOut == quoteOf(n, p, tScale, 0));\n')] },
  { id: 'Q3a', file: F, test: SET, expect: ['NP04'], note: 'a bid pays no less than its floor (it pays exactly its quote: takeable evidence)', edits: [rep('                require(sOut == quoteOf(n, p, tScale, 0));', '                require(sOut <= quoteOf(n, p, tScale, 0));')] },
  { id: 'Q3b', file: F, test: SET, expect: ['NP03'], note: 'a bid pays no more than its floor', edits: [rep('                require(sOut == quoteOf(n, p, tScale, 0));', '                require(sOut >= quoteOf(n, p, tScale, 0));')] },
  { id: 'Q4', file: F, run: { [FQ]: ['NP12a', 'NP12b'], [CFQ]: ['NC40', 'NC43', 'NC40u', 'NC43u'] }, note: "C1: an ask's custody is its whole amountLeft (every displayed amount takeable; never evidence otherwise)", edits: [del('                require(custody == amountLeft);\n')] },
  { id: 'Q5', file: F, run: { [FQ]: ['NP10'], [CFQ]: ['NC41', 'NC41u'] }, note: 'C1: the escrow pays the quote (an unfunded bid can be neither filled nor evidence)', edits: [del('            require(outAmount >= 0);\n')] },
  { id: 'Q6', file: F, run: { [FQ]: ['NP11a', 'NP11b'], [CFQ]: ['NC42a', 'NC42b', 'NC42au', 'NC42bu'] }, note: 'C1: the order UTXO funds its delivery carrier and tip (no filler-funded carrier wall)', edits: [del('                require(tx.inputs[self].value >= deliveryCarrier + tipKas);\n')] },
  { id: 'Q7', file: F, test: FQ, expect: ['NP13'], note: 'a rest keeps something in the custody (no continuation with an empty escrow)', edits: [del('                require(outAmount > 0);\n')] },
  { id: 'Q8', file: F, test: CAR, expect: both('NP118'), note: 'the KAS tip is the floor', edits: [rep('            int tipKas = quoteOf(n, tip, baseScale, 0);', '            int tipKas = quoteOf(n, tip, baseScale, baseScale - 1);')] },
  // ---- the maker's T at output i
  { id: 'T1', file: F, run: { [OUT]: ['NP90tk', 'NP90tr', 'NP91tk', 'NP91tr', 'NP92tk', 'NP92tr', 'NP93tk'], [ALI]: both('NP40') }, inputOnly: ['NP92tk', 'NP92tr'], note: "the maker's T output pinned (owner, owner type, extension, borrowing, minter; one delivery for two orders; the token programs also refuse NP92)", edits: [del(T_PIN)] },
  { id: 'T2k', file: F, test: OUT, expect: ['NP91tk'], note: 'KCC-20 delivery owned by a key (scheme 0x00)', edits: [rep('            byte sc = SCHEME_P2PK;', '            byte sc = SCHEME_COVID;')] },
  { id: 'T2r', file: F, test: OUT, expect: ['NP91tr'], note: 'KRON delivery of id_type 3 (address)', edits: [rep('            byte ty = KRON_DELIVERY;', '            byte ty = KRON_COVID;')] },
  { id: 'T3', file: F, test: OUT, expect: both('NP94'), note: 'the delivery carries the T covenant', edits: [del('            require(OpOutputCovenantId(self) == tCovId);\n')] },
  { id: 'T4', file: F, test: OUT, expect: both('NP95'), inputOnly: both('NP95'), hold: { [OUT]: both('NP96') }, note: "the T template source's prefix / suffix hash (a planted look-alike; the T program also refuses the bound look-alike output)", edits: [del('            require(blake3(byte[](tPre as byte[8]) + tpre + byte[](tSuf as byte[8]) + tsuf) == tTplHash);\n')] },
  { id: 'T5', file: F, hold: { [OUT]: both('NP96') }, note: 'the T template source carries the T covenant: defence in depth (any T-covenant input is a genuine T program input whose bytes the hash pins; another program fails the hash)', edits: [del('            require(OpInputCovenantId(tTplIn) == tCovId);\n')] },
  { id: 'T6k', file: F, test: CUS, expect: ['NP23tk'], note: 'tFamily is 1 or 2 (hostile field read as KCC-20)', edits: [del('                require(tFamily == FAM_KCC20);\n')] },
  // ---- the S outputs (rest, IOC return, refund)
  { id: 'O1', file: F, run: { [OUT]: [...both('NP97'), ...both('NP98'), ...both('NP99')], [LIF]: both('NP76'), [ALI]: both('NP41') }, hold: { [LIF]: both('NP78') }, note: 'the one S output (rest owned by the order / return / refund to the maker, exact amount) at the custody index / output i (a refund custody at another index is held by the covenant-id pin too)', edits: [del(S_PIN)] },
  { id: 'O2', file: F, test: OUT, expect: both('NP100'), hold: { [LIF]: both('NP78') }, note: 'the S output carries the S covenant (a refund custody at another index is held by the script pin too)', edits: [del('            require(OpOutputCovenantId(outIdx) == sCovId);\n')] },
  { id: 'O12', file: F, test: LIF, expect: both('NP78'), note: 'both S-output pins removed: a refund custody moved to another index leaves output i a plain output of the order value', edits: [del(S_PIN), del('            require(OpOutputCovenantId(outIdx) == sCovId);\n')] },
  // ---- continuation and carriers
  { id: 'K1', file: F, test: ALI, expect: both('NP42'), hold: { [ALI]: both('NP43') }, note: 'one continuation (a second output bound to the id is a forged UTXO of it; none fails OpCovOutputIdx anyway)', edits: [del('                require(OpCovOutputCount(selfId) == 1);\n')] },
  { id: 'K2', file: F, test: CAR, expect: [...both('NP110'), ...both('NP111')], note: 'the continuation is this script with amountLeft - n and custody - sOut', edits: [rx('                byte[] me = OpTxInputScriptSigSubstr(self, len - this.bytecodeSize, len);', /\n\s*require\(tx\.outputs\[selfOut\]\.scriptPubKey == [\s\S]*?\)\)\);/, '')] },
  { id: 'K3', file: F, test: CAR, expect: both('NP112'), note: 'the continuation keeps the order value minus the delivery carrier and the tip', edits: [del('                require(tx.outputs[selfOut].value >= tx.inputs[self].value - deliveryCarrier - tipKas);\n')] },
  { id: 'K4', file: F, test: CAR, expect: both('NP113'), note: "the custody rest keeps its carrier", edits: [rx('                require(tx.outputs[selfOut].value >= tx.inputs[self].value - deliveryCarrier - tipKas);', /\n\s*require\(tx\.outputs\[custIn\]\.value >= tx\.inputs\[custIn\]\.value\);/, '')] },
  { id: 'K5', file: F, test: CAR, expect: both('NP114'), note: 'the delivery carries deliveryCarrier', edits: [del('                require(tx.outputs[self].value >= deliveryCarrier);\n')] },
  { id: 'K6', file: F, test: PAR, expect: ['NP53'], note: 'FOK: all or nothing', edits: [del('                require(tif != TIF_FOK || amountLeft == n);\n')] },
  { id: 'K7', file: F, test: ALI, expect: both('NP44'), note: 'a terminating fill leaves no output bound to the order id', edits: [rx('                require(tif != TIF_FOK || amountLeft == n);', /\n\s*require\(OpCovOutputCount\(selfId\) == 0\);/, '')] },
  { id: 'K8', file: F, test: CAR, expect: both('NP115'), note: 'the IOC return keeps its carrier', edits: [rx('                if (outAmount > 0) {', /\n\s*require\(tx\.outputs\[custIn\]\.value >= tx\.inputs\[custIn\]\.value\);/, '')] },
  { id: 'K9', file: F, test: CAR, expect: both('NP116'), note: "IOC / done: the maker gets the order value back but the tip", edits: [del('                    require(tx.outputs[self].value >= tx.inputs[self].value - tipKas);\n')] },
  { id: 'K10', file: F, test: CAR, expect: both('NP117'), note: 'sold out: every carrier back to the maker but the tip', edits: [del('                    require(tx.outputs[self].value >= carriers - tipKas);\n')] },
  // ---- cancel
  { id: 'X1', file: F, test: LIF, expect: both('NP79'), note: 'cancel: SIGHASH_ALL only', edits: [del('        require(sb[64] == SIGHASH_ALL);\n')] },
  { id: 'X2', file: F, test: LIF, expect: both('NP80'), note: 'cancel: the maker signs', edits: [rep('        require(checkSig(s, maker));', '        require(sb.length == 65);')] },
];
