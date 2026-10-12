// KRON sell-side suite: contracts/adapters/kron/v2 (KobAskKron, KobBidKron, KobCondAskKron, KobIfdBidKron), protocol v3
// (amounts in base units, minFill, quoteOf rounding in the maker's favour, merge argument -(k * 2^53 + m)).
// Ported from the ad-hoc ablate_sell.mjs; ids and expectations preserved where the check still exists. Each mutation must
// flip its scenarios on BOTH pinned KRON templates (the tests loop over both and print "=== KRON template <file>").
// Touch trigger: the T*-* entries (lib/touch.mjs generates one per evidence check; see README). The V3-* entries mutate
// the checks protocol v3 added (minFill, rounding direction, tip <= price, the 0x08 merge checks, the update parent guard,
// n < MERGE_K on booking and on a re-arming take-profit, the merge exit at the entry's scale).
// Audit completion (2026-10-13): every require of the four templates has an entry (the AU-KA / AU-KB / AU-KCA / AU-KIB
// blocks at the end; new witnesses in the v3_audit_*_kron test fns, ids NAU-K* / PAU-K*), or is listed here.
// Checks without a witness attack (not in the catalog):
//  - cancel `require(sb.length == 65)` (KobAskKron, KobBidKron, KobCondAskKron, KobIfdBidKron): `sb[64]` of a shorter
//    signature fails on its own, a longer one is no Schnorr signature for checkSig.
//  - `require(refundTip >= 0)` (KobAskKron settle n = 0, KobBidKron refund, KobCondAskKron settle n = 0, KobIfdBidKron
//    refund): its only use is `tx.outputs[self].value + refundTip >= <carriers / escrow>`, so a negative refundTip raises
//    what the maker must receive. It only tightens the keeper's limit; no transaction becomes acceptable without it.
//  - KobCondAskKron update `require(side == SIDE_BID)`: touch() returns SIDE_ASK (tk >= 0) or SIDE_BID (tk < 0) and
//    nothing else, so the else branch of `if (side == SIDE_ASK)` is only entered with side == SIDE_BID.
// Input-only flips (the token program refuses the same transaction on its own input): the stray-scan bounds (a 5th token
// input), an id_type 3 custody of the order's id (no input is P2PK of the id), the zero fills (an empty delivery).
// Defence in depth (hold + combined): fill n > 0 and the minFill rule (AU-KA-13 / 13x, AU-KCA-15 / 15x); the IFD exit
// script equality and its genesis id, the covenant id being derived from the script (AU-KIB-18 / 18x); the exit template
// hash holds NI4 (also refused by the script equality) and flips NAU-KIB34, a look-alike exit template.
import { del, rep, rx, rxDel } from '../lib/edits.mjs';
import { touchMutations } from '../lib/touch.mjs';

export const suite = {
  name: 'kron-sell',
  family: 'KRON adapter (protocol v3, sell-side contracts)',
  testBin: 'kob_kron_v2_tests',
  srcDir: 'contracts/adapters/kron/v2',
  templateMarker: /^=== KRON template (\S+)/,
  expectedTemplates: 2,
};

const T_AUC = 'v2_lifecycle_auctions_and_kill';
const T_STOP = 'v2_lifecycle_stop_auction_keeper_trailing';
const T_IFD = 'v2_lifecycle_ifd_stop_entry';
const T_STRAY = 'v2_lifecycle_stray_custody';
const T_RPT = 'v2_repeat_ifd_attacks';
const T_TRIG = 'v2_negative_trigger_manipulation';
const T_EXTRA = 'v2_kron_v23_extra_attacks';
// touch trigger batteries, one test fn per trigger kind
const T_SET = 'v2_touch_cond_settle';
const T_ARM = 'v2_touch_cond_arm';
const T_TRL = 'v2_touch_cond_trail';
const T_IF = 'v2_touch_ifd_fill';
const T_IA = 'v2_touch_ifd_arm';
// protocol v3
const T_MIN = 'v3_min_fill';
const T_RND = 'v3_rounding';
const T_TIP = 'v3_tip';
const T_PUSH = 'v3_merge_push';
const T_LIM = 'v3_merge_limits';
// audit completion (2026-10-13): every require of the four templates
const T_TCE = 'v2_negative_tif_cancel_expiry';
const T_NLT = 'v2_negative_limits_and_tips';
const T_NTV = 'v2_negative_time_variants';
const T_KAS = 'v2_kron_attacks_ask_side';
const T_KBS = 'v2_kron_attacks_bid_side';
const T_KCI = 'v2_kron_attacks_conditional_and_ifd';
const T_OCO = 'v2_negative_oco_and_ifd';
const T_AU_KA = 'v3_audit_ask_kron';
const T_AU_KB = 'v3_audit_bid_kron';
const T_AU_KCA = 'v3_audit_cond_ask_kron';
const T_AU_KIB = 'v3_audit_ifd_bid_kron';

export const mutations = [
  // ---- KobAskKron
  {
    id: 'A1',
    file: 'KobAskKron',
    test: T_EXTRA,
    expect: ['KS9'],
    note: 'decayStep > 0',
    edits: [
      del(`                require(decayStep > 0);
`),
    ],
  },
  {
    id: 'A2',
    file: 'KobAskKron',
    test: T_AUC,
    expect: ['NV10', 'NV11'],
    note: 'per-slice decay origin',
    edits: [
      del(`                if (interval > 0) {
                    int slice = utxoDaa + interval;
                    if (slice > origin) { origin = slice; }
                }
`),
    ],
  },
  {
    id: 'A3',
    file: 'KobAskKron',
    test: T_EXTRA,
    expect: ['KS10'],
    note: 't >= origin',
    edits: [
      del(`                require(t >= origin);
`),
    ],
  },
  {
    id: 'A4',
    file: 'KobAskKron',
    test: T_AUC,
    expect: ['NM3'],
    note: 'auction time CLTV',
    edits: [
      del(`                require(tx.daa >= t);
`),
    ],
  },
  {
    id: 'A5',
    file: 'KobAskKron',
    test: T_AUC,
    expect: ['NM1'],
    note: 'auction price formula (always the bound)',
    edits: [
      rep(`                p = price - slope * ((t - origin) / decayStep);
                if (p < priceEnd) { p = priceEnd; }`, '                p = priceEnd;'),
    ],
  },
  {
    id: 'A6',
    file: 'KobAskKron',
    test: T_AUC,
    expect: ['NC8', 'NC8b'],
    note: 'IOC/FOK kill one DAA early',
    edits: [
      rep('kill = kill + IOC_LIFE;', 'kill = kill + IOC_LIFE - 1;'),
    ],
  },
  {
    id: 'A7',
    file: 'KobAskKron',
    test: T_AUC,
    expect: ['NC8b'],
    note: 'kill counted from UTXO DAA, not activeFrom',
    edits: [
      del('                if (activeFrom > kill) { kill = activeFrom; }\n'),
    ],
  },
  {
    id: 'A8',
    file: 'KobAskKron',
    test: T_AUC,
    expect: ['NC9'],
    note: 'GTC also killable',
    edits: [
      rep(`            if (tif != TIF_GTC) {
                int kill`, `            if (tif >= 0) {
                int kill`),
    ],
  },
  {
    id: 'A9',
    file: 'KobAskKron',
    test: T_STRAY,
    expect: ['NS2'],
    note: 'exact custody amount (custody == amountLeft)',
    edits: [
      del(`        require(tinAmount == amountLeft);
`),
    ],
  },
  {
    id: 'A10',
    file: 'KobAskKron',
    test: T_STRAY,
    expect: ['NS3', 'NS4'],
    note: 'stray scan',
    edits: [
      del(`        noStrays(selfId, tokenIn);
`),
    ],
  },
  {
    id: 'A11',
    file: 'KobAskKron',
    test: T_STRAY,
    expect: ['NS1', 'NS2', 'NS3', 'NS4'],
    note: 'exact custody + stray scan (NS1 is double-locked)',
    edits: [
      del(`        require(tinAmount == amountLeft);
`),
      del(`        noStrays(selfId, tokenIn);
`),
    ],
  },
  {
    id: 'A12',
    file: 'KobAskKron',
    test: T_EXTRA,
    expect: ['KS2'],
    note: 'continuation amountLeft not decremented',
    edits: [
      rep('contSpk(amountLeft - n)', 'contSpk(amountLeft)'),
    ],
  },
  // ---- KobBidKron
  {
    id: 'B1',
    file: 'KobBidKron',
    test: T_AUC,
    expect: ['NC10'],
    note: 'IOC/FOK kill one DAA early',
    edits: [
      rep('kill = kill + IOC_LIFE;', 'kill = kill + IOC_LIFE - 1;'),
    ],
  },
  {
    id: 'B2',
    file: 'KobBidKron',
    test: T_AUC,
    expect: ['NC10b'],
    note: 'GTC also killable',
    edits: [
      rep(`        if (tif != TIF_GTC) {
            int kill`, `        if (tif >= 0) {
            int kill`),
    ],
  },
  {
    id: 'B3',
    file: 'KobBidKron',
    test: T_STRAY,
    expect: ['NS5'],
    note: 'fill stray scan',
    edits: [
      del(`        noStrays(selfId);
`),
    ],
  },
  {
    id: 'B4',
    file: 'KobBidKron',
    test: T_STRAY,
    expect: ['NS6'],
    note: 'refund refuses token inputs',
    edits: [
      del(`        require(OpCovInputCount(tokenCovId) == 0);  // strays owned by this id stay put
`),
    ],
  },
  {
    id: 'B5',
    file: 'KobBidKron',
    test: T_AUC,
    expect: ['NM4'],
    note: 'rising price formula (always the bound)',
    edits: [
      rep(`            p = price + slope * ((t - origin) / decayStep);
            if (p > priceEnd) { p = priceEnd; }`, '            p = priceEnd;'),
    ],
  },
  {
    id: 'B6',
    file: 'KobBidKron',
    test: T_EXTRA,
    expect: ['KS5'],
    note: 'decayStep > 0',
    edits: [
      del(`            require(decayStep > 0);
`),
    ],
  },
  {
    id: 'B7',
    file: 'KobBidKron',
    test: T_EXTRA,
    expect: ['KS6'],
    note: 'per-slice rising origin',
    edits: [
      del(`            if (interval > 0) {
                int slice = OpTxInputDaaScore(self) + interval;
                if (slice > origin) { origin = slice; }
            }
`),
    ],
  },
  // ---- KobCondAskKron
  {
    id: 'C1',
    file: 'KobCondAskKron',
    test: T_STOP,
    expect: ['NA2'],
    note: 'auction time CLTV',
    edits: [
      del(`                        require(tx.daa >= t);
`),
    ],
  },
  {
    id: 'C2',
    file: 'KobCondAskKron',
    test: T_STOP,
    expect: ['NA3'],
    note: 't >= arming origin',
    edits: [
      del(`                        require(t >= newArmed);
`),
    ],
  },
  {
    id: 'C3',
    file: 'KobCondAskKron',
    test: T_STOP,
    expect: ['NA1'],
    note: 'stop auction ramp (always the floor)',
    edits: [
      rep('if (e < bandDaa) { bps = slipBps * e / bandDaa; }', 'bps = slipBps;'),
    ],
  },
  {
    id: 'C4',
    file: 'KobCondAskKron',
    test: T_STOP,
    expect: ['NA5'],
    note: 'arm + fill pays the stop',
    edits: [
      del(`                    if (bandDaa > 0) {
                        bps = 0;
                    }
`),
    ],
  },
  {
    id: 'C5',
    file: 'KobCondAskKron',
    test: T_STOP,
    expect: ['NA4'],
    note: 'origin recorded at the first auction fill',
    edits: [
      del(`            if (armed == 1 && bandDaa > 0) {
                // record the auction origin (the arming transaction's DAA) for the remainder
                newArmed = OpTxInputDaaScore(self);
            }
`),
    ],
  },
  {
    id: 'C6',
    file: 'KobCondAskKron',
    test: T_STOP,
    expect: ['NA6'],
    note: 'continuation SPK equality (settle)',
    edits: [
      del(`                require(tx.outputs[selfOut].scriptPubKey == contSpk(stopPrice, newArmed, amountLeft - n));
`),
    ],
  },
  {
    id: 'C7',
    file: 'KobCondAskKron',
    test: T_STOP,
    expect: ['NK1'],
    note: 'keeper takes at most keeperTip',
    edits: [
      del(`        require(tx.outputs[selfOut].value >= tx.inputs[self].value - keeperTip);
`),
    ],
  },
  {
    id: 'C8',
    file: 'KobCondAskKron',
    test: T_STOP,
    expect: ['NK2'],
    note: 'keeperTip >= 0',
    edits: [
      del(`        require(keeperTip >= 0);
`),
    ],
  },
  {
    id: 'C9',
    file: 'KobCondAskKron',
    test: T_STOP,
    expect: ['NT18b'],
    note: 'trail capped below TP',
    edits: [
      del(`                if (kt < k) { k = kt; }
`),
    ],
  },
  {
    id: 'C10',
    file: 'KobCondAskKron',
    test: T_TRIG,
    expect: ['NT17'],
    note: 'trail exactly k steps (one step)',
    edits: [
      rep('newStop = stopPrice + k * trailStep;', 'newStop = stopPrice + trailStep;'),
    ],
  },
  {
    id: 'C11',
    file: 'KobCondAskKron',
    test: T_TRIG,
    expect: ['NT17b'],
    note: 'trail exactly k steps (k + 1)',
    edits: [
      rep('newStop = stopPrice + k * trailStep;', 'newStop = stopPrice + (k + 1) * trailStep;'),
    ],
  },
  {
    id: 'C12',
    file: 'KobCondAskKron',
    test: T_STRAY,
    expect: ['NS7b'],
    note: 'exact custody amount (custody == amountLeft)',
    edits: [
      del(`        require(tinAmount == amountLeft);
`),
    ],
  },
  {
    id: 'C13',
    file: 'KobCondAskKron',
    test: T_STRAY,
    expect: ['NS7'],
    note: 'stray scan',
    edits: [
      del(`        noStrays(selfId, tokenIn);
`),
    ],
  },
  {
    id: 'C14',
    file: 'KobCondAskKron',
    test: T_RPT,
    expect: ['NRP10'],
    note: 'TP without the entry only from rptUntil',
    edits: [
      del(`                        require(tx.daa >= rptUntil);
`),
    ],
  },
  {
    id: 'C15',
    file: 'KobCondAskKron',
    test: T_RPT,
    expect: ['NRP13'],
    note: 'stop leg refuses the entry',
    edits: [
      del(`                    require(present == 0);
`),
    ],
  },
  {
    id: 'C16',
    file: 'KobCondAskKron',
    test: T_RPT,
    expect: ['NRP14'],
    hold: { [T_RPT]: ['NRP28'] },
    note: 'entry\'s merge argument names this exit and n (NRP28, an arming update of the entry, also fails the 0x08 check: see C16b)',
    edits: [
      del(`                require(int(byte[8](OpTxInputScriptSigSubstr(pin, 1, 9))) == 0 - (self * MERGE_K + n));
`),
    ],
  },
  {
    id: 'C16b',
    file: 'KobCondAskKron',
    test: T_RPT,
    expect: ['NRP28'],
    note: 'merge argument value and the entry\'s leading 0x08 both removed: an arming update of the entry passes as its merge',
    edits: [
      del(`                require(OpTxInputScriptSigSubstr(pin, 0, 1) == byte[](0x08));
`),
      del(`                require(int(byte[8](OpTxInputScriptSigSubstr(pin, 1, 9))) == 0 - (self * MERGE_K + n));
`),
    ],
  },
  {
    id: 'C17',
    file: 'KobCondAskKron',
    test: T_RPT,
    expect: ['NRP19'],
    note: 'merge-n binding on both sides (exit AND entry check removed)',
    edits: [
      del(`                require(int(byte[8](OpTxInputScriptSigSubstr(pin, 1, 9))) == 0 - (self * MERGE_K + n));
`),
    ],
    also: [{ file: 'KobIfdBidKron', edits: [
        del(`            require(int(byte[8](OpTxInputScriptSigSubstr(k, 1, 9))) == m);
`),
      ] }],
  },
  {
    id: 'C18',
    file: 'KobCondAskKron',
    test: T_RPT,
    expect: ['NRP11'],
    note: 'maker payout floor (partial re-arming TP)',
    edits: [
      rep(`                require(tx.outputs[tokOut].value >= tx.inputs[tokenIn].value);
                require(tx.outputs[self].value >= allIn);`, '                require(tx.outputs[tokOut].value >= tx.inputs[tokenIn].value);'),
    ],
  },
  {
    id: 'C19',
    file: 'KobCondAskKron',
    test: T_RPT,
    expect: ['NRP12'],
    note: 'sell-out: entry gets the exit carriers',
    edits: [
      del(`                    require(tx.outputs[OpCovOutputIdx(parent, 0)].value >= tx.inputs[pin].value + rptBudget + carriers);
`),
    ],
  },
  // ---- KobIfdBidKron
  {
    id: 'I1',
    file: 'KobIfdBidKron',
    test: T_IFD,
    expect: ['NI13'],
    note: 'unarmed stop entry reads its trigger evidence (touch)',
    edits: [
      rep(`                if (armed == 0) {
                    require(touchBid(ev) >= entryStop);
                    newArmed = 1;`, `                if (armed == 0) {
                    newArmed = 1;`),
    ],
  },
  {
    id: 'I5',
    file: 'KobIfdBidKron',
    test: T_IFD,
    expect: ['NI16'],
    note: 'entry auction price ramp',
    edits: [
      rep('                            p = entryStop + (price - entryStop) * e / bandDaa;', '                            p = price;'),
    ],
  },
  {
    id: 'I6',
    file: 'KobIfdBidKron',
    test: T_IFD,
    expect: ['NI17'],
    note: 'auction origin recorded',
    edits: [
      del(`                        if (armed == 1) {
                            newArmed = OpTxInputDaaScore(self);
                        }
`),
    ],
  },
  {
    id: 'I7',
    file: 'KobIfdBidKron',
    test: T_IFD,
    expect: ['NI17b'],
    note: 'armed carried into the continuation',
    edits: [
      rep(`                    require(touchBid(ev) >= entryStop);
                    newArmed = 1;`, '                    require(touchBid(ev) >= entryStop);'),
    ],
  },
  {
    id: 'I8',
    file: 'KobIfdBidKron',
    run: { [T_IFD]: ['NI18'], [T_MIN]: ['V3IB01'] },
    note: 'minFill (n >= minFill unless the fill takes everything left)',
    edits: [
      del(`            require(n >= minFill || n == amountLeft);
`),
    ],
  },
  {
    id: 'I9',
    file: 'KobIfdBidKron',
    test: T_IFD,
    expect: ['NI19'],
    note: 'entryStop <= price',
    edits: [
      del(`                require(entryStop <= price);
`),
    ],
  },
  {
    id: 'I10',
    file: 'KobIfdBidKron',
    test: T_IFD,
    expect: ['NI20'],
    note: 'update needs a stop entry',
    edits: [
      del(`        require(entryStop > 0);
`),
    ],
  },
  {
    id: 'I11',
    file: 'KobIfdBidKron',
    test: T_IFD,
    expect: ['NI21'],
    note: 'keeper takes at most keeperTip',
    edits: [
      del(`        require(tx.outputs[c].value >= tx.inputs[self].value - keeperTip);
`),
    ],
  },
  {
    id: 'I12',
    file: 'KobIfdBidKron',
    test: T_IFD,
    expect: ['NI23'],
    note: 'update continuation SPK equality',
    edits: [
      del(`        require(tx.outputs[c].scriptPubKey == contSpk(amountLeft, 1, rptAmount));
`),
    ],
  },
  {
    id: 'I13',
    file: 'KobIfdBidKron',
    test: T_STRAY,
    expect: ['NS8'],
    note: 'fill stray scan',
    edits: [rxDel('    entry fill(', /^\s*noStrays\(selfId\);$/m)],
  },
  {
    id: 'I14',
    file: 'KobIfdBidKron',
    run: { [T_EXTRA]: ['KS3'], [T_IA]: ['NTJ23'] },
    note: 'update refuses token inputs owned by the entry (touch: the evidence fill brings foreign token inputs)',
    edits: [rxDel('    entry update(int ev) {', /^\s*noStrays\(selfId\);$/m)],
  },
  {
    id: 'I15',
    file: 'KobIfdBidKron',
    test: T_EXTRA,
    expect: ['KS4'],
    note: 'refund refuses token inputs',
    edits: [
      rep(`        require(OpCovInputCount(tokenCovId) == 0);
        require(OpCovOutputCount(selfId) == 0);`, '        require(OpCovOutputCount(selfId) == 0);'),
    ],
  },
  {
    id: 'I16',
    file: 'KobIfdBidKron',
    test: T_EXTRA,
    expect: ['KS8'],
    note: 'update keeperTip >= 0',
    edits: [
      del(`        require(keeperTip >= 0);
`),
    ],
  },
  {
    id: 'R1',
    file: 'KobIfdBidKron',
    test: T_RPT,
    hold: { [T_RPT]: ['NRP21'] },
    note: 'exit is a genuine KobCondAsk (template + P2SH check) (the crafted input also fails the exit-terms comparison, confirmed on its own by FXL3a/b, so removing the template check alone holds)',
    edits: [
      rep('            byte[] x = tplState(k, COND_PRE, EXIT_STATE_LEN, COND_SUF, COND_TPL);', `            int xsz = COND_PRE + EXIT_STATE_LEN + COND_SUF;
            int xb = OpTxInputScriptSigLen(k) - xsz;
            byte[] x = OpTxInputScriptSigSubstr(k, xb, xb + xsz).slice(COND_PRE, COND_PRE + EXIT_STATE_LEN);`),
    ],
  },
  {
    id: 'R2',
    file: 'KobIfdBidKron',
    test: T_RPT,
    hold: { [T_RPT]: ['NRP20'] },
    note: 'exit\'s parent = this entry (the parent check is part of the exit-tail comparison with rptPrice; a plain exit also fails on rptPrice, so removing the parent alone holds)',
    edits: [
      rep('byte[](0x20) + byte[](selfId) + byte[](0x08)', 'byte[](0x20) + x.slice(280, 312) + byte[](0x08)'),
    ],
  },
  {
    id: 'R3',
    file: 'KobIfdBidKron',
    run: { [T_PUSH]: ['V3IB05'] },
    hold: { [T_RPT]: ['NRP22'] },
    note: 'exit sells exactly m (NRP22, beside the exit\'s update, is also refused by the exit\'s parent guard and the 0x08 check)',
    edits: [
      del(`            require(int(byte[8](OpTxInputScriptSigSubstr(k, 1, 9))) == m);
`),
    ],
  },
  {
    id: 'R4',
    file: 'KobIfdBidKron',
    test: T_RPT,
    expect: ['NRP23'],
    note: 'm > 0',
    edits: [
      del(`            require(m > 0);
`),
    ],
  },
  {
    id: 'R5',
    file: 'KobIfdBidKron',
    test: T_RPT,
    expect: ['NRP16', 'NRP17', 'NRP17b', 'NRP18'],
    note: 'continuation SPK equality (fill/merge)',
    edits: [
      del(`            require(tx.outputs[c].scriptPubKey == contSpk(newLeft, newArmed, newRpt));
`),
    ],
  },
  {
    id: 'R6',
    file: 'KobIfdBidKron',
    test: T_RPT,
    expect: ['NRP18'],
    note: 'empty stop entry re-arms unarmed',
    edits: [
      rep(`if (amountLeft == 0) {
                newArmed = 0;
`, `if (amountLeft == 0) {
`),
    ],
  },
  {
    id: 'R7',
    file: 'KobIfdBidKron',
    test: T_RPT,
    expect: ['NRP15'],
    note: 'merge floor (the budget of m)',
    edits: [
      rep('floor = tx.inputs[self].value + quoteOf(m, rate, scale - 1);', 'floor = tx.inputs[self].value + quoteOf(m, rate, scale - 1) - 1;'),
    ],
  },
  {
    id: 'R8',
    file: 'KobIfdBidKron',
    test: T_RPT,
    expect: ['NRP24'],
    note: 'stray scan in merge (shares the fill guard)',
    edits: [rxDel('    entry fill(', /^\s*noStrays\(selfId\);$/m)],
  },
  {
    id: 'R9',
    file: 'KobIfdBidKron',
    test: T_RPT,
    expect: ['NRP6'],
    note: "booking: the exit's rptUntil counts from the entry UTXO's DAA (its last activity), never from the filler's t (the cycle dated by t instead)",
    edits: [
      rep(`                int idleEnd = OpTxInputDaaScore(self) + MAX_IDLE;
                xUntil`, `                int idleEnd = t + MAX_IDLE;
                xUntil`),
    ],
  },
  {
    id: 'R10',
    file: 'KobIfdBidKron',
    test: T_RPT,
    expect: ['NRP7'],
    note: 'repeating entry never terminates on a fill',
    edits: [
      rep('if (newLeft > 0 || rptAmount > 0) {', 'if (newLeft > 0) {'),
    ],
  },
  {
    id: 'R11',
    file: 'KobIfdBidKron',
    test: T_RPT,
    expect: ['NRP8'],
    note: 'continuation floor on a fill',
    edits: [
      rep('floor = left - exitCarrier;', 'floor = left - exitCarrier - 1;'),
    ],
  },
  {
    id: 'R12',
    file: 'KobIfdBidKron',
    test: T_RPT,
    expect: ['NRP3'],
    note: 'cycle count decremented',
    edits: [
      rep('newRpt = rptAmount - n;', 'newRpt = rptAmount;'),
    ],
  },
  {
    id: 'R13',
    file: 'KobIfdBidKron',
    test: T_RPT,
    expect: ['NRP4'],
    note: 'exit rptPrice',
    edits: [
      rep('xPrice = price + tip;', 'xPrice = price + tip - 1;'),
    ],
  },
  {
    id: 'R14',
    file: 'KobIfdBidKron',
    test: T_RPT,
    expect: ['NRP5'],
    note: 'exit rptUntil',
    edits: [
      rep('xUntil = expiryDaa < idleEnd ? expiryDaa : idleEnd;', 'xUntil = (expiryDaa < idleEnd ? expiryDaa : idleEnd) - 1;'),
    ],
  },
  {
    id: 'R15',
    file: 'KobIfdBidKron',
    test: T_RPT,
    expect: ['NRP1'],
    note: 'booking writes a plain exit (parent, rptPrice, rptUntil) although re-arms remain',
    edits: [
      rep('xParent = selfId;', 'xParent = ZERO32;'),
      rep('xPrice = price + tip;', 'xPrice = 0;'),
      rep('xUntil = expiryDaa < idleEnd ? expiryDaa : idleEnd;', 'xUntil = 0;'),
    ],
  },
  {
    id: 'R16',
    file: 'KobIfdBidKron',
    test: T_RPT,
    expect: ['NRP2'],
    note: 'no booking once the re-arms are exhausted (rptAmount > n)',
    edits: [
      rep('if (rptAmount > n) {', 'if (rptAmount >= n) {'),
    ],
  },
  {
    id: 'R17',
    file: 'KobIfdBidKron',
    test: T_RPT,
    expect: ['NRP3b'],
    note: 'continuation keeps the repeat count',
    edits: [
      rep('newRpt = rptAmount - n;', 'newRpt = 0;'),
    ],
  },
  // ---- fix pass (2026-09-30)
  {
    id: 'FXE1',
    file: 'KobAskKron',
    test: 'v2_kron_fix_pass_regressions',
    expect: ['FXE1', 'FXE2'],
    note: 'the IOC return is positional (tokOut == tokenIn): no shared remainder, no remainder that is also a bid delivery',
    edits: [del('                    require(tokOut == tokenIn);\n')],
  },
  {
    id: 'FXL3a',
    file: 'KobIfdBidKron',
    test: T_RPT,
    expect: ['NRP25', 'NRP27'],
    note: 'the merge requires the exit terms [0..181) (maker, legs) to be the committed exitState',
    edits: [del('            require(x.slice(0, 181) == st.slice(0, 181));\n')],
  },
  {
    id: 'FXL3b',
    file: 'KobIfdBidKron',
    test: T_RPT,
    expect: ['NRP26'],
    note: 'the merge requires parent = this entry and rptPrice = its budget rate',
    edits: [rxDel('int rate = price + tip;', /^\s*require\(x\.slice\(279, 322\) == [^\n]*\);$/m)],
  },
  {
    id: 'V3-I-scale',
    file: 'KobIfdBidKron',
    test: T_RPT,
    expect: ['NRP30'],
    note: "merge: the committed exit (exitState, compared with the exit's state) quotes at the entry's scale (else the exit returns budget(m) at its scale and the entry re-arms m at its own)",
    edits: [del('            require(int(byte[8](st.slice(118, 126))) == scale);\n')],
  },
  // ---- touch trigger: KobCondAsk touchAsk (stop leg armed inside its fill), touch (update: arm / trail)
  ...touchMutations({
    prefix: 'TA',
    file: 'KobCondAskKron',
    fn: 'function touchAsk(int ev, int tk) : int {',
    what: 'touchAsk (settle, stop leg)',
    runs: {
      push08: { [T_SET]: ['NTA10'] },
      npos: { [T_SET]: ['NTA11'] },
      tpl: { [T_SET]: ['NTA17'] },
      p2sh: { [T_SET]: ['NTA18'] },
      token: { [T_SET]: ['NTA07b'] },
      scale: { [T_SET]: ['NTA08'] },
      mintouch: { [T_SET]: ['NTA09'], [T_TRIG]: ['NT5'] },
      slope: { [T_SET]: ['NTA05'] },
      cltv: { [T_SET]: ['NTA01', 'NTA02', 'NTA03', 'NTA04'] },
      interval: { [T_SET]: ['NTA04'] },
      active: { [T_SET]: ['NTA03'] },
      tokdaa: { [T_SET]: ['NTA02'] },
      tkcov: { [T_SET]: ['NTA02b'] },
      owner: { [T_SET]: ['NTA15', 'NTA16'] },
      // the look-alike holder / non-P2SH input is also refused by the evidence ask's stray guard and the token program
      tktpl: { run: { [T_SET]: ['NTA02c'] }, inputOnly: ['NTA02c'] },
      tkp2sh: { run: { [T_SET]: ['NTA02d'] }, inputOnly: ['NTA02d'] },
    },
  }),
  {
    id: 'TA-rule',
    file: 'KobCondAskKron',
    run: { [T_SET]: ['NTA19'], [T_TRIG]: ['NT3'] },
    note: 'settle, unarmed stop leg: the evidence ask quotes <= stopPrice',
    edits: [rxDel('    entry settle(', /^\s*require\(rp <= stopPrice\);$/m)],
  },
  ...touchMutations({
    prefix: 'TU',
    file: 'KobCondAskKron',
    fn: 'function touch(int ev, int tk) : (int, int) {',
    what: 'touch (update: arm on an ask, trail on a bid)',
    runs: {
      push08: { [T_ARM]: ['NTU10'], [T_TRL]: ['NTT10', 'NTT11'] },
      npos: { [T_ARM]: ['NTU11'] },
      tpl: { [T_ARM]: ['NTU17'], [T_TRL]: ['NTT17'] },
      p2sh: { [T_ARM]: ['NTU18'], [T_TRL]: ['NTT18'] },
      token: { [T_ARM]: ['NTU07b'], [T_TRL]: ['NTT07'] },
      scale: { [T_ARM]: ['NTU08'], [T_TRL]: ['NTT08'] },
      mintouch: { [T_ARM]: ['NTU09'], [T_TRL]: ['NTT09'] },
      slope: { [T_ARM]: ['NTU05'], [T_TRL]: ['NTT05'] },
      cltv: { [T_ARM]: ['NTU01', 'NTU02', 'NTU03', 'NTU04'], [T_TRL]: ['NTT01', 'NTT03', 'NTT04'] },
      interval: { [T_ARM]: ['NTU04'], [T_TRL]: ['NTT04'] },
      active: { [T_ARM]: ['NTU03'], [T_TRL]: ['NTT03'] },
      tokdaa: { [T_ARM]: ['NTU02'] },
      tkcov: { [T_ARM]: ['NTU02b'] },
      owner: { [T_ARM]: ['NTU15', 'NTU16'] },
      tktpl: { run: { [T_ARM]: ['NTU02c'] }, inputOnly: ['NTU02c'] },
      tkp2sh: { run: { [T_ARM]: ['NTU02d'] }, inputOnly: ['NTU02d'] },
    },
  }),
  {
    id: 'TU-rule',
    file: 'KobCondAskKron',
    test: T_ARM,
    expect: ['NTU19'],
    note: 'update, arm: the evidence ask quotes <= stopPrice',
    edits: [rxDel('    entry update(int ev, int tk) {', /^\s*require\(rp <= stopPrice\);$/m)],
  },
  {
    id: 'TT-k',
    file: 'KobCondAskKron',
    run: { [T_TRL]: ['NTT19', 'NTT19b'] },
    note: 'update, trail: the evidence bid justifies k >= 1 steps (rp >= stop + step + gap; k = 0 drains keeperTip and resets trailWait, k < 0 moves the stop down)',
    edits: [del('            require(k >= 1);\n')],
  },
  {
    id: 'TT-wait',
    file: 'KobCondAskKron',
    test: T_TRIG,
    expect: ['NT15'],
    note: 'update, trail: at most once per trailWait DAA (CSV)',
    edits: [del('            require(this.ageDaa >= trailWait);\n')],
  },
  {
    id: 'TT-step',
    file: 'KobCondAskKron',
    hold: { [T_TRIG]: ['NT19'], [T_ARM]: ['NTU06'] },
    note: 'update, trail: trailStep > 0 (a non-trailing order divides by trailStep = 0 and fails anyway)',
    edits: [del('            require(trailStep > 0);\n')],
  },
  {
    id: 'TU-armed',
    file: 'KobCondAskKron',
    test: T_TRIG,
    expect: ['NT14'],
    note: 'update only while unarmed',
    edits: [rxDel('    entry update(int ev, int tk) {', /^\s*require\(armed == 0\);$/m)],
  },
  {
    id: 'TU-own',
    file: 'KobCondAskKron',
    run: { [T_ARM]: ['NTU23'], [T_TRL]: ['NTT23'], [T_TRIG]: ['NT13'] },
    note: 'update spends no token input owned by the order (bounded scan; the evidence fill brings foreign token inputs)',
    edits: [del('        noStrays(selfId, -1);\n')],
  },
  // ---- touch trigger: KobIfdBid touchBid (buy-stop entry armed inside its fill or by update)
  ...touchMutations({
    prefix: 'TI',
    file: 'KobIfdBidKron',
    fn: 'function touchBid(int ev) : int {',
    what: 'touchBid (buy-stop entry)',
    runs: {
      push08: { [T_IF]: ['NTI10'], [T_IA]: ['NTJ10', 'NTJ11'] },
      npos: { run: { [T_IF]: ['NTI11b'], [T_IA]: ['NTJ11b'] }, inputOnly: ['NTI11b', 'NTJ11b'] },
      tpl: { [T_IF]: ['NTI17'], [T_IA]: ['NTJ17'] },
      p2sh: { [T_IF]: ['NTI18'], [T_IA]: ['NTJ18'] },
      token: { [T_IF]: ['NTI07'], [T_IA]: ['NTJ07'] },
      scale: { [T_IF]: ['NTI08'], [T_IA]: ['NTJ08'] },
      mintouch: { [T_IF]: ['NTI09'], [T_IA]: ['NTJ09'] },
      slope: { [T_IF]: ['NTI05'], [T_IA]: ['NTJ05'] },
      cltv: { [T_IF]: ['NTI01', 'NTI03', 'NTI04'], [T_IA]: ['NTJ01', 'NTJ03', 'NTJ04'] },
      interval: { [T_IF]: ['NTI04'], [T_IA]: ['NTJ04'] },
      active: { [T_IF]: ['NTI03'], [T_IA]: ['NTJ03'] },
    },
  }),
  {
    id: 'TI-auc',
    file: 'KobIfdBidKron',
    test: 'v2_lifecycle_ifd_stop_entry',
    expect: ['NI16b'],
    note: 'buy-stop entry armed inside its fill with an auction: the fill pays the stop (the auction opens at the trigger)',
    edits: [
      del(`                    if (bandDaa > 0) {
                        p = entryStop;
                    }
`),
    ],
  },
  {
    id: 'TI-rule',
    file: 'KobIfdBidKron',
    test: T_IF,
    expect: ['NTI19'],
    note: 'fill, unarmed stop entry: the evidence bid quotes >= entryStop',
    edits: [rx('    entry fill(', /require\(touchBid\(ev\) >= entryStop\);/, 'require(touchBid(ev) >= 0);')],
  },
  {
    id: 'TJ-rule',
    file: 'KobIfdBidKron',
    test: T_IA,
    expect: ['NTJ19'],
    note: 'update: the evidence bid quotes >= entryStop',
    edits: [rx('    entry update(int ev) {', /require\(touchBid\(ev\) >= entryStop\);/, 'require(touchBid(ev) >= 0);')],
  },

  // ---- update arms only an entry with something to fill and its stop on the limit's side; a merge keeps an armed
  // entry's band origin
  {
    id: 'TJ-amount',
    file: 'KobIfdBidKron',
    test: T_IA,
    expect: ['NTJ24'],
    note: 'update: the entry has something to fill (an empty repeating entry would pay keeperTip for a useless arm)',
    edits: [rxDel('    entry update(int ev) {', /^\s*require\(amountLeft > 0\);$/m)],
  },
  {
    id: 'TJ-limit',
    file: 'KobIfdBidKron',
    test: T_IA,
    expect: ['NTJ25'],
    note: "update: the stop is on the limit's side (entryStop <= price; else no fill can follow the arm)",
    edits: [rxDel('    entry update(int ev) {', /^\s*require\(entryStop <= price\);$/m)],
  },
  {
    id: 'TJ-origin',
    file: 'KobIfdBidKron',
    test: T_RPT,
    expect: ['NRP29'],
    note: 'merge into an entry armed by update (armed 1, band): the continuation records the band origin (else the auction restarts)',
    edits: [rep('if (armed == 1 && bandDaa > 0) {', 'if (armed == 1 && bandDaa < 0) {')],
  },

  // ---- protocol v3: KobAskKron
  {
    id: 'V3-A-minfill',
    file: 'KobAskKron',
    test: T_MIN,
    expect: ['V3A01'],
    note: 'n >= minFill unless the fill takes everything left',
    edits: [del('            require(n >= minFill || n == amountLeft);\n')],
  },
  {
    id: 'V3-A-ceil',
    file: 'KobAskKron',
    test: T_RND,
    expect: ['V3A02'],
    note: 'proceeds rounded up (c = scale - 1 -> 0)',
    edits: [rep('int allIn = quoteOf(n, p - tip, scale - 1);', 'int allIn = quoteOf(n, p - tip, 0);')],
  },
  {
    id: 'V3-A-tip',
    file: 'KobAskKron',
    test: T_TIP,
    expect: ['V3A03', 'V3A04'],
    note: 'p >= tip (also a decayed price): a negative proceeds rate would pay the maker nothing',
    edits: [del('            require(p >= tip);\n')],
  },
  // ---- protocol v3: KobBidKron
  {
    id: 'V3-B-minfill',
    file: 'KobBidKron',
    test: T_MIN,
    expect: ['V3B01'],
    note: 'n >= minFill unless the bid ends (less than one minimum fill of buying power left)',
    edits: [del('        require(n >= minFill || !canContinue);\n')],
  },
  {
    id: 'V3-B-minfill0',
    file: 'KobBidKron',
    test: T_MIN,
    expect: ['V3B02'],
    note: 'minFill > 0',
    edits: [del('        require(minFill > 0);\n')],
  },
  {
    id: 'V3-B-cont',
    file: 'KobBidKron',
    test: T_MIN,
    expect: ['V3B04'],
    note: 'canContinue compares with the ceil budget of one minimum fill (c = scale - 1 -> 0)',
    edits: [rep('quoteOf(minFill, pMax + tip, scale - 1)', 'quoteOf(minFill, pMax + tip, 0)')],
  },
  {
    id: 'V3-B-floor',
    file: 'KobBidKron',
    test: T_RND,
    expect: ['V3B05'],
    note: 'the maker pays the floor (allIn c = 0 -> scale - 1)',
    edits: [rep('int allIn = quoteOf(n, p + tip, 0);', 'int allIn = quoteOf(n, p + tip, scale - 1);')],
  },
  {
    id: 'V3-B-used',
    file: 'KobBidKron',
    test: T_RND,
    expect: ['V3B06'],
    note: 'the escrow is consumed at the ceil budget (used c = scale - 1 -> 0)',
    edits: [rep('int used = quoteOf(n, pMax + tip, scale - 1);', 'int used = quoteOf(n, pMax + tip, 0);')],
  },
  // ---- protocol v3: KobCondAskKron
  {
    id: 'V3-C-minfill',
    file: 'KobCondAskKron',
    test: T_MIN,
    expect: ['V3CA01'],
    note: 'n >= minFill unless the fill takes everything left',
    edits: [del('            require(n >= minFill || n == amountLeft);\n')],
  },
  {
    id: 'V3-C-ceil',
    file: 'KobCondAskKron',
    test: T_RND,
    expect: ['V3CA02'],
    note: 'proceeds rounded up (c = scale - 1 -> 0)',
    edits: [rep('int allIn = quoteOf(n, legPrice - tip, scale - 1);', 'int allIn = quoteOf(n, legPrice - tip, 0);')],
  },
  {
    id: 'V3-C-rpt',
    file: 'KobCondAskKron',
    test: T_RND,
    expect: ['V3CA03'],
    note: 're-arm budget returned to the entry on a sell-out rounded up (rptBudget c = scale - 1 -> 0; the entry\'s own floor, in + budget, does not count the exit carriers the continuation also gets, so the exit\'s check is the binding one)',
    edits: [rep('rptBudget = quoteOf(n, rptPrice, scale - 1);', 'rptBudget = quoteOf(n, rptPrice, 0);')],
  },
  {
    id: 'V3-C-tip',
    file: 'KobCondAskKron',
    test: T_TIP,
    expect: ['V3CA04', 'V3CA04b'],
    note: 'legPrice >= tip (either leg)',
    edits: [del('            require(legPrice >= tip);\n')],
  },
  {
    id: 'V3-C-push',
    file: 'KobCondAskKron',
    test: T_PUSH,
    expect: ['V3CA06'],
    inputOnly: ['V3CA06'],
    hold: { [T_RPT]: ['NRP28'] },
    note: 'the entry\'s sigscript starts with 0x08 (its merge argument is an 8-byte push; a fill of n >= 2^55 pushed by OP_PUSHDATA1 would alias a merge value; the KRON token refuses such amounts, so the exit input alone proves it)',
    edits: [del('                require(OpTxInputScriptSigSubstr(pin, 0, 1) == byte[](0x08));\n')],
  },
  {
    id: 'V3-C-parent',
    file: 'KobCondAskKron',
    run: { [T_PUSH]: ['V3CA05'] },
    hold: { [T_RPT]: ['NRP22'] },
    note: 'update never next to the repeat entry (OpCovInputCount(parent) == 0): an 8-byte ev push would alias the merge amount',
    edits: [rxDel('    entry update(int ev, int tk) {', /^\s*require\(OpCovInputCount\(parent\) == 0\);$/m)],
  },
  {
    id: 'V3-C-mergek',
    file: 'KobCondAskKron',
    test: T_PUSH,
    expect: ['V3CA09'],
    inputOnly: ['V3CA09'],
    note: "re-arm: the exit sells n < 2^53 (the merge argument -(self * 2^53 + n) names this input and n only then: an exit of 2^53 + m at input i reads the merge of the booked exit at input i + 1 as its own; the KRON token refuses custodies above 10^9, so the exit input alone proves it)",
    edits: [del('                require(n < MERGE_K);\n')],
  },
  // ---- protocol v3: KobIfdBidKron
  {
    id: 'V3-I-floor',
    file: 'KobIfdBidKron',
    test: T_RND,
    expect: ['V3IB02'],
    note: 'the maker pays the floor (spend c = 0 -> scale - 1)',
    edits: [rep('int spend = quoteOf(n, p + tip, 0);', 'int spend = quoteOf(n, p + tip, scale - 1);')],
  },
  {
    id: 'V3-I-mergeceil',
    file: 'KobIfdBidKron',
    test: T_RND,
    expect: ['V3IB03'],
    note: 'merge floor: the budget of m comes back rounded up (c = scale - 1 -> 0)',
    edits: [rep('floor = tx.inputs[self].value + quoteOf(m, rate, scale - 1);', 'floor = tx.inputs[self].value + quoteOf(m, rate, 0);')],
  },
  {
    id: 'V3-I-push',
    file: 'KobIfdBidKron',
    test: T_PUSH,
    expect: ['V3IB04'],
    hold: { [T_RPT]: ['NRP22'] },
    note: 'the exit\'s sigscript starts with 0x08 (its settle n is an 8-byte push; a refund pushed by OP_PUSHDATA1 would read as m = 8)',
    edits: [del('            require(OpTxInputScriptSigSubstr(k, 0, 1) == byte[](0x08));\n')],
  },
  {
    id: 'V3-I-mergek',
    file: 'KobIfdBidKron',
    test: T_LIM,
    expect: ['V3IB06'],
    inputOnly: ['V3IB06'],
    note: 'a booked exit\'s amount is below MERGE_K (2^53: it must fit the merge argument; the KRON token refuses amounts above 10^9, so the entry input alone proves it)',
    edits: [del('                require(n < MERGE_K);\n')],
  },

  // ================================================================ audit completion (2026-10-13)
  // AB-BEGIN
  { id: "AU-KA-1", file: "KobAskKron", run: { [T_AU_KA]: ["NAU-KA03"] }, note: "tplRs: the custody input is the P2SH of the holder script it pushes (a planted UTXO of the token covenant id)", edits: [del("        require(tx.inputs[idx].scriptPubKey == byte[](new ScriptPubKeyP2SHFromRedeemScript(rs)));\n")] },
  { id: "AU-KA-2", file: "KobAskKron", run: { [T_AU_KA]: ["NAU-KA04"] }, note: "tplRs: the custody's template is the token's (a planted P2SH of a look-alike template)", edits: [del("        require(blake3(byte[](pre as byte[8]) + rs.slice(0, pre) + byte[](suf as byte[8]) + rs.slice(pre + stLen, size)) == tpl);\n")] },
  { id: "AU-KA-3", file: "KobAskKron", run: { [T_AU_KA]: ["NAU-KA22"] }, inputOnly: ["NAU-KA22"], note: "noStrays bound: at most MAX_TOK_IN = 4 token inputs (a 5th is not scanned; the KRON program refuses a 5th input on its own, so only the ask flips)", edits: [del("        require(cnt <= MAX_TOK_IN);\n")] },
  { id: "AU-KA-S0", file: "KobAskKron", run: { [T_AU_KA]: ["NAU-KAS0"] }, note: "noStrays scan slot 0: a stray of the ask at token slot 0 rides to the taker", edits: [rep("require(byte[32](OpTxInputScriptSigSubstr(x0, e0, e0 + 32)) != me);", 'require(true);')] },
  { id: "AU-KA-S1", file: "KobAskKron", run: { [T_AU_KA]: ["NAU-KAS1"] }, note: "noStrays scan slot 1: a stray of the ask at token slot 1 rides to the taker", edits: [rep("require(byte[32](OpTxInputScriptSigSubstr(x1, e1, e1 + 32)) != me);", 'require(true);')] },
  { id: "AU-KA-S2", file: "KobAskKron", run: { [T_AU_KA]: ["NAU-KAS2"] }, note: "noStrays scan slot 2: a stray of the ask at token slot 2 rides to the taker", edits: [rep("require(byte[32](OpTxInputScriptSigSubstr(x2, e2, e2 + 32)) != me);", 'require(true);')] },
  { id: "AU-KA-S3", file: "KobAskKron", run: { [T_AU_KA]: ["NAU-KAS3"] }, note: "noStrays scan slot 3: a stray of the ask at token slot 3 rides to the taker", edits: [rep("require(byte[32](OpTxInputScriptSigSubstr(x3, e3, e3 + 32)) != me);", 'require(true);')] },
  { id: "AU-KA-4", file: "KobAskKron", run: { [T_AU_KA]: ["NAU-KA01"] }, note: "the ask is the only input of its covenant id (a sibling UTXO of the id beside the fill)", edits: [del("        require(OpCovInputCount(selfId) == 1);\n")] },
  { id: "AU-KA-5", file: "KobAskKron", run: { [T_AU_KA]: ["NAU-KA02"] }, note: "the custody carries the token covenant id (not a UTXO of another token of the same program owned by the ask)", edits: [del("        require(OpInputCovenantId(tokenIn) == tokenCovId);\n")] },
  { id: "AU-KA-6", file: "KobAskKron", run: { [T_AU_KA]: ["NAU-KA05"] }, note: "the custody is owned by this ask's covenant id (not tokens of another id: the ask's real custody would be orphaned)", edits: [del("        require(byte[32](tin.slice(1, 33)) == selfId);\n")] },
  { id: "AU-KA-7", file: "KobAskKron", run: { [T_KAS]: ["KA4b"] }, inputOnly: ["KA4b"], note: "the custody has id_type 2 (covenant id): a custody of the order id with id_type 3 is also refused by the token program (no input is P2PK of the id), so only the ask flips", edits: [del("        require(tin.slice(33, 36) == byte[](0x010208));\n")] },
  { id: "AU-KA-8", file: "KobAskKron", run: { [T_KAS]: ["KA4c"] }, note: "the custody is no minter (is_minter 0)", edits: [del("        require(tin.slice(44, 46) == byte[](0x0100));\n")] },
  { id: "AU-KA-9", file: "KobAskKron", run: { [T_AUC]: ["NC8","NC9","NC8b"], [T_TCE]: ["NC3","NC4"] }, note: "refund only once due: min(expiryDaa, 90 days idle, IOC / FOK kill)", edits: [del("            require(tx.daa >= idleEnd);\n")] },
  { id: "AU-KA-10", file: "KobAskKron", run: { [T_AU_KA]: ["NAU-KA08"] }, note: "a refund leaves no output bound to the ask's covenant id (a keeper's UTXO of the id would own every later stray)", edits: [rep("            require(refundTip >= 0);\n            require(OpCovOutputCount(selfId) == 0);\n", "            require(refundTip >= 0);\n")] },
  { id: "AU-KA-11", file: "KobAskKron", run: { [T_TCE]: ["NC6"] }, note: "refund: the maker gets the carriers minus at most refundTip", edits: [del("            require(tx.outputs[self].value + refundTip >= carriers);\n")] },
  { id: "AU-KA-12", file: "KobAskKron", run: { [T_TCE]: ["NT-ACT"] }, note: "no fill before activeFrom", edits: [del("            require(tx.daa >= activeFrom);\n")] },
  { id: "AU-KA-13", file: "KobAskKron", hold: { [T_AU_KA]: ["NAU-KA10"] }, note: "fill n > 0 (NAU-KA10, n = -1, is also refused by n >= minFill || n == amountLeft; AU-KA-13x removes both)", edits: [del("            require(n > 0);\n")] },
  { id: "AU-KA-13x", file: "KobAskKron", run: { [T_AU_KA]: ["NAU-KA10"] }, note: "n > 0 and the minFill rule removed together: a fill of n = -1 (the taker adds a unit, the maker is paid nothing)", edits: [del("            require(n > 0);\n"), del("            require(n >= minFill || n == amountLeft);\n")] },
  { id: "AU-KA-14", file: "KobAskKron", run: { [T_AU_KA]: ["NAU-KA11"] }, note: "scale > 0 (hostile field: an ask at a negative scale sells for nothing)", edits: [del("            require(scale > 0);\n")] },
  { id: "AU-KA-15", file: "KobAskKron", run: { [T_NLT]: ["NF7"] }, note: "tip >= 0 (hostile field)", edits: [del("            require(tip >= 0);\n")] },
  { id: "AU-KA-16", file: "KobAskKron", run: { [T_AU_KA]: ["NAU-KA09"] }, note: "n <= amountLeft: a fill cannot claim more than the ask holds (an evidence fill of 1 base unit claiming minTouch would arm a stop)", edits: [del("            require(n <= amountLeft);\n")] },
  { id: "AU-KA-17", file: "KobAskKron", run: { [T_NTV]: ["NV1"] }, note: "TWAP: the UTXO is >= interval DAA old (CSV)", edits: [del("                require(this.ageDaa >= interval);\n")] },
  { id: "AU-KA-18", file: "KobAskKron", run: { [T_NTV]: ["NV2"] }, note: "TWAP: n <= maxFill", edits: [del("            require(maxFill == 0 || n <= maxFill);\n")] },
  { id: "AU-KA-19", file: "KobAskKron", run: { [T_NTV]: ["NV7"] }, note: "decay: slope > 0 (a rising ask would reward understated time)", edits: [del("                require(slope > 0);\n")] },
  { id: "AU-KA-20", file: "KobAskKron", run: { [T_NLT]: ["NF9"] }, note: "positional payout: output i pays P2PK(maker)", edits: [del("            require(tx.outputs[self].scriptPubKey == byte[](new ScriptPubKeyP2PK(maker)));\n")] },
  { id: "AU-KA-21", file: "KobAskKron", run: { [T_AU_KA]: ["NAU-KA14"] }, note: "GTC rest: exactly one output bound to the ask's id (no second UTXO of the id for the taker)", edits: [del("                require(OpCovOutputCount(selfId) == 1);\n")] },
  { id: "AU-KA-22", file: "KobAskKron", run: { [T_NLT]: ["NF10"] }, note: "GTC rest: the continuation keeps the order's carrier", edits: [del("                require(tx.outputs[selfOut].value >= tx.inputs[self].value);\n")] },
  { id: "AU-KA-23", file: "KobAskKron", run: { [T_KAS]: ["KA7"] }, note: "GTC rest: the rest custody keeps its carrier", edits: [rep("                require(tx.outputs[selfOut].value >= tx.inputs[self].value);\n                require(tx.outputs[tokOut].value >= tx.inputs[tokenIn].value);\n", "                require(tx.outputs[selfOut].value >= tx.inputs[self].value);\n")] },
  { id: "AU-KA-24", file: "KobAskKron", run: { [T_NLT]: ["NF1","NF2"], [T_NTV]: ["NV6"], [T_RND]: ["V3A02"] }, note: "partial fill: the maker gets at least the all-in proceeds", edits: [del("                require(tx.outputs[self].value >= allIn);\n")] },
  { id: "AU-KA-25", file: "KobAskKron", run: { [T_AU_KA]: ["NAU-KA17"] }, note: "FOK: a fill that leaves tokens unsold is refused", edits: [del("                require(tif != TIF_FOK || outAmount == 0);\n")] },
  { id: "AU-KA-26", file: "KobAskKron", run: { [T_AU_KA]: ["NAU-KA18"] }, note: "termination (IOC, sold out): no output bound to the ask's id", edits: [del("                require(OpCovOutputCount(selfId) == 0);\n")] },
  { id: "AU-KA-27", file: "KobAskKron", run: { [T_AU_KA]: ["NAU-KA19"] }, note: "IOC return: the returned rest keeps the custody's carrier", edits: [del("                    require(tx.outputs[tokOut].value >= tx.inputs[tokenIn].value);\n")] },
  { id: "AU-KA-28", file: "KobAskKron", run: { [T_AUC]: ["NM1"] }, note: "IOC: the maker gets the all-in proceeds plus the order carrier", edits: [del("                    require(tx.outputs[self].value >= allIn + tx.inputs[self].value);\n")] },
  { id: "AU-KA-29", file: "KobAskKron", run: { [T_AU_KA]: ["NAU-KA20"] }, note: "sold out: the maker gets the all-in proceeds plus both carriers", edits: [del("                    require(tx.outputs[self].value >= allIn + carriers);\n")] },
  { id: "AU-KA-30", file: "KobAskKron", run: { [T_TCE]: ["NT-IOC2"], [T_KAS]: ["KA1","KA2","KA2b","KA13","KA13b","KA13c"] }, note: "the one token output (rest, IOC return, refund) is pinned: the custody template with the expected state (owner, id_type; the minter byte is also refused by the token program)", edits: [del("            require(tx.outputs[outIdx].scriptPubKey == byte[](new ScriptPubKeyP2SHFromRedeemScript(\n                trs.slice(0, tplPrefixLen) + byte[](0x20) + byte[](outOwner) + byte[](0x01) + byte[](outType) + byte[](0x08)\n                    + byte[](outAmount as byte[8]) + byte[](0x0100) + trs.slice(tplPrefixLen + 46, trs.length)\n            )));\n")] },
  { id: "AU-KA-31", file: "KobAskKron", run: { [T_AU_KA]: ["NAU-KA21"] }, note: "the pinned token output is bound to the token covenant (an unbound copy of the holder script would let the taker take the rest)", edits: [del("            require(OpOutputCovenantId(outIdx) == tokenCovId);\n")] },
  { id: "AU-KA-32", file: "KobAskKron", run: { [T_TCE]: ["NC1"] }, note: "cancel: SIGHASH_ALL only", edits: [del("        require(sb[64] == SIGHASH_ALL);\n")] },
  { id: "AU-KA-33", file: "KobAskKron", run: { [T_TCE]: ["NC2"] }, note: "cancel: the maker's signature", edits: [del("        require(checkSig(s, maker));\n")] },
  { id: "AU-KB-1", file: "KobBidKron", run: { [T_AU_KB]: ["NAU-KB26"] }, inputOnly: ["NAU-KB26"], note: "noStrays bound: at most MAX_TOK_IN = 4 token inputs (a 5th is not scanned; the KRON program refuses a 5th input on its own, so only the bid flips)", edits: [del("        require(cnt <= MAX_TOK_IN);\n")] },
  { id: "AU-KB-S0", file: "KobBidKron", run: { [T_AU_KB]: ["NAU-KBS0"] }, note: "noStrays scan slot 0: a stray of the bid at token slot 0 rides to the taker", edits: [rep("require(byte[32](OpTxInputScriptSigSubstr(x0, e0, e0 + 32)) != me);", 'require(true);')] },
  { id: "AU-KB-S1", file: "KobBidKron", run: { [T_AU_KB]: ["NAU-KBS1"] }, note: "noStrays scan slot 1: a stray of the bid at token slot 1 rides to the taker", edits: [rep("require(byte[32](OpTxInputScriptSigSubstr(x1, e1, e1 + 32)) != me);", 'require(true);')] },
  { id: "AU-KB-S2", file: "KobBidKron", run: { [T_AU_KB]: ["NAU-KBS2"] }, note: "noStrays scan slot 2: a stray of the bid at token slot 2 rides to the taker", edits: [rep("require(byte[32](OpTxInputScriptSigSubstr(x2, e2, e2 + 32)) != me);", 'require(true);')] },
  { id: "AU-KB-S3", file: "KobBidKron", run: { [T_AU_KB]: ["NAU-KBS3"] }, note: "noStrays scan slot 3: a stray of the bid at token slot 3 rides to the taker", edits: [rep("require(byte[32](OpTxInputScriptSigSubstr(x3, e3, e3 + 32)) != me);", 'require(true);')] },
  { id: "AU-KB-2", file: "KobBidKron", run: { [T_AU_KB]: ["NAU-KB01"] }, note: "the bid is the only input of its covenant id (a sibling UTXO of the id beside the fill)", edits: [rep("        int self = this.activeInputIndex;\n        byte[32] selfId = OpInputCovenantId(self);\n        require(OpCovInputCount(selfId) == 1);\n", "        int self = this.activeInputIndex;\n        byte[32] selfId = OpInputCovenantId(self);\n")] },
  { id: "AU-KB-3", file: "KobBidKron", run: { [T_AU_KB]: ["NAU-KB02"] }, note: "no fill before activeFrom", edits: [del("        require(tx.daa >= activeFrom);\n")] },
  { id: "AU-KB-4", file: "KobBidKron", run: { [T_AU_KB]: ["NAU-KB03"] }, inputOnly: ["NAU-KB03"], note: "fill n > 0 (a zero fill would close an exhausted bid before its expiry; the KRON program also refuses the empty delivery on its own input, so only the bid flips)", edits: [del("        require(n > 0);\n")] },
  { id: "AU-KB-5", file: "KobBidKron", run: { [T_AU_KB]: ["NAU-KB04"] }, note: "scale > 0 (hostile field: a negative scale turns the quote negative)", edits: [del("        require(scale > 0);\n")] },
  { id: "AU-KB-6", file: "KobBidKron", run: { [T_AU_KB]: ["NAU-KB05"] }, note: "tip >= 0 (hostile field: the bid would pay below its quoted price)", edits: [del("        require(tip >= 0);\n")] },
  { id: "AU-KB-7", file: "KobBidKron", run: { [T_AU_KB]: ["NAU-KB06"] }, note: "reserve >= 0 (hostile field: a negative reserve lets an underfunded bid fill)", edits: [del("        require(reserve >= 0);\n")] },
  { id: "AU-KB-8", file: "KobBidKron", run: { [T_AU_KB]: ["NAU-KB07"] }, note: "deliveryCarrier >= 0 (hostile field)", edits: [del("        require(deliveryCarrier >= 0);\n")] },
  { id: "AU-KB-9", file: "KobBidKron", run: { [T_NTV]: ["NV3"] }, note: "DCA: the UTXO is >= interval DAA old (CSV)", edits: [del("            require(this.ageDaa >= interval);\n")] },
  { id: "AU-KB-10", file: "KobBidKron", run: { [T_NTV]: ["NV4"] }, note: "DCA: n <= maxFill", edits: [del("        require(maxFill == 0 || n <= maxFill);\n")] },
  { id: "AU-KB-11", file: "KobBidKron", run: { [T_AU_KB]: ["NAU-KB08"] }, note: "rising: slope > 0 (a falling bid would reward understated time)", edits: [del("            require(slope > 0);\n")] },
  { id: "AU-KB-12", file: "KobBidKron", run: { [T_NTV]: ["NV9"] }, note: "rising: tx.daa >= t (CLTV proves the time)", edits: [del("            require(tx.daa >= t);\n")] },
  { id: "AU-KB-13", file: "KobBidKron", run: { [T_AU_KB]: ["NAU-KB10"] }, note: "rising: t >= origin (a time before the origin is refused; it could only keep or lower the price)", edits: [del("            require(t >= origin);\n")] },
  { id: "AU-KB-14", file: "KobBidKron", run: { [T_AU_KB]: ["NAU-KB11"] }, note: "a fill never spends the reserve: left >= reserve", edits: [del("        require(left >= reserve);\n")] },
  { id: "AU-KB-15", file: "KobBidKron", run: { [T_AU_KB]: ["NAU-KB12"] }, note: "the delivery template is read from a token input of this token (the bytes are also hash-pinned, so a same-program source of another covenant gains nothing; the check pins the source)", edits: [del("        require(OpInputCovenantId(tokenTplIn) == tokenCovId);\n")] },
  { id: "AU-KB-16", file: "KobBidKron", run: { [T_AU_KB]: ["NAU-KB13"] }, note: "the delivery template's hash is the token's (a planted look-alike would give the maker an anyone-can-spend fake)", edits: [del("        require(blake3(byte[](tplPrefixLen as byte[8]) + tpre + byte[](tplSuffixLen as byte[8]) + tsuf) == tokenTplHash);\n")] },
  { id: "AU-KB-17", file: "KobBidKron", run: { [T_KBS]: ["KB1","KB2","KB2b"] }, note: "positional delivery: output i is the holder of exactly n base units owned by the maker with id_type 3 (the minter byte is also refused by the token program)", edits: [del("        require(tx.outputs[self].scriptPubKey == byte[](new ScriptPubKeyP2SHFromRedeemScript(\n            tpre + byte[](0x20) + byte[](maker) + byte[](0x01) + byte[](deliveryType) + byte[](0x08)\n                + byte[](nb) + byte[](0x0100) + tsuf\n        )));\n")] },
  { id: "AU-KB-18", file: "KobBidKron", run: { [T_AU_KB]: ["NAU-KB15"] }, note: "the delivery is bound to the token covenant (an unbound copy of the holder script delivers nothing)", edits: [del("        require(OpOutputCovenantId(self) == tokenCovId);\n")] },
  { id: "AU-KB-19", file: "KobBidKron", run: { [T_AU_KB]: ["NAU-KB16"] }, note: "GTC continuation: exactly one output bound to the bid's id", edits: [del("            require(OpCovOutputCount(selfId) == 1);\n")] },
  { id: "AU-KB-20", file: "KobBidKron", run: { [T_AU_KB]: ["NAU-KB17"] }, note: "GTC continuation: this script (not a bid of the taker)", edits: [del("            require(tx.outputs[selfOut].scriptPubKey == this.activeScriptPubKey);\n")] },
  { id: "AU-KB-21", file: "KobBidKron", run: { [T_NLT]: ["NF5","NF6"] }, note: "GTC continuation: exactly the unused escrow", edits: [del("            require(tx.outputs[selfOut].value == left - deliveryCarrier);\n")] },
  { id: "AU-KB-22", file: "KobBidKron", run: { [T_NLT]: ["NF4"], [T_RND]: ["V3B05"] }, note: "GTC: the delivery carries the carrier plus the budget not spent", edits: [del("            require(tx.outputs[self].value >= deliveryCarrier + used - allIn);\n")] },
  { id: "AU-KB-23", file: "KobBidKron", run: { [T_TCE]: ["NT-FOK2"] }, note: "FOK: a fill that leaves buying power is refused", edits: [del("            require(tif != TIF_FOK || !canContinue);\n")] },
  { id: "AU-KB-24", file: "KobBidKron", run: { [T_AU_KB]: ["NAU-KB18"] }, note: "termination: no output bound to the bid's id", edits: [del("            require(OpCovOutputCount(selfId) == 0);\n")] },
  { id: "AU-KB-25", file: "KobBidKron", run: { [T_AUC]: ["NM4"], [T_NLT]: ["NF3"], [T_NTV]: ["NV8"] }, note: "termination: everything not spent rides on the delivery", edits: [del("            require(tx.outputs[self].value >= inValue - allIn);\n")] },
  { id: "AU-KB-26", file: "KobBidKron", run: { [T_AU_KB]: ["NAU-KB24"] }, note: "cancel: SIGHASH_ALL only", edits: [del("        require(sb[64] == SIGHASH_ALL);\n")] },
  { id: "AU-KB-27", file: "KobBidKron", run: { [T_AU_KB]: ["NAU-KB25"] }, note: "cancel: the maker's signature", edits: [del("        require(checkSig(s, maker));\n")] },
  { id: "AU-KB-28", file: "KobBidKron", run: { [T_AUC]: ["NC10","NC10b"], [T_TCE]: ["NC3b","NC5"] }, note: "refund only once due: min(expiryDaa, 90 days idle, IOC / FOK kill)", edits: [del("        require(tx.daa >= idleEnd);\n")] },
  { id: "AU-KB-29", file: "KobBidKron", run: { [T_AU_KB]: ["NAU-KB19"] }, note: "refund: the bid is the only input of its covenant id", edits: [rep("        require(tx.daa >= idleEnd);\n        byte[32] selfId = OpInputCovenantId(self);\n        require(OpCovInputCount(selfId) == 1);\n", "        require(tx.daa >= idleEnd);\n        byte[32] selfId = OpInputCovenantId(self);\n")] },
  { id: "AU-KB-30", file: "KobBidKron", run: { [T_AU_KB]: ["NAU-KB21"] }, note: "refund: no output bound to the bid's id", edits: [rep("        require(OpCovInputCount(tokenCovId) == 0);  // strays owned by this id stay put\n        require(OpCovOutputCount(selfId) == 0);\n", "        require(OpCovInputCount(tokenCovId) == 0);  // strays owned by this id stay put\n")] },
  { id: "AU-KB-31", file: "KobBidKron", run: { [T_TCE]: ["NC7"], [T_AU_KB]: ["NAU-KB22"] }, note: "refund: output i pays P2PK(maker)", edits: [del("        require(tx.outputs[self].scriptPubKey == byte[](new ScriptPubKeyP2PK(maker)));\n")] },
  { id: "AU-KB-32", file: "KobBidKron", run: { [T_AU_KB]: ["NAU-KB23"] }, note: "refund: the maker gets the escrow minus at most refundTip", edits: [del("        require(tx.outputs[self].value + refundTip >= tx.inputs[self].value);\n")] },
  // AB-END
  // CI-BEGIN
  { id: "AU-KCA-1", file: "KobCondAskKron", run: { [T_AU_KCA]: ["NAU-KCA03"] }, note: "tplRs: the custody input is the P2SH of the holder script it pushes (a planted UTXO of the token covenant id)", edits: [rep("    function tplRs(int idx, int pre, int stLen, int suf, byte[32] tpl) : byte[] {\n        int size = pre + stLen + suf;\n        int base = OpTxInputScriptSigLen(idx) - size;\n        byte[] rs = OpTxInputScriptSigSubstr(idx, base, base + size);\n        require(tx.inputs[idx].scriptPubKey == byte[](new ScriptPubKeyP2SHFromRedeemScript(rs)));\n", "    function tplRs(int idx, int pre, int stLen, int suf, byte[32] tpl) : byte[] {\n        int size = pre + stLen + suf;\n        int base = OpTxInputScriptSigLen(idx) - size;\n        byte[] rs = OpTxInputScriptSigSubstr(idx, base, base + size);\n")] },
  { id: "AU-KCA-2", file: "KobCondAskKron", run: { [T_AU_KCA]: ["NAU-KCA04"] }, note: "tplRs: the custody's template is the token's (a planted P2SH of a look-alike template)", edits: [rep("    function tplRs(int idx, int pre, int stLen, int suf, byte[32] tpl) : byte[] {\n        int size = pre + stLen + suf;\n        int base = OpTxInputScriptSigLen(idx) - size;\n        byte[] rs = OpTxInputScriptSigSubstr(idx, base, base + size);\n        require(tx.inputs[idx].scriptPubKey == byte[](new ScriptPubKeyP2SHFromRedeemScript(rs)));\n        require(blake3(byte[](pre as byte[8]) + rs.slice(0, pre) + byte[](suf as byte[8]) + rs.slice(pre + stLen, size)) == tpl);\n", "    function tplRs(int idx, int pre, int stLen, int suf, byte[32] tpl) : byte[] {\n        int size = pre + stLen + suf;\n        int base = OpTxInputScriptSigLen(idx) - size;\n        byte[] rs = OpTxInputScriptSigSubstr(idx, base, base + size);\n        require(tx.inputs[idx].scriptPubKey == byte[](new ScriptPubKeyP2SHFromRedeemScript(rs)));\n")] },
  { id: "AU-KCA-3", file: "KobCondAskKron", run: { [T_SET]: ["NTA18"], [T_ARM]: ["NTU18"], [T_TRL]: ["NTT18"] }, note: "tplState (evidence and its custody): the input is the P2SH of the redeem script it pushes (the real evidence script pushed by a non-P2SH input)", edits: [rep("    function tplState(int idx, int pre, int stLen, int suf, byte[32] tpl) : byte[] {\n        int size = pre + stLen + suf;\n        int base = OpTxInputScriptSigLen(idx) - size;\n        byte[] rs = OpTxInputScriptSigSubstr(idx, base, base + size);\n        require(tx.inputs[idx].scriptPubKey == byte[](new ScriptPubKeyP2SHFromRedeemScript(rs)));\n", "    function tplState(int idx, int pre, int stLen, int suf, byte[32] tpl) : byte[] {\n        int size = pre + stLen + suf;\n        int base = OpTxInputScriptSigLen(idx) - size;\n        byte[] rs = OpTxInputScriptSigSubstr(idx, base, base + size);\n")] },
  { id: "AU-KCA-4", file: "KobCondAskKron", run: { [T_TRIG]: ["NT6"], [T_SET]: ["NTA17"], [T_ARM]: ["NTU17"], [T_TRL]: ["NTT17"] }, note: "tplState (evidence and its custody): the template hash (a look-alike evidence order's state under another template)", edits: [rep("    function tplState(int idx, int pre, int stLen, int suf, byte[32] tpl) : byte[] {\n        int size = pre + stLen + suf;\n        int base = OpTxInputScriptSigLen(idx) - size;\n        byte[] rs = OpTxInputScriptSigSubstr(idx, base, base + size);\n        require(tx.inputs[idx].scriptPubKey == byte[](new ScriptPubKeyP2SHFromRedeemScript(rs)));\n        require(blake3(byte[](pre as byte[8]) + rs.slice(0, pre) + byte[](suf as byte[8]) + rs.slice(pre + stLen, size)) == tpl);\n", "    function tplState(int idx, int pre, int stLen, int suf, byte[32] tpl) : byte[] {\n        int size = pre + stLen + suf;\n        int base = OpTxInputScriptSigLen(idx) - size;\n        byte[] rs = OpTxInputScriptSigSubstr(idx, base, base + size);\n        require(tx.inputs[idx].scriptPubKey == byte[](new ScriptPubKeyP2SHFromRedeemScript(rs)));\n")] },
  { id: "AU-KCA-5", file: "KobCondAskKron", run: { [T_AU_KCA]: ["NAU-KCA39"] }, inputOnly: ["NAU-KCA39"], note: "noStrays bound: at most MAX_TOK_IN = 4 token inputs (a 5th is not scanned; the KRON program refuses a 5th input on its own, so only the order flips)", edits: [del("        require(cnt <= MAX_TOK_IN);\n")] },
  { id: "AU-KCA-S0", file: "KobCondAskKron", run: { [T_AU_KCA]: ["NAU-KCAS0"] }, note: "noStrays scan slot 0: a stray of the order at token slot 0 rides to the taker", edits: [rep("require(byte[32](OpTxInputScriptSigSubstr(x0, e0, e0 + 32)) != me);", 'require(true);')] },
  { id: "AU-KCA-S1", file: "KobCondAskKron", run: { [T_AU_KCA]: ["NAU-KCAS1"] }, note: "noStrays scan slot 1: a stray of the order at token slot 1 rides to the taker", edits: [rep("require(byte[32](OpTxInputScriptSigSubstr(x1, e1, e1 + 32)) != me);", 'require(true);')] },
  { id: "AU-KCA-S2", file: "KobCondAskKron", run: { [T_AU_KCA]: ["NAU-KCAS2"] }, note: "noStrays scan slot 2: a stray of the order at token slot 2 rides to the taker", edits: [rep("require(byte[32](OpTxInputScriptSigSubstr(x2, e2, e2 + 32)) != me);", 'require(true);')] },
  { id: "AU-KCA-S3", file: "KobCondAskKron", run: { [T_AU_KCA]: ["NAU-KCAS3"] }, note: "noStrays scan slot 3: a stray of the order at token slot 3 rides to the taker", edits: [rep("require(byte[32](OpTxInputScriptSigSubstr(x3, e3, e3 + 32)) != me);", 'require(true);')] },
  { id: "AU-KCA-6", file: "KobCondAskKron", run: { [T_AU_KCA]: ["NAU-KCA01"] }, note: "settle: the order is the only input of its covenant id (a sibling UTXO of the id beside the fill)", edits: [rep("        int n = int(nb);\n        int self = this.activeInputIndex;\n        byte[32] selfId = OpInputCovenantId(self);\n        require(OpCovInputCount(selfId) == 1);\n", "        int n = int(nb);\n        int self = this.activeInputIndex;\n        byte[32] selfId = OpInputCovenantId(self);\n")] },
  { id: "AU-KCA-7", file: "KobCondAskKron", run: { [T_AU_KCA]: ["NAU-KCA02"] }, note: "the custody carries the token covenant id (not a UTXO of another token of the same program owned by the order)", edits: [del("        require(OpInputCovenantId(tokenIn) == tokenCovId);\n")] },
  { id: "AU-KCA-8", file: "KobCondAskKron", run: { [T_AU_KCA]: ["NAU-KCA05"] }, note: "the custody is owned by this order's covenant id", edits: [del("        require(byte[32](tin.slice(1, 33)) == selfId);\n")] },
  { id: "AU-KCA-9", file: "KobCondAskKron", run: { [T_AU_KCA]: ["NAU-KCA06"] }, inputOnly: ["NAU-KCA06"], note: "the custody has id_type 2 (covenant id): a custody of the order id with id_type 3 is also refused by the token program (no input is P2PK of the id), so only the order flips", edits: [del("        require(tin.slice(33, 36) == byte[](0x010208));\n")] },
  { id: "AU-KCA-10", file: "KobCondAskKron", run: { [T_KCI]: ["KC4"] }, note: "the custody is no minter (is_minter 0)", edits: [del("        require(tin.slice(44, 46) == byte[](0x0100));\n")] },
  { id: "AU-KCA-11", file: "KobCondAskKron", run: { [T_AU_KCA]: ["NAU-KCA08"] }, note: "refund only from min(expiryDaa, 90 days idle)", edits: [del("            require(tx.daa >= idleEnd);\n")] },
  { id: "AU-KCA-12", file: "KobCondAskKron", run: { [T_AU_KCA]: ["NAU-KCA09"] }, note: "a refund leaves no output bound to the order's id", edits: [rep("            require(refundTip >= 0);\n            require(OpCovOutputCount(selfId) == 0);\n", "            require(refundTip >= 0);\n")] },
  { id: "AU-KCA-13", file: "KobCondAskKron", run: { [T_AU_KCA]: ["NAU-KCA10"] }, note: "refund: the maker gets the carriers minus at most refundTip", edits: [del("            require(tx.outputs[self].value + refundTip >= carriers);\n")] },
  { id: "AU-KCA-14", file: "KobCondAskKron", run: { [T_AU_KCA]: ["NAU-KCA11"] }, note: "no fill before activeFrom", edits: [del("            require(tx.daa >= activeFrom);\n")] },
  { id: "AU-KCA-15", file: "KobCondAskKron", hold: { [T_AU_KCA]: ["NAU-KCA19"] }, note: "fill n > 0 (NAU-KCA19, n = -1, is also refused by n >= minFill || n == amountLeft; AU-KCA-15x removes both)", edits: [del("            require(n > 0);\n")] },
  { id: "AU-KCA-15x", file: "KobCondAskKron", run: { [T_AU_KCA]: ["NAU-KCA19"] }, note: "n > 0 and the minFill rule removed together: a fill of n = -1 (the taker adds a unit, the maker is paid nothing)", edits: [del("            require(n > 0);\n"), del("            require(n >= minFill || n == amountLeft);\n")] },
  { id: "AU-KCA-16", file: "KobCondAskKron", run: { [T_AU_KCA]: ["NAU-KCA12"] }, note: "scale > 0 (hostile field: a negative scale sells for nothing)", edits: [del("            require(scale > 0);\n")] },
  { id: "AU-KCA-17", file: "KobCondAskKron", run: { [T_AU_KCA]: ["NAU-KCA13"] }, note: "tip >= 0 (hostile field)", edits: [del("            require(tip >= 0);\n")] },
  { id: "AU-KCA-18", file: "KobCondAskKron", run: { [T_AU_KCA]: ["NAU-KCA22"] }, note: "n <= amountLeft (a fill claiming more than the custody holds)", edits: [del("            require(n <= amountLeft);\n")] },
  { id: "AU-KCA-19", file: "KobCondAskKron", run: { [T_AU_KCA]: ["NAU-KCA23"] }, note: "the TP leg needs tpPrice > 0 (a stop-only order would sell on the TP leg for nothing)", edits: [del("                require(tpPrice > 0);\n")] },
  { id: "AU-KCA-20", file: "KobCondAskKron", run: { [T_AU_KCA]: ["NAU-KCA24"] }, note: "leg is 0 or 1", edits: [del("                require(leg == 1);\n")] },
  { id: "AU-KCA-21", file: "KobCondAskKron", run: { [T_AU_KCA]: ["NAU-KCA25"] }, note: "the stop leg needs stopPrice > 0", edits: [del("                require(stopPrice > 0);\n")] },
  { id: "AU-KCA-22", file: "KobCondAskKron", run: { [T_AU_KCA]: ["NAU-KCA26"] }, note: "slipBps >= 0 (hostile field)", edits: [del("                require(slipBps >= 0);\n")] },
  { id: "AU-KCA-23", file: "KobCondAskKron", run: { [T_AU_KCA]: ["NAU-KCA27"] }, note: "slipBps <= 10000 (hostile field: a band wider than the stop price; outside an auction the leg price would also go negative)", edits: [del("                require(slipBps <= 10000);\n")] },
  { id: "AU-KCA-24", file: "KobCondAskKron", run: { [T_AU_KCA]: ["NAU-KCA28"] }, note: "stopPrice <= MAX_STOP (the band product must fit; with slipBps 0 an oversized stop would sell)", edits: [del("                require(stopPrice <= MAX_STOP);\n")] },
  { id: "AU-KCA-25", file: "KobCondAskKron", run: { [T_AU_KCA]: ["NAU-KCA17"] }, note: "positional payout: output i pays P2PK(maker)", edits: [del("            require(tx.outputs[self].scriptPubKey == byte[](new ScriptPubKeyP2PK(maker)));\n")] },
  { id: "AU-KCA-26", file: "KobCondAskKron", run: { [T_AU_KCA]: ["NAU-KCA14"] }, note: "rest: exactly one output bound to the order's id", edits: [del("                require(OpCovOutputCount(selfId) == 1);\n")] },
  { id: "AU-KCA-27", file: "KobCondAskKron", run: { [T_AU_KCA]: ["NAU-KCA29"] }, note: "rest: the continuation keeps the order's carrier", edits: [del("                require(tx.outputs[selfOut].value >= tx.inputs[self].value);\n")] },
  { id: "AU-KCA-28", file: "KobCondAskKron", run: { [T_AU_KCA]: ["NAU-KCA16"] }, note: "rest: the rest custody keeps its carrier", edits: [del("                require(tx.outputs[tokOut].value >= tx.inputs[tokenIn].value);\n")] },
  { id: "AU-KCA-29", file: "KobCondAskKron", run: { [T_AU_KCA]: ["NAU-KCA18"] }, note: "sold out: no output bound to the order's id", edits: [del("                require(OpCovOutputCount(selfId) == 0);\n")] },
  { id: "AU-KCA-30", file: "KobCondAskKron", run: { [T_AU_KCA]: ["NAU-KCA31"] }, note: "re-arming sell-out: the maker gets the profit (all-in minus the budget)", edits: [del("                    require(tx.outputs[self].value >= allIn);\n")] },
  { id: "AU-KCA-31", file: "KobCondAskKron", run: { [T_AU_KCA]: ["NAU-KCA20"] }, note: "sold out: the maker gets the all-in proceeds plus both carriers", edits: [del("                    require(tx.outputs[self].value >= allIn + carriers);\n")] },
  { id: "AU-KCA-32", file: "KobCondAskKron", run: { [T_AU_KCA]: ["NAU-KCA30"], [T_KCI]: ["KC1","KC2"] }, note: "the one token output (rest, refund) is pinned (template, amount, owner, id_type; the minter byte is also refused by the token program)", edits: [del("            require(tx.outputs[outIdx].scriptPubKey == byte[](new ScriptPubKeyP2SHFromRedeemScript(\n                trs.slice(0, tplPrefixLen) + byte[](0x20) + byte[](outOwner) + byte[](0x01) + byte[](outType) + byte[](0x08)\n                    + byte[](outAmount as byte[8]) + byte[](0x0100) + trs.slice(tplPrefixLen + 46, trs.length)\n            )));\n")] },
  { id: "AU-KCA-33", file: "KobCondAskKron", run: { [T_AU_KCA]: ["NAU-KCA21"] }, note: "the pinned token output is bound to the token covenant", edits: [del("            require(OpOutputCovenantId(outIdx) == tokenCovId);\n")] },
  { id: "AU-KCA-34", file: "KobCondAskKron", run: { [T_AU_KCA]: ["NAU-KCA32"] }, note: "update: the order is the only input of its covenant id", edits: [rep("    entry update(int ev, int tk) {\n        int self = this.activeInputIndex;\n        byte[32] selfId = OpInputCovenantId(self);\n        require(OpCovInputCount(selfId) == 1);\n", "    entry update(int ev, int tk) {\n        int self = this.activeInputIndex;\n        byte[32] selfId = OpInputCovenantId(self);\n")] },
  { id: "AU-KCA-35", file: "KobCondAskKron", run: { [T_AU_KCA]: ["NAU-KCA33"] }, note: "update: stopPrice > 0 (an order without a stop leg is never armed or trailed)", edits: [rep("        require(armed == 0);\n        require(stopPrice > 0);\n", "        require(armed == 0);\n")] },
  { id: "AU-KCA-36", file: "KobCondAskKron", run: { [T_AU_KCA]: ["NAU-KCA34"] }, note: "update: not before activeFrom", edits: [rep("        require(keeperTip >= 0);\n        require(tx.daa >= activeFrom);\n", "        require(keeperTip >= 0);\n")] },
  { id: "AU-KCA-37", file: "KobCondAskKron", run: { [T_AU_KCA]: ["NAU-KCA35"] }, note: "update: exactly one output bound to the order's id", edits: [rep("        }\n        require(OpCovOutputCount(selfId) == 1);\n", "        }\n")] },
  { id: "AU-KCA-38", file: "KobCondAskKron", run: { [T_AU_KCA]: ["NAU-KCA36"] }, note: "update: the continuation changes only stopPrice / armed as the evidence justifies", edits: [del("        require(tx.outputs[selfOut].scriptPubKey == contSpk(newStop, newArmed, amountLeft));\n")] },
  { id: "AU-KCA-39", file: "KobCondAskKron", run: { [T_AU_KCA]: ["NAU-KCA37"] }, note: "cancel: SIGHASH_ALL only", edits: [del("        require(sb[64] == SIGHASH_ALL);\n")] },
  { id: "AU-KCA-40", file: "KobCondAskKron", run: { [T_AU_KCA]: ["NAU-KCA38"] }, note: "cancel: the maker's signature", edits: [del("        require(checkSig(s, maker));\n")] },
  { id: "AU-KIB-1", file: "KobIfdBidKron", run: { [T_IF]: ["NTI18"], [T_IA]: ["NTJ18"] }, note: "tplState (evidence bid, merged exit): the input is the P2SH of the redeem script it pushes", edits: [del("        require(tx.inputs[idx].scriptPubKey == byte[](new ScriptPubKeyP2SHFromRedeemScript(rs)));\n")] },
  { id: "AU-KIB-2", file: "KobIfdBidKron", run: { [T_IF]: ["NTI17"], [T_IA]: ["NTJ17"] }, note: "tplState (evidence bid, merged exit): the template hash (a look-alike evidence bid)", edits: [del("        require(blake3(byte[](pre as byte[8]) + rs.slice(0, pre) + byte[](suf as byte[8]) + rs.slice(pre + stLen, size)) == tpl);\n")] },
  { id: "AU-KIB-3", file: "KobIfdBidKron", run: { [T_AU_KIB]: ["NAU-KIB30"] }, inputOnly: ["NAU-KIB30"], note: "noStrays bound: at most MAX_TOK_IN = 4 token inputs (a 5th is not scanned; the KRON program refuses a 5th input on its own, so only the entry flips)", edits: [del("        require(cnt <= MAX_TOK_IN);\n")] },
  { id: "AU-KIB-S0", file: "KobIfdBidKron", run: { [T_AU_KIB]: ["NAU-KIBS0"] }, note: "noStrays scan slot 0: a stray of the entry at token slot 0 rides to the taker", edits: [rep("require(byte[32](OpTxInputScriptSigSubstr(x0, e0, e0 + 32)) != me);", 'require(true);')] },
  { id: "AU-KIB-S1", file: "KobIfdBidKron", run: { [T_AU_KIB]: ["NAU-KIBS1"] }, note: "noStrays scan slot 1: a stray of the entry at token slot 1 rides to the taker", edits: [rep("require(byte[32](OpTxInputScriptSigSubstr(x1, e1, e1 + 32)) != me);", 'require(true);')] },
  { id: "AU-KIB-S2", file: "KobIfdBidKron", run: { [T_AU_KIB]: ["NAU-KIBS2"] }, note: "noStrays scan slot 2: a stray of the entry at token slot 2 rides to the taker", edits: [rep("require(byte[32](OpTxInputScriptSigSubstr(x2, e2, e2 + 32)) != me);", 'require(true);')] },
  { id: "AU-KIB-S3", file: "KobIfdBidKron", run: { [T_AU_KIB]: ["NAU-KIBS3"] }, note: "noStrays scan slot 3: a stray of the entry at token slot 3 rides to the taker", edits: [rep("require(byte[32](OpTxInputScriptSigSubstr(x3, e3, e3 + 32)) != me);", 'require(true);')] },
  { id: "AU-KIB-4", file: "KobIfdBidKron", run: { [T_AU_KIB]: ["NAU-KIB01"] }, note: "fill / merge: the entry is the only input of its covenant id", edits: [rep("        int n = int(nb);\n        int self = this.activeInputIndex;\n        byte[32] selfId = OpInputCovenantId(self);\n        require(OpCovInputCount(selfId) == 1);\n", "        int n = int(nb);\n        int self = this.activeInputIndex;\n        byte[32] selfId = OpInputCovenantId(self);\n")] },
  { id: "AU-KIB-5", file: "KobIfdBidKron", run: { [T_AU_KIB]: ["NAU-KIB02"] }, note: "scale > 0 (hostile field: a negative scale turns the spend negative)", edits: [del("        require(scale > 0);\n")] },
  { id: "AU-KIB-6", file: "KobIfdBidKron", run: { [T_AU_KIB]: ["NAU-KIB03"] }, note: "tip >= 0 (hostile field)", edits: [del("        require(tip >= 0);\n")] },
  { id: "AU-KIB-7", file: "KobIfdBidKron", run: { [T_AU_KIB]: ["NAU-KIB04"] }, note: "merge: the exit matches the committed exitState on [189..244) (slipBps .. minRestDaa)", edits: [del("            require(x.slice(189, 244) == st.slice(189, 244));\n")] },
  { id: "AU-KIB-8", file: "KobIfdBidKron", run: { [T_AU_KIB]: ["NAU-KIB05"] }, note: "merge: the exit matches the committed exitState on [252..271) (bandDaa, keeperTip)", edits: [del("            require(x.slice(252, 271) == st.slice(252, 271));\n")] },
  { id: "AU-KIB-9", file: "KobIfdBidKron", run: { [T_AU_KIB]: ["NAU-KIB07"] }, note: "no fill before activeFrom", edits: [del("            require(tx.daa >= activeFrom);\n")] },
  { id: "AU-KIB-10", file: "KobIfdBidKron", run: { [T_AU_KIB]: ["NAU-KIB08"] }, inputOnly: ["NAU-KIB08"], note: "fill n > 0 (a zero fill of an empty repeating entry books an empty exit; the KRON program also refuses the empty delivery on its own input, so only the entry flips)", edits: [del("            require(n > 0);\n")] },
  { id: "AU-KIB-11", file: "KobIfdBidKron", run: { [T_OCO]: ["NI10"] }, note: "n <= amountLeft", edits: [del("            require(n <= amountLeft);\n")] },
  { id: "AU-KIB-12", file: "KobIfdBidKron", run: { [T_AU_KIB]: ["NAU-KIB09"] }, note: "deliveryCarrier >= 0 (hostile field)", edits: [del("            require(deliveryCarrier >= 0);\n")] },
  { id: "AU-KIB-13", file: "KobIfdBidKron", run: { [T_AU_KIB]: ["NAU-KIB10"] }, note: "exitCarrier >= 0 (hostile field)", edits: [del("            require(exitCarrier >= 0);\n")] },
  { id: "AU-KIB-14", file: "KobIfdBidKron", run: { [T_AU_KIB]: ["NAU-KIB31"] }, note: "stop entry auction: tx.daa >= t (CLTV proves the auction time)", edits: [del("                        require(tx.daa >= t);\n")] },
  { id: "AU-KIB-15", file: "KobIfdBidKron", run: { [T_AU_KIB]: ["NAU-KIB32"] }, note: "stop entry auction: t >= the auction origin", edits: [del("                        require(t >= newArmed);\n")] },
  { id: "AU-KIB-16", file: "KobIfdBidKron", run: { [T_AU_KIB]: ["NAU-KIB33"] }, note: "repeat: while a minimum fill of re-arms is left, a fill takes at most that many (and is booked)", edits: [del("                require(rptAmount <= 1 || rptAmount - 1 < minFill);\n")] },
  { id: "AU-KIB-17", file: "KobIfdBidKron", run: { [T_AU_KIB]: ["NAU-KIB34"] }, hold: { [T_OCO]: ["NI4"] }, note: "the exit template is KobCondAskKron (COND_TPL): a look-alike template around the committed state would make the exit and its tokens anyone-can-spend (NI4, the KobAskKron template beside a KobCondAskKron output, is also refused by the exit script equality)", edits: [del("            require(blake3(byte[](cPre.length as byte[8]) + cPre + byte[](cSuf.length as byte[8]) + cSuf) == COND_TPL);\n")] },
  { id: "AU-KIB-18", file: "KobIfdBidKron", hold: { [T_OCO]: ["NI2","NI2b"] }, note: "the exit is exactly the committed KobCondAskKron with amountLeft := n (defence in depth: the exit covenant id is derived from exitSpk, so a tampered exit output also fails the genesis id check; AU-KIB-18x removes both)", edits: [del("            require(tx.outputs[exitOut].scriptPubKey == exitSpk);\n")] },
  { id: "AU-KIB-19", file: "KobIfdBidKron", run: { [T_OCO]: ["NI6","NI8"] }, hold: { [T_OCO]: ["NI7"] }, note: "the exit is a fresh genesis covenant created by this input (NI7, the entry's own id, is also refused by the continuation count)", edits: [del("            require(exitId == blake2bWithKey(\n                byte[](OpOutpointTxId(self)) + byte[](OpOutpointIndex(self) as byte[4]) + byte[](0x0100000000000000)\n                    + byte[](exitOut as byte[4]) + byte[](tx.outputs[exitOut].value as byte[8])\n                    + exitSpk.slice(0, 2) + byte[](0x2300000000000000) + exitSpk.slice(2, 37),\n                byte[](\"CovenantID\")\n            ));\n")] },
  { id: "AU-KIB-18x", file: "KobIfdBidKron", run: { [T_OCO]: ["NI2"] }, note: "the exit script equality and the genesis id check removed together: a tampered exit is accepted", edits: [del("            require(tx.outputs[exitOut].scriptPubKey == exitSpk);\n"), del("            require(exitId == blake2bWithKey(\n                byte[](OpOutpointTxId(self)) + byte[](OpOutpointIndex(self) as byte[4]) + byte[](0x0100000000000000)\n                    + byte[](exitOut as byte[4]) + byte[](tx.outputs[exitOut].value as byte[8])\n                    + exitSpk.slice(0, 2) + byte[](0x2300000000000000) + exitSpk.slice(2, 37),\n                byte[](\"CovenantID\")\n            ));\n")] },
  { id: "AU-KIB-20", file: "KobIfdBidKron", run: { [T_OCO]: ["NI5c"] }, note: "continuing fill: the exit holds at least exitCarrier", edits: [del("                require(tx.outputs[exitOut].value >= exitCarrier);\n")] },
  { id: "AU-KIB-21", file: "KobIfdBidKron", run: { [T_OCO]: ["NI5"] }, note: "final fill: the exit gets everything left", edits: [del("                require(tx.outputs[exitOut].value >= left);\n")] },
  { id: "AU-KIB-22", file: "KobIfdBidKron", run: { [T_AU_KIB]: ["NAU-KIB12"] }, note: "the delivery template is read from a token input of this token", edits: [del("            require(OpInputCovenantId(tokenTplIn) == tokenCovId);\n")] },
  { id: "AU-KIB-23", file: "KobIfdBidKron", run: { [T_AU_KIB]: ["NAU-KIB13"] }, note: "the delivery template's hash is the token's (a planted look-alike would give the exit an anyone-can-spend fake)", edits: [del("            require(blake3(byte[](tplPrefixLen as byte[8]) + tpre + byte[](tplSuffixLen as byte[8]) + tsuf) == tokenTplHash);\n")] },
  { id: "AU-KIB-24", file: "KobIfdBidKron", run: { [T_OCO]: ["NI1","NI3"], [T_KCI]: ["KI1","KI2"] }, note: "positional delivery: output i holds exactly n base units owned by the exit's id with id_type 2 (the minter byte is also refused by the token program)", edits: [del("            require(tx.outputs[self].scriptPubKey == byte[](new ScriptPubKeyP2SHFromRedeemScript(\n                tpre + byte[](0x20) + byte[](exitId) + byte[](0x01) + byte[](exitType) + byte[](0x08)\n                    + byte[](nb) + byte[](0x0100) + tsuf\n            )));\n")] },
  { id: "AU-KIB-25", file: "KobIfdBidKron", run: { [T_AU_KIB]: ["NAU-KIB14"] }, note: "the delivery is bound to the token covenant", edits: [del("            require(OpOutputCovenantId(self) == tokenCovId);\n")] },
  { id: "AU-KIB-26", file: "KobIfdBidKron", run: { [T_AU_KIB]: ["NAU-KIB15"] }, note: "the delivery carries at least deliveryCarrier", edits: [del("            require(tx.outputs[self].value >= deliveryCarrier);\n")] },
  { id: "AU-KIB-27", file: "KobIfdBidKron", run: { [T_AU_KIB]: ["NAU-KIB11"] }, note: "continuation: exactly one output bound to the entry's id", edits: [del("            require(OpCovOutputCount(selfId) == 1);\n")] },
  { id: "AU-KIB-28", file: "KobIfdBidKron", run: { [T_OCO]: ["NI5b"] }, note: "continuation: holds at least the floor", edits: [del("            require(tx.outputs[c].value >= floor);\n")] },
  { id: "AU-KIB-29", file: "KobIfdBidKron", run: { [T_AU_KIB]: ["NAU-KIB16"] }, note: "termination: no output bound to the entry's id", edits: [del("            require(OpCovOutputCount(selfId) == 0);\n")] },
  { id: "AU-KIB-30", file: "KobIfdBidKron", run: { [T_AU_KIB]: ["NAU-KIB17"] }, note: "update: the entry is the only input of its covenant id", edits: [rep("    entry update(int ev) {\n        int self = this.activeInputIndex;\n        byte[32] selfId = OpInputCovenantId(self);\n        require(OpCovInputCount(selfId) == 1);\n", "    entry update(int ev) {\n        int self = this.activeInputIndex;\n        byte[32] selfId = OpInputCovenantId(self);\n")] },
  { id: "AU-KIB-31", file: "KobIfdBidKron", run: { [T_AU_KIB]: ["NAU-KIB18"] }, note: "update: only an unarmed entry (re-arming an armed auction entry would restart its auction)", edits: [del("        require(armed == 0);\n")] },
  { id: "AU-KIB-32", file: "KobIfdBidKron", run: { [T_AU_KIB]: ["NAU-KIB20"] }, note: "update: not before activeFrom", edits: [rep("        require(keeperTip >= 0);\n        require(tx.daa >= activeFrom);\n", "        require(keeperTip >= 0);\n")] },
  { id: "AU-KIB-33", file: "KobIfdBidKron", run: { [T_AU_KIB]: ["NAU-KIB21"] }, note: "update: exactly one output bound to the entry's id", edits: [rep("        require(touchBid(ev) >= entryStop);\n        require(OpCovOutputCount(selfId) == 1);\n", "        require(touchBid(ev) >= entryStop);\n")] },
  { id: "AU-KIB-34", file: "KobIfdBidKron", run: { [T_AU_KIB]: ["NAU-KIB22"] }, note: "cancel: SIGHASH_ALL only", edits: [del("        require(sb[64] == SIGHASH_ALL);\n")] },
  { id: "AU-KIB-35", file: "KobIfdBidKron", run: { [T_AU_KIB]: ["NAU-KIB23"] }, note: "cancel: the maker's signature", edits: [del("        require(checkSig(s, maker));\n")] },
  { id: "AU-KIB-36", file: "KobIfdBidKron", run: { [T_AU_KIB]: ["NAU-KIB24"] }, note: "refund only from min(expiryDaa, 90 days idle)", edits: [del("        require(tx.daa >= idleEnd);\n")] },
  { id: "AU-KIB-37", file: "KobIfdBidKron", run: { [T_AU_KIB]: ["NAU-KIB25"] }, note: "refund: the entry is the only input of its covenant id", edits: [rep("        require(tx.daa >= idleEnd);\n        byte[32] selfId = OpInputCovenantId(self);\n        require(OpCovInputCount(selfId) == 1);\n", "        require(tx.daa >= idleEnd);\n        byte[32] selfId = OpInputCovenantId(self);\n")] },
  { id: "AU-KIB-38", file: "KobIfdBidKron", run: { [T_AU_KIB]: ["NAU-KIB27"] }, note: "refund: no output bound to the entry's id", edits: [rep("        require(OpCovInputCount(tokenCovId) == 0);\n        require(OpCovOutputCount(selfId) == 0);\n", "        require(OpCovInputCount(tokenCovId) == 0);\n")] },
  { id: "AU-KIB-39", file: "KobIfdBidKron", run: { [T_AU_KIB]: ["NAU-KIB28"] }, note: "refund: output i pays P2PK(maker)", edits: [del("        require(tx.outputs[self].scriptPubKey == byte[](new ScriptPubKeyP2PK(maker)));\n")] },
  { id: "AU-KIB-40", file: "KobIfdBidKron", run: { [T_AU_KIB]: ["NAU-KIB29"] }, note: "refund: the maker gets the escrow minus at most refundTip", edits: [del("        require(tx.outputs[self].value + refundTip >= tx.inputs[self].value);\n")] },
  // CI-END
];
