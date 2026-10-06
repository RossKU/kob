// KRON sell-side suite: contracts/adapters/kron/v2 (KobAskKron, KobBidKron, KobCondAskKron, KobIfdBidKron), protocol v3
// (amounts in base units, minFill, quoteOf rounding in the maker's favour, merge argument -(k * 2^53 + m)).
// Ported from the ad-hoc ablate_sell.mjs; ids and expectations preserved where the check still exists. Each mutation must
// flip its scenarios on BOTH pinned KRON templates (the tests loop over both and print "=== KRON template <file>").
// Touch trigger: the T*-* entries (lib/touch.mjs generates one per evidence check; see README). The V3-* entries mutate
// the checks protocol v3 added (minFill, rounding direction, tip <= price, the 0x08 merge checks, the update parent guard,
// n < MERGE_K on booking).
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
    note: 'booking time t <= lockTime (CLTV)',
    edits: [
      rep(`                require(tx.daa >= t);
                int utxoDaa`, '                int utxoDaa'),
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
];
