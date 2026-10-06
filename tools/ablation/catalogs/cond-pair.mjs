// KobCondPair (contracts/v2/KobCondPair.sil): the pair conditional (stop / stop-limit / trailing / TP leg / OCO, and the
// exit of KobIfdPair). This catalog owns EVERY check of KobCondPair EXCEPT the repeat-IFD merge / re-arm checks, which
// the cond-pair-rpt catalog owns (the TP fill with / without the entry, rptUntil, the budget / prefund / proceeds of a
// re-arm, the sell-out, and the update / stop-leg "never next to the entry" rules: `OpCovInputCount(parent) == 0`,
// `require(present == 0)`, `tx.daa >= rptUntil`, the merge argument, `n < MERGE_K`, `dOut > 0`, the re-arming sell-out).
//
// Test binary: crates/kob-tests/tests/kob_cond_pair_tests.rs (ids NC..; a / b = the ASK / BID side, sk / sr / tk = the
// custody S / the bought token T is KCC-20 / KRON). Attacks run on the two mixed program pairs (KCC20Ref_8x8 +
// KronToken2433, KronToken2433 + KCC20Ref_8x8), NC59 on KCC-20 / KCC-20 (see cond_pair_evidence_tk). Scenarios that edit
// the evidence order's own state also fail that order's input: `inputOnly` (the proof is the conditional's own input).
//
// Every `require` of KobCondPair.sil is an entry below (expect or hold), an entry of cond-pair-rpt, or one of the checks
// listed here. The unrolled noStrays scan (one function for S and T, fill / refund / update): its eight slot lines
// (GS0..GS7) are witnessed by a T stray at each slot (NC18t<k>, on the side whose T is the KCC-20 token), its bound (GB)
// by a 9th input. The repeat-IFD requires left to cond-pair-rpt (kob_ifd_pair_tests): update `OpCovInputCount(parent)
// == 0`, `tx.daa >= rptUntil`, a stop fill's `present == 0`, `n < MERGE_K`, the entry's 0x08 first push and its merge
// argument `-(i * 2^53 + n)`, `dOut > 0`, and the re-arming sell-out's `outAmount == 0` and delivery carrier.
//
// Checks without a witness attack (not in the catalog):
//  - the fixed push bytes of a custody state: every genuine token UTXO has them.
//  - cancel `require(sb.length == 65)`: `sb[64]` of a shorter signature fails by itself; a longer one is no signature.
//  - refund `require(refundTip >= 0)`: its only use is `outputs[self].value + refundTip >= carriers`; a negative value
//    only raises what the maker must receive (the keeper's take is bounded above by refundTip), never lowers it.
//  - the side rule of the evidence (the order's own counterparties as evidence, NC26 / NC35): held by the template
//    authentication and the identity check (EVHASH, EVP2SH, EVTPL hold; EVID flips NC35).
import { del, rep, rx } from '../lib/edits.mjs';

export const suite = {
  name: 'cond-pair',
  family: 'pair orders: KobCondPair (excl. repeat merges)',
  testBin: 'kob_cond_pair_tests',
  srcDir: 'contracts/v2',
  templateMarker: null,
  expectedTemplates: 1,
};

const F = 'KobCondPair';
const SET = 'cond_pair_settlement';
const EV0 = 'cond_pair_evidence_ev0';
const EV1 = 'cond_pair_evidence_ev1';
const EXP = 'cond_pair_evidence_exposure';
const TR = 'cond_pair_trailing';
const UP = 'cond_pair_updates';
const LIF = 'cond_pair_lifecycle';
const OV = 'cond_pair_overflow';
const CUS = 'cond_pair_custody';
const FIL = 'cond_pair_fills';
const CAR = 'cond_pair_carriers';
const MISC = 'cond_pair_evidence_misc';

const both = id => [`${id}a`, `${id}b`];

// anchors for function-scoped rx edits
const A_EVLEG = '    function evLeg(int ev, bool ask, byte[32] tok, int fam, int sc, int tk, int tokSuf, int minN) : int {';
const A_RD = '    function rd(int ev, bool ask, bool pair, byte[32] tok, int fam, int sc, int kSuf, byte[32] oTok, int oFam, int osc, int oSuf, int tk, int minN) : int {';
const A_TRAIL = '            bool cap = true;\n            if (trail) {';
const A_UPDATE = '        if (update) {\n            // UPDATE: never next to the repeat entry';

export const mutations = [
  // ---- the order's own identity and custody
  { id: 'S1', file: F, test: SET, expect: both('NC17'), note: 'the order is the only input of its covenant id (a sibling UTXO of the id)', edits: [del('        require(OpCovInputCount(selfId) == 1);\n')] },
  { id: 'C1', file: F, test: SET, expect: ['NC04a', 'NC04b'], note: 'exact custody: held == custody', edits: [del('            require(held == custody);\n')] },
  { id: 'C2', file: F, test: LIF, expect: both('NC82'), inputOnly: both('NC82'), note: 'a refund needs a positive custody (a zero custody leaves output i unpinned)', edits: [del('                require(custody > 0);\n')] },
  // ---- strays of both tokens (fill and refund share the top-of-entry guards)
  { id: 'G1', file: F, run: { [SET]: ['NC06a', 'NC06b', 'NC08a', 'NC08b'], [UP]: ['NC76'] }, note: 'no S input owned by this id but the custody (fill, refund; the update spends none)', edits: [del('        noStrays(sCovId, selfId, cust, sOff);\n')] },
  { id: 'G2', file: F, run: { [SET]: ['NC07a', 'NC07b', 'NC09a', 'NC09b'], [UP]: ['NC77'] }, note: 'no T input owned by this id', edits: [del('        noStrays(tCovId, selfId, 0 - 1, tOff);\n')] },
  { id: 'G3', file: F, test: UP, expect: ['NC76'], note: 'an update spends no S input of the order (cust = -1 so the custody is not exempted)', edits: [rep('        if (update) { cust = 0 - 1; }', '        if (update) { cust = custIn; }')] },
  // the unrolled scan slot by slot (one shared function: the T scan of a TP fill witnesses each line), and its bound (a 9th
  // input of a KCC-20 token is beyond the scan; the token program refuses it on its own input, so only the order flips)
  ...[0, 1, 2, 3, 4, 5, 6, 7].map(k => ({ id: `GS${k}`, file: F, test: SET, expect: [`NC18t${k}`], note: `noStrays scan slot ${k}: a stray of the order at the input of slot ${k} of its token`, edits: [rep(`require(byte[32](OpTxInputScriptSigSubstr(x${k}, e${k}, e${k} + 32)) != me);`, 'require(true);')] })),
  { id: 'GB', file: F, test: SET, expect: ['NC19t'], inputOnly: ['NC19t'], note: 'noStrays bound: at most MAX_TOK_IN = 8 inputs of the token (a 9th is not scanned; the KCC-20 program refuses it on its own input)', edits: [del('        require(cnt <= MAX_TOK_IN);\n')] },
  // ---- settlement (leg fill)
  { id: 'N1', file: F, test: SET, expect: ['NC05'], note: 'n <= amountLeft', edits: [del('                require(n <= amountLeft);\n')] },
  { id: 'N2', file: F, test: SET, expect: ['NC01'], note: 'an ask receives at least the ceil', edits: [del('                    require(tOut >= quoteOf(n, legPrice, sA, sA - 1));\n')] },
  { id: 'N2r', file: F, test: SET, expect: ['NC01'], note: "an ask's leg price is rounded up", edits: [rep('                    require(tOut >= quoteOf(n, legPrice, sA, sA - 1));', '                    require(tOut >= quoteOf(n, legPrice, sA, 0));')] },
  { id: 'N3', file: F, test: SET, expect: ['NC02'], note: 'a bid pays at most the floor', edits: [del('                    require(sOut <= quoteOf(n, legPrice, sA, 0));\n')] },
  { id: 'N4', file: F, test: SET, expect: ['NC03'], note: 'a bid receives exactly n', edits: [del('                    require(tOut == n);\n')] },
  // ---- the maker's token at output i and the S outputs
  { id: 'O1', file: F, run: { [SET]: both('NC11'), [LIF]: both('NC84') }, note: "the maker's token / the S output pinned (owner, amount, index)", edits: [del('                require(tx.outputs[outIdx].scriptPubKey == byte[](new ScriptPubKeyP2SHFromRedeemScript(\n                    trs.slice(0, sPre) + tokenState(sFamily, outAmount, outOwner, rest, gx) + trs.slice(sPre + sLen, trs.length)\n                )));\n')] },
  { id: 'O2', file: F, test: SET, expect: both('NC16'), note: 'the S output carries the S covenant', edits: [del('                require(OpOutputCovenantId(outIdx) == sCovId);\n')] },
  { id: 'O3', file: F, test: SET, expect: both('NC13'), note: "the maker's token output at i is pinned (delivery / profit)", edits: [del('                require(tx.outputs[self].scriptPubKey == byte[](new ScriptPubKeyP2SHFromRedeemScript(\n                    oPre + tokenState(oFam, dOut, byte[32](maker), false, oGx) + oSuf\n                )));\n')] },
  { id: 'O4', file: F, test: SET, expect: both('NC14'), note: "the maker's token output carries its token covenant", edits: [del('                require(OpOutputCovenantId(self) == oTok);\n')] },
  { id: 'K1', file: F, test: SET, expect: both('NC10'), note: 'one continuation on a partial fill', edits: [rx('        if (cont) {', /require\(OpCovOutputCount\(selfId\) == 1\);/, 'require(true);')] },
  // ---- evidence. rd reads the A leg (mode 0) and the pair order (mode 1); evLeg reads the B leg (mode 0); tplState
  // authenticates both. Scenarios that doctor the evidence order's own state also fail that order's input (inputOnly):
  // the proof is the conditional's own input.
  // rd (A leg / pair order)
  { id: 'EVPUSH', file: F, run: { [EV0]: both('NC2C'), [EV1]: both('NC3C') }, note: 'rd: the evidence sigscript starts with the 8-byte fill push (a cancel ground to read as a fill)', edits: [rx(A_RD, /require\(OpTxInputScriptSigSubstr\(ev, 0, 1\) == byte\[\]\(0x08\)\);/, 'require(true);')] },
  { id: 'EVN', file: F, run: { [EV0]: both('NC20'), [EV1]: both('NC30') }, inputOnly: [...both('NC20'), ...both('NC30')], note: 'rd: the evidence is filled (n > 0)', edits: [rx(A_RD, /require\(n > 0\);/, 'require(true);')] },
  { id: 'EVMT', file: F, run: { [EV0]: both('NC24'), [EV1]: both('NC34') }, inputOnly: both('NC24'), note: 'rd: n >= minTouch', edits: [rx(A_RD, /require\(n >= minN\);/, 'require(true);')] },
  { id: 'EVID', file: F, run: { [EV0]: [...both('NC22'), ...both('NC23')], [EV1]: [...both('NC32'), ...both('NC33'), ...both('NC35')] }, inputOnly: [...both('NC22'), ...both('NC23'), ...both('NC32'), ...both('NC33'), ...both('NC35')], note: 'rd: token and scale (mode 1: side, S, T and both scales) are the order\'s', edits: [rx(A_RD, /require\(got == want\);/, 'require(true);')] },
  { id: 'EVSLOPE', file: F, run: { [EV0]: both('NC21'), [EV1]: both('NC31') }, inputOnly: [...both('NC21'), ...both('NC31')], note: 'rd: the evidence is not decaying', edits: [rx(A_RD, /require\(int\(byte\[8\]\(o\.slice\(208 \+ w, 216 \+ w\)\)\) == 0\);/, '')] },
  { id: 'EVREST', file: F, run: { [EXP]: ['NC50', 'NC52', 'NC53', 'NC51a'], [EV1]: both('NC3E') }, inputOnly: ['NC52', 'NC53'], note: 'rd: rested >= minRestDaa before the lock (every exposure term)', edits: [rx(A_RD, /require\(tx\.daa >= exposed \+ minRestDaa\);/, 'require(true);')] },
  { id: 'EVINT', file: F, test: EXP, expect: ['NC53'], inputOnly: ['NC53'], note: 'rd: the interval exposure term', edits: [rx(A_RD, /int exposed = OpTxInputDaaScore\(ev\) \+ int\(byte\[8\]\(o\.slice\(190 \+ w, 198 \+ w\)\)\);/, 'int exposed = OpTxInputDaaScore(ev);')] },
  { id: 'EVACT', file: F, test: EXP, expect: ['NC52'], inputOnly: ['NC52'], note: 'rd: the activeFrom exposure term', edits: [rx(A_RD, /int act = int\(byte\[8\]\(o\.slice\(163 \+ d, 171 \+ d\)\)\);/, 'int act = 0;')] },
  { id: 'EVTKDAA', file: F, test: EXP, expect: ['NC51a'], note: "rd: the ask evidence's custody DAA exposure term", edits: [rx(A_RD, /int td = OpTxInputDaaScore\(tk\);/, 'int td = 0;')] },
  { id: 'EVTK', file: F, test: 'cond_pair_evidence_tk', expect: ['NC59a'], note: "rd: the ask evidence's custody is a token input of its token", edits: [rx(A_RD, /require\(OpInputCovenantId\(tk\) == cTok\);/, 'require(true);')] },
  { id: 'EVTKOWN', file: F, test: EXP, expect: ['NC57a'], note: "rd: the ask evidence's custody is owned by the evidence ask (covenant marker)", edits: [rx(A_RD, /require\(OpTxInputScriptSigSubstr\(tk, end, end \+ 32 \+ mark\.length\) == byte\[\]\(OpInputCovenantId\(ev\)\) \+ mark\);/, 'require(true);')] },
  // evLeg (the B leg of mode 0)
  { id: 'EVBPUSH', file: F, test: EXP, expect: both('NC5D'), note: 'evLeg: the B evidence sigscript starts with the 8-byte fill push (a cancel ground to read as a fill)', edits: [rx(A_EVLEG, /require\(OpTxInputScriptSigSubstr\(ev, 0, 1\) == byte\[\]\(0x08\)\);/, 'require(true);')] },
  { id: 'EVBN', file: F, test: EV0, expect: both('NC2N'), inputOnly: both('NC2N'), note: 'evLeg: the B evidence is filled (n > 0)', edits: [rx(A_EVLEG, /require\(n > 0\);/, 'require(true);')] },
  { id: 'EVBMT', file: F, test: EV0, expect: both('NC25'), inputOnly: both('NC25'), note: 'evLeg: n_B >= ceil(minTouch * stop / scale(A))', edits: [rx(A_EVLEG, /require\(n >= minN\);/, 'require(true);')] },
  { id: 'EVBTOK', file: F, test: EV0, expect: both('NC2T'), inputOnly: both('NC2T'), note: 'evLeg: the B evidence is of token B', edits: [rx(A_EVLEG, /require\(byte\[32\]\(o\.slice\(34, 66\)\) == tok\);/, 'require(true);')] },
  { id: 'EVBSCALE', file: F, test: EV0, expect: both('NC28'), inputOnly: both('NC28'), note: "evLeg: the B evidence quotes at B's scale", edits: [rx(A_EVLEG, /require\(int\(byte\[8\]\(q\.slice\(0, 8\)\)\) == sc\);/, '')] },
  { id: 'EVBSLOPE', file: F, test: EV0, expect: both('NC29'), inputOnly: both('NC29'), note: 'evLeg: the B evidence is not decaying', edits: [rx(A_EVLEG, /require\(int\(byte\[8\]\(r\.slice\(18, 26\)\)\) == 0\);/, '')] },
  { id: 'EVBREST', file: F, run: { [EV0]: [...both('NC2E'), ...both('NC2I'), ...both('NC2A')], [EXP]: ['NC51b'] }, inputOnly: [...both('NC2I'), ...both('NC2A')], note: 'evLeg: rested >= minRestDaa (every exposure term)', edits: [rx(A_EVLEG, /require\(tx\.daa >= exposed \+ minRestDaa\);/, 'require(true);')] },
  { id: 'EVBINT', file: F, test: EV0, expect: both('NC2I'), inputOnly: both('NC2I'), note: 'evLeg: the interval exposure term', edits: [rx(A_EVLEG, /int exposed = OpTxInputDaaScore\(ev\) \+ int\(byte\[8\]\(r\.slice\(0, 8\)\)\);/, 'int exposed = OpTxInputDaaScore(ev);')] },
  { id: 'EVBACT', file: F, test: EV0, expect: both('NC2A'), inputOnly: both('NC2A'), note: 'evLeg: the activeFrom exposure term', edits: [rx(A_EVLEG, /int act = int\(byte\[8\]\(q\.slice\(45, 53\)\)\);/, 'int act = 0;')] },
  { id: 'EVBTKDAA', file: F, test: EXP, expect: ['NC51b'], note: "evLeg: the ask evidence's custody DAA exposure term", edits: [rx(A_EVLEG, /int td = OpTxInputDaaScore\(tk\);/, 'int td = 0;')] },
  { id: 'EVBTK', file: F, test: 'cond_pair_evidence_tk', expect: ['NC59b'], note: "evLeg: the ask evidence's custody is a token input of its token", edits: [rx(A_EVLEG, /require\(OpInputCovenantId\(tk\) == tok\);/, 'require(true);')] },
  { id: 'EVBTKOWN', file: F, test: EXP, expect: ['NC57b'], note: "evLeg: the ask evidence's custody is owned by the evidence ask", edits: [rx(A_EVLEG, /require\(OpTxInputScriptSigSubstr\(tk, end, end \+ 32 \+ mark\.length\) == byte\[\]\(OpInputCovenantId\(ev\)\) \+ mark\);/, 'require(true);')] },
  // tplState (both readers)
  { id: 'EVHASH', file: F, run: { [EXP]: ['NC58'] }, hold: { [EV0]: both('NC26'), [EXP]: ['NC54', 'NC55', 'NC56'] }, note: 'the evidence template hash (a look-alike P2SH redeem; the wrong side and a wrong input are also refused by the P2SH check or the fill push)', edits: [rx('    function tplState(int idx, int pre, int stLen, int suf, byte[32] tpl) : byte[] {', /require\(blake3\([\s\S]*?\) == tpl\);/, 'require(true);')] },
  { id: 'EVP2SH', file: F, run: { [EXP]: ['NC5P'] }, hold: { [EV0]: both('NC26'), [EXP]: ['NC54', 'NC55', 'NC56'] }, note: 'the evidence is a P2SH spend of the redeem it shows (a planted UTXO with genuine redeem bytes)', edits: [rx('    function tplState(int idx, int pre, int stLen, int suf, byte[32] tpl) : byte[] {', /require\(tx\.inputs\[idx\]\.scriptPubKey == byte\[\]\(new ScriptPubKeyP2SHFromRedeemScript\(rs\)\)\);/, 'require(true);')] },
  // the implied-rate comparison (arm and trailing validity)
  { id: 'EVARM', file: F, run: { [EV1]: both('NC36'), [TR]: both('NC60') }, inputOnly: both('NC36'), note: 'the implied rate reaches the stop / the trail is valid (a >= q hi, a <= q lo)', edits: [rx('            int q = quoteOf(x, b, sB, c);', /if \(hi\) \{\n\s*require\(a >= q\);\n\s*\} else \{\n\s*require\(a <= q\);\n\s*\}/, 'require(true);')] },
  // ---- trailing
  { id: 'TRMAX', file: F, test: TR, expect: both('NC61'), note: 'the trail is maximal (a < q2 hi, a > q2 lo)', edits: [rx('            if (!cap) {', /if \(hi\) \{\n\s*require\(a < q2\);\n\s*\} else \{\n\s*require\(a > q2\);\n\s*\}/, 'require(true);')] },
  // the same comparisons one branch at a time: hi (the A leg is a bid: arm a buy stop, trail a sell stop) and lo
  { id: 'EVARMH', file: F, run: { [EV1]: ['NC36b'], [TR]: ['NC60a'] }, inputOnly: ['NC36b'], note: 'hi branch: a buy stop arms only at a rate >= its stop; a sell stop trails only to a valid stop', edits: [rep('require(a >= q);', 'require(true);')] },
  { id: 'EVARML', file: F, run: { [EV1]: ['NC36a'], [TR]: ['NC60b'] }, inputOnly: ['NC36a'], note: 'lo branch: a sell stop arms only at a rate <= its stop; a buy stop trails only to a valid stop', edits: [rep('require(a <= q);', 'require(true);')] },
  { id: 'TRMAXH', file: F, test: TR, expect: ['NC61a'], note: 'hi branch: a sell stop trails maximally', edits: [rep('require(a < q2);', 'require(true);')] },
  { id: 'TRMAXL', file: F, test: TR, expect: ['NC61b'], note: 'lo branch: a buy stop trails maximally', edits: [rep('require(a > q2);', 'require(true);')] },
  { id: 'TRK', file: F, hold: { [TR]: both('NC60') }, note: 'k > 0 in the trail branch (k <= 0 is an arm; the validity / maximality checks also bind)', edits: [rx(A_TRAIL, /require\(k > 0\);/, 'require(true);')] },
  { id: 'TRWAIT', file: F, test: TR, expect: both('NC63'), note: 'at most one ratchet per trailWait', edits: [rx(A_TRAIL, /require\(this\.ageDaa >= trailWait\);/, 'require(true);')] },
  { id: 'TRCAPA', file: F, test: TR, expect: ['NC64a'], note: 'ASK trail stays below tpPrice', edits: [rx(A_TRAIL, /require\(newStop < tpPrice\);/, 'require(true);')] },
  { id: 'TRCAPB', file: F, test: TR, expect: ['NC64b'], note: 'BID trail stays above the floor (max(tpPrice, 0))', edits: [rx(A_TRAIL, /require\(newStop > floorP\);/, 'require(true);')] },
  // ---- update encodings and keeperTip
  { id: 'U1', file: F, test: UP, expect: ['NC70'], note: 'an update has n == 0', edits: [rx('        if (update) {\n            require(upd == 1);', /require\(n == 0\);/, 'require(true);')] },
  { id: 'U2', file: F, test: UP, expect: ['NC71'], note: 'upd is 0 or 1', edits: [rx('        bool update = upd != 0;', /require\(upd == 1\);/, 'require(true);')] },
  { id: 'U3', file: F, test: UP, expect: ['NC72'], note: 'only an unarmed stop is updated', edits: [rx(A_UPDATE, /require\(armed == 0\);/, 'require(true);')] },
  { id: 'U4', file: F, test: UP, expect: ['NC73'], note: 'the order has a stop leg', edits: [rx(A_UPDATE, /require\(stopPrice > 0\);/, 'require(true);')] },
  { id: 'U5', file: F, test: UP, expect: ['NC78'], note: 'an update waits for activeFrom', edits: [rx(A_UPDATE, /require\(tx\.daa >= activeFrom\);/, 'require(true);')] },
  { id: 'U6', file: F, test: UP, expect: ['NC74'], note: 'the keeper takes at most keeperTip (the continuation keeps value - keeperTip)', edits: [rep('            floor = tx.inputs[self].value - keeperTip;', '            floor = tx.inputs[self].value - keeperTip - 1;')] },
  { id: 'U7', file: F, test: UP, expect: ['NC75'], note: 'keeperTip >= 0 (hostile field)', edits: [rx(A_UPDATE, /require\(keeperTip >= 0\);/, 'require(true);')] },
  // ---- leg selection and stop band
  { id: 'L1', file: F, test: SET, expect: ['NC15'], inputOnly: ['NC15'], hold: { [FIL]: ['NC127'] }, note: 'a positive leg price (a sell stop with slipBps 10000 trades at 0; a KRON T also refuses the empty delivery; a zero stopPrice is held by `stopPrice > 0` too)', edits: [del('                require(legPrice > 0);\n')] },
  // ---- refund / cancel
  { id: 'E1', file: F, run: { [LIF]: [...both('NC80'), ...both('NC81')] }, note: 'refund only once due (expiry, 90 days idle)', edits: [rx('                // REFUND', /require\(tx\.daa >= idleEnd\);/, 'require(true);')] },
  { id: 'E2', file: F, test: LIF, expect: both('NC83'), note: 'a refund pays the maker everything but refundTip', edits: [del('                require(tx.outputs[self].value + refundTip >= carriers);\n')] },
  { id: 'E3', file: F, test: LIF, expect: both('NC85'), note: 'a refund (or a terminating fill) leaves no output bound to the order id', edits: [rx('        if (cont) {', /require\(OpCovOutputCount\(selfId\) == 0\);/, 'require(true);')] },
  { id: 'X1', file: F, test: LIF, expect: both('NC86'), note: 'cancel: SIGHASH_ALL only', edits: [del('        require(sb[64] == SIGHASH_ALL);\n')] },
  { id: 'X2', file: F, test: LIF, expect: both('NC87'), note: 'cancel: the maker signs', edits: [rep('        require(checkSig(s, maker));', '        require(sb.length == 65);')] },
  // ---- the custody: identity and codec (per family of S), planted custodies (generic)
  { id: 'C3r', file: F, test: CUS, expect: ['NC100sr'], inputOnly: ['NC100sr'], note: 'KRON custody owned by this order (the KRON program also refuses an owner not spent)', edits: [del('                require(byte[32](tin.slice(1, 33)) == selfId);\n')] },
  { id: 'C3k', file: F, test: CUS, expect: ['NC100sk'], inputOnly: ['NC100sk'], note: 'KCC-20 custody owned by this order (the KCC-20 program also refuses an owner not spent)', edits: [del('                require(byte[32](tin.slice(10, 42)) == selfId);\n')] },
  { id: 'C4r', file: F, test: CUS, expect: ['NC101sr'], inputOnly: ['NC101sr'], note: 'KRON custody of owner type 2 (covenant id)', edits: [del('                require(tin.slice(33, 36) == byte[](0x010208));\n')] },
  { id: 'C5r', file: F, test: CUS, expect: ['NC102sr'], note: 'KRON custody not a minter', edits: [del('                require(tin.slice(44, 46) == byte[](0x0100));\n')] },
  { id: 'C4k', file: F, test: CUS, expect: ['NC101sk'], note: 'KCC-20 custody of owner scheme 0x04, borrowing disabled', edits: [del('                require(tin.slice(43, 46) == byte[](0x040100));\n')] },
  { id: 'C6k', file: F, test: CUS, expect: ['NC102sk'], note: 'sFamily is 1 or 2 (hostile field read as KCC-20)', edits: [del('                require(sFamily == FAM_KCC20);\n')] },
  { id: 'C6tk', file: F, test: CUS, expect: ['NC103tk'], note: 'tFamily is 1 or 2 (hostile field read as KCC-20)', edits: [del('            require(tFamily == FAM_KCC20);\n')] },
  { id: 'C0', file: F, test: CUS, expect: both('NC104'), note: 'the custody carries the S covenant (not a UTXO of another token owned by the order)', edits: [del('            require(OpInputCovenantId(custIn) == sCovId);\n')] },
  { id: 'CP2SH', file: F, test: CUS, expect: both('NC105'), note: 'the custody is a P2SH spend of its redeem (a planted UTXO of the S covenant id)', edits: [rx('    function tplRs(int idx, int pre, int stLen, int suf, byte[32] tpl) : byte[] {', /require\(tx\.inputs\[idx\]\.scriptPubKey == byte\[\]\(new ScriptPubKeyP2SHFromRedeemScript\(rs\)\)\);/, 'require(true);')] },
  { id: 'CHASH', file: F, test: CUS, expect: both('NC106'), note: "the custody's template is S's (a planted look-alike P2SH)", edits: [rx('    function tplRs(int idx, int pre, int stLen, int suf, byte[32] tpl) : byte[] {', /require\(blake3\([\s\S]*?\) == tpl\);/, 'require(true);')] },
  // ---- fill parameters, legs, stop band and auction
  { id: 'F1', file: F, test: FIL, expect: ['NC110'], note: 'no fill before activeFrom', edits: [rx('                // FILL', /require\(tx\.daa >= activeFrom\);/, 'require(true);')] },
  { id: 'F2', file: F, hold: { [FIL]: ['NC128'] }, note: 'fill n > 0: defence in depth (n = -1 under a hostile minFill: an ask then needs sOut == -1, a bid tOut == -1, negative token amounts)', edits: [del('                require(n > 0);\n')] },
  { id: 'F3', file: F, test: FIL, expect: ['NC111'], note: 'minimum fill', edits: [del('                require(n >= minFill || n == amountLeft);\n')] },
  { id: 'F4', file: F, test: FIL, expect: ['NC112'], note: 'tip >= 0 (hostile field)', edits: [del('                require(tip >= 0);\n')] },
  { id: 'F5', file: F, test: FIL, expect: ['NC113'], note: 'deliveryCarrier >= 0 (hostile field)', edits: [del('                require(deliveryCarrier >= 0);\n')] },
  { id: 'F6', file: F, test: FIL, expect: ['NC114'], note: 'scale(A) > 0 (a negative scale makes the ceil of the quote negative)', edits: [del('                require(sA > 0);\n')] },
  { id: 'F7', file: F, hold: { [FIL]: ['NC115'] }, note: 'a TP fill needs tpPrice > 0 (defence in depth: a zero TP is also a zero legPrice)', edits: [del('                    require(tpPrice > 0);\n')] },
  { id: 'F7L', file: F, test: FIL, expect: ['NC115'], note: 'tpPrice > 0 and legPrice > 0 both removed: a TP fill of an order without a TP leg', edits: [del('                    require(tpPrice > 0);\n'), del('                require(legPrice > 0);\n')] },
  { id: 'F8', file: F, test: FIL, expect: ['NC116'], note: 'leg is 0 or 1 (leg 2 would fill an unarmed stop without evidence)', edits: [del('                    require(leg == 1);\n')] },
  { id: 'F9', file: F, hold: { [FIL]: ['NC127'] }, note: 'the stop leg needs stopPrice > 0: defence in depth (a zero stop is a zero legPrice)', edits: [del('                    require(stopPrice > 0);\n')] },
  { id: 'F9L', file: F, test: FIL, expect: ['NC127'], note: 'stopPrice > 0 and legPrice > 0 both removed: an armed stop of hostile stopPrice 0 sells for one unit of B', edits: [del('                    require(stopPrice > 0);\n'), del('                require(legPrice > 0);\n')] },
  { id: 'F10', file: F, test: FIL, expect: ['NC118'], note: 'slipBps >= 0 (hostile field)', edits: [del('                    require(slipBps >= 0);\n')] },
  { id: 'F11', file: F, test: FIL, expect: ['NC119'], note: 'slipBps <= 10000', edits: [del('                    require(slipBps <= 10000);\n')] },
  { id: 'F12', file: F, test: FIL, expect: ['NC120'], note: 'the auction time t is proven by CLTV', edits: [del('                            require(tx.daa >= t);\n')] },
  { id: 'F13', file: F, test: FIL, expect: ['NC121'], note: 'the auction time t is not before the origin', edits: [del('                            require(t >= newArmed);\n')] },
  { id: 'F14', file: F, test: FIL, expect: ['NC122'], note: 'the auction band opens linearly over bandDaa', edits: [del('                            if (e < bandDaa) { bps = slipBps * e / bandDaa; }\n')] },
  { id: 'F15', file: F, test: FIL, expect: ['NC123'], note: 'the fill that arms a stop with bandDaa > 0 trades at the stop itself', edits: [del('                        if (bandDaa > 0) {\n                            bps = 0;\n                        }\n')] },
  { id: 'F16', file: F, hold: { [OV]: ['NC92'] }, note: 'stopPrice <= MAX_STOP (defence in depth: a larger stop overflows the quote product anyway)', edits: [del('                    require(stopPrice <= MAX_STOP);\n')] },
  { id: 'F17', file: F, test: FIL, expect: ['NC124'], note: 'an ask releases exactly n', edits: [del('                    require(sOut == n);\n')] },
  { id: 'F18', file: F, test: FIL, expect: ['NC125'], note: 'a bid pays a non-negative amount', edits: [del('                    require(sOut >= 0);\n')] },
  { id: 'F19', file: F, test: FIL, expect: ['NC126'], note: 'the escrow pays the quote (an unfunded bid cannot be filled)', edits: [del('                require(outAmount >= 0);\n')] },
  { id: 'F20', file: F, test: FIL, expect: ['NC117'], note: 'side is 1 or 2 (a fill of hostile side 3)', edits: [rx('                int sA = tScale;', /require\(side == SIDE_BID\);/, 'require(true);')] },
  { id: 'F21', file: F, hold: { [MISC]: ['NC142'] }, note: 'side is 1 or 2 in the arming block: defence in depth (pairEv reads a side-3 order as an ASK, so its evidence token check refuses the BID evidence)', edits: [rx('            int sB = tScale;', /require\(side == SIDE_BID\);/, 'require(true);')] },
  // ---- the T template source, rests, returns, carriers
  { id: 'T1', file: F, test: CAR, expect: both('NC130'), inputOnly: both('NC130'), hold: { [CAR]: both('NC131') }, note: "the T template source's hash (a planted look-alike; the T program also refuses the bound look-alike output)", edits: [del('                    require(blake3(byte[](tPre as byte[8]) + oPre + byte[](tSuf as byte[8]) + oSuf) == tTplHash);\n')] },
  { id: 'T2', file: F, hold: { [CAR]: both('NC131') }, note: 'the T template source carries the T covenant: defence in depth (any T-covenant input is a genuine T program input whose bytes the hash pins; another program fails the hash)', edits: [del('                    require(OpInputCovenantId(tTplIn) == tCovId);\n')] },
  { id: 'K2', file: F, test: CAR, expect: both('NC132'), note: 'the continuation is this script with the new mutable window', edits: [del('            require(tx.outputs[selfOut].scriptPubKey == contSpk(newStop, newArmed, cLeft, cCust));\n')] },
  { id: 'K3', file: F, test: CAR, expect: both('NC133'), note: 'the continuation keeps its floor (order value - deliveryCarrier - tip)', edits: [del('            require(tx.outputs[selfOut].value >= floor);\n')] },
  { id: 'K4', file: F, test: CAR, expect: both('NC134'), note: 'the custody rest keeps its carrier', edits: [rx('                    floor = tx.inputs[self].value - deliveryCarrier - tipKas;', /require\(tx\.outputs\[custIn\]\.value >= tx\.inputs\[custIn\]\.value\);/, 'require(true);')] },
  { id: 'K5', file: F, test: CAR, expect: both('NC135'), note: 'the delivery carries deliveryCarrier', edits: [rx('                    floor = tx.inputs[self].value - deliveryCarrier - tipKas;', /require\(tx\.outputs\[self\]\.value >= deliveryCarrier\);/, 'require(true);')] },
  { id: 'K6', file: F, test: CAR, expect: ['NC136a'], note: 'a return keeps its carrier', edits: [rx('                            // what is left of the custody returns', /require\(tx\.outputs\[custIn\]\.value >= tx\.inputs\[custIn\]\.value\);/, 'require(true);')] },
  { id: 'K7', file: F, test: CAR, expect: ['NC136b'], note: 'done with a return: the maker gets the order value but the tip', edits: [del('                            require(tx.outputs[self].value >= tx.inputs[self].value - tipKas);\n')] },
  { id: 'K8', file: F, test: CAR, expect: both('NC137'), note: 'sold out: every carrier back to the maker but the tip', edits: [del('                            require(tx.outputs[self].value >= carriers - tipKas);\n')] },
  { id: 'Q7', file: F, test: CAR, expect: ['NC138'], note: 'a rest keeps something in the custody (no continuation with an empty escrow)', edits: [del('                    require(outAmount > 0);\n')] },
  // ---- evidence mode argument, the B quote, trailing gap / step
  { id: 'EVMODE', file: F, test: MISC, expect: ['NC140'], note: 'evMode is 0 or 1', edits: [del('            require(evMode == 1);\n')] },
  { id: 'EVB0', file: F, test: MISC, expect: ['NC141'], inputOnly: ['NC141'], note: 'the B quote is positive (a buy stop would arm on any B ask at 0)', edits: [del('        require(b > 0);\n')] },
  { id: 'TRGAP', file: F, test: MISC, expect: ['NC143'], note: 'trailGap >= 0 (hostile field: a sell stop would trail above the rate)', edits: [rx(A_TRAIL, /require\(trailGap >= 0\);/, 'require(true);')] },
  { id: 'TRSTEP', file: F, hold: { [MISC]: ['NC144'] }, note: 'trailStep > 0: defence in depth (a zero / negative step makes validity and maximality contradict)', edits: [rx(A_TRAIL, /require\(trailStep > 0\);/, 'require(true);')] },
  { id: 'TRX', file: F, hold: { [MISC]: ['NC145'] }, note: "a buy stop's s' - gap >= 0: defence in depth (a negative x makes the validity a <= floor(x * b / sB) fail)", edits: [rx(A_TRAIL, /require\(x >= 0\);/, 'require(true);')] },
];
mutations.push(
  // both template checks of the evidence removed: the order's own counterparties (wrong side) and the order / a token
  // input / a P2PK input named as evidence
  { id: 'EVTPL', file: F, hold: { [EV0]: both('NC26'), [EXP]: ['NC54', 'NC55', 'NC56'] }, note: 'evidence template hash and P2SH both removed: the wrong side (the order own counterparties) and the order / a token / a P2PK input are still refused (state read at the other template offsets fails the token / scale identity; the fill push refuses token / P2PK inputs)', edits: [rx('    function tplState(int idx, int pre, int stLen, int suf, byte[32] tpl) : byte[] {', /require\(tx\.inputs\[idx\]\.scriptPubKey == byte\[\]\(new ScriptPubKeyP2SHFromRedeemScript\(rs\)\)\);\n\s*require\(blake3\([\s\S]*?\) == tpl\);/, 'require(true);')] },
);
