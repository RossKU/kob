// KCC-20 buy-side lifecycle suite (protocol v3, base units, no lot layouts): contracts/v2.
// Scenario ids follow the current test names (NRP* / NRPB*; new v3 attacks V3* / NV3*).
// Touch trigger: the T*-* entries (lib/touch.mjs generates one per evidence check; see README). The touch keys
// renamed in v3: `unit` -> `scale`, `units` -> `mintouch` (the evidence quotes per the same scale; n >= minTouch base units).
import { del, rep, rx, rxDel } from '../lib/edits.mjs';
import { touchMutations } from '../lib/touch.mjs';

export const suite = {
  name: 'kcc20-buy',
  family: 'KCC-20 v3',
  testBin: 'kob_v2_buy_tests',
  srcDir: 'contracts/v2',
  templateMarker: null,
  expectedTemplates: 1,
};

const T_RPT = 'v2_buy_repeat_ifd_attacks';
// touch trigger batteries, one test fn per trigger kind
const T_BS = 'v2_buy_touch_cond_settle';
const T_BA = 'v2_buy_touch_cond_arm';
const T_BT = 'v2_buy_touch_cond_trail';
const T_AF = 'v2_buy_touch_ifd_fill';
const T_AA = 'v2_buy_touch_ifd_arm';
const T_NEG = 'v2_buy_negative';
const T_LC = 'v2_buy_lifecycle';
// protocol v3 test fns (base units, rounding, minFill, tip, repeat-merge framing)
const T_MF = 'v3_buy_min_fill';
const T_RND = 'v3_buy_rounding';
const T_TIP = 'v3_buy_tip';
const T_MB = 'v3_buy_merge_base';
const T_MP = 'v3_buy_merge_push';

export const mutations = [
  // ---- KobCondBid
  {
    id: 'C1',
    file: 'KobCondBid',
    test: T_RPT,
    expect: ['NRPB10'],
    note: 'TP without the entry only from rptUntil',
    edits: [
      del('                    require(tx.daa >= rptUntil);'),
    ],
  },
  {
    id: 'C2',
    file: 'KobCondBid',
    test: T_RPT,
    expect: ['NRPB13'],
    note: 'stop-leg fill refuses the entry (present == 0)',
    edits: [
      del('                require(present == 0);'),
    ],
  },
  {
    id: 'C3',
    file: 'KobCondBid',
    test: T_RPT,
    expect: ['NRPB14'],
    hold: { [T_RPT]: ['NRPB34'] },
    note: 'entry merge argument names this exit and n (NRPB34, an arming update of the entry named as the merge, is also refused by the v3 0x08 first-byte check: held here, flipped by C3b)',
    edits: [
      del('            require(int(byte[8](OpTxInputScriptSigSubstr(pin, 1, 9))) == 0 - (self * MERGE_K + n));'),
    ],
  },
  {
    id: 'C3b',
    file: 'KobCondBid',
    test: T_RPT,
    expect: ['NRPB34'],
    note: 'both checks on the entry pin removed (0x08 first byte and the merge value): an arming update of the entry is read as the merge',
    edits: [
      del('            require(OpTxInputScriptSigSubstr(pin, 0, 1) == byte[](0x08));\n'),
      del('            require(int(byte[8](OpTxInputScriptSigSubstr(pin, 1, 9))) == 0 - (self * MERGE_K + n));'),
    ],
  },
  {
    id: 'C4',
    file: 'KobCondBid',
    test: T_RPT,
    expect: ['NRPB11'],
    note: 'maker profit >= ceil proceeds - spend',
    edits: [
      del('            require(tx.outputs[self].value >= proceeds - spend);'),
    ],
  },
  {
    id: 'C5',
    file: 'KobCondBid',
    run: { [T_RPT]: ['NRPB12'], [T_MB]: ['NV3CB13'] },
    note: 'exit continuation keep >= in - proceeds - ceil prefund (NV3CB13: a one-sompi-short continuation at a non-multiple amount)',
    edits: [
      rep('            keep = tx.inputs[self].value - proceeds - quoteOf(n, rptPre, scale - 1);', '            keep = 0;'),
    ],
  },
  // ---- KobIfdAsk
  {
    id: 'D1',
    file: 'KobIfdAsk',
    test: T_RPT,
    hold: { [T_RPT]: ['NRPB21'] },
    note: 'exit is a genuine KobCondBid (template + P2SH check) (the crafted input also fails the exit-terms comparison, confirmed on its own by FXL3c/d, so removing the template check alone holds)',
    edits: [
      rep('                byte[] x = tplState(k, COND_BID_PRE, EXIT_STATE_LEN, COND_BID_SUF, COND_BID_TPL);', '                byte[] x = OpTxInputScriptSigSubstr(k, OpTxInputScriptSigLen(k) - EXIT_STATE_LEN - COND_BID_SUF, OpTxInputScriptSigLen(k) - COND_BID_SUF);'),
    ],
  },
  {
    id: 'D2',
    file: 'KobIfdAsk',
    test: T_RPT,
    hold: { [T_RPT]: ['NRPB20'] },
    note: 'exit\'s parent = this entry (the parent check is part of the exit-tail comparison with rptPrice; a plain exit also fails on rptPrice, so removing the parent alone holds)',
    edits: [
      rep('byte[](0x20) + byte[](selfId) + byte[](0x08)', 'byte[](0x20) + x.slice(322, 354) + byte[](0x08)'),
    ],
  },
  {
    id: 'D3',
    file: 'KobIfdAsk',
    test: T_RPT,
    expect: ['NRPB15'],
    note: 'merge: partial buy-back re-armed with ceil(m * prefund)',
    edits: [
      rep('                floor = tx.inputs[self].value + quoteOf(m, prefund, scale - 1);', '                floor = 0;'),
    ],
  },
  {
    id: 'D3b',
    file: 'KobIfdAsk',
    run: { [T_RPT]: ['NRPB16'], [T_MB]: ['NV3IA13'] },
    note: 'merge: sell-out leftovers come back (floor with the exit value; NV3IA13: a one-sompi-short sell-out continuation at a non-multiple amount)',
    edits: [
      del('                    if (all > floor) { floor = all; }'),
    ],
  },
  {
    id: 'D4',
    file: 'KobIfdAsk',
    test: T_RPT,
    expect: ['NRPB16b'],
    note: 'merge: custody carrier kept (tokOut value >= keepCarrier)',
    edits: [
      del('                require(tx.outputs[tokOut].value >= keepCarrier);'),
    ],
  },
  {
    id: 'D5',
    file: 'KobIfdAsk',
    test: T_RPT,
    expect: ['NRPB18'],
    note: 'token output pin (custody / remainder / refund state), condition inverted',
    edits: [
      rep(`        if (outAmount > 0) {
            // Pin the only token output`, `        if (outAmount < 0) {
            // Pin the only token output`),
    ],
  },
  {
    id: 'D6',
    file: 'KobIfdAsk',
    test: T_RPT,
    expect: ['NRPB19'],
    note: 'continuation SPK equality (limit, cycle count, amount)',
    edits: [
      del('            require(tx.outputs[c].scriptPubKey == contSpk(newArmed, newLeft, newRpt));'),
    ],
  },
  {
    id: 'D6b',
    file: 'KobIfdAsk',
    test: T_RPT,
    expect: ['NRPB19d'],
    note: 'empty stop entry re-arms unarmed (newArmed = 0)',
    edits: [
      rep(`if (amountLeft == 0) {
                    newArmed = 0;
`, `if (amountLeft == 0) {
`),
    ],
  },
  {
    id: 'D7',
    file: 'KobIfdAsk',
    test: T_RPT,
    expect: ['NRPB24b'],
    note: 'an empty entry has no custody (custody = 0 - 1 statement removed)',
    edits: [
      del('            custody = 0 - 1;'),
    ],
  },
  {
    id: 'D7b',
    file: 'KobIfdAsk',
    test: T_RPT,
    expect: ['NRPB24'],
    note: 'stray scan in merge (noStrays(selfId, custody))',
    edits: [
      del('        noStrays(selfId, custody);'),
    ],
  },
  {
    id: 'D8',
    file: 'KobIfdAsk',
    test: T_RPT,
    expect: ['NRPB25'],
    note: 'custody holds exactly amountLeft base units (amount check)',
    edits: [
      del('            require(int(byte[8](tin.slice(1, 9))) == tinAmount);'),
    ],
  },
  {
    id: 'D9',
    file: 'KobIfdAsk',
    test: T_RPT,
    expect: ['NRPB6'],
    note: 'booking time t <= lockTime',
    edits: [
      rep(`                    require(tx.daa >= t);
                    int utxoDaa`, '                    int utxoDaa'),
    ],
  },
  {
    id: 'D10',
    file: 'KobIfdAsk',
    test: T_RPT,
    expect: ['NRPB7'],
    note: 'repeating entry never terminates on a fill',
    edits: [
      rep('                if (outAmount > 0 || rptAmount > 0) {', '                if (outAmount > 0) {'),
    ],
  },
  {
    id: 'D11',
    file: 'KobIfdAsk',
    test: T_RPT,
    expect: ['NRPB8'],
    note: 'sold out: entry keeps the custody carrier',
    edits: [
      del('                        floor = floor + tx.inputs[tokenIn].value;'),
    ],
  },
  {
    id: 'D12',
    file: 'KobIfdAsk',
    test: T_RPT,
    expect: ['NRPB26'],
    note: 'close only with no amount left',
    edits: [
      del('        require(amountLeft == 0);'),
    ],
  },
  {
    id: 'D13',
    file: 'KobIfdAsk',
    test: T_RPT,
    expect: ['NRPB28'],
    note: 'close refuses token inputs',
    edits: [
      rep(`        require(OpCovInputCount(tokenCovId) == 0);
        require(OpCovOutputCount(selfId) == 0);
        require(refundTip >= 0);
        require(tx.outputs[self].scriptPubKey == byte[](new ScriptPubKeyP2PK(maker)));
        require(tx.outputs[self].value + refundTip >= tx.inputs[self].value);
    }

    // Arm`, `        require(OpCovOutputCount(selfId) == 0);
        require(refundTip >= 0);
        require(tx.outputs[self].scriptPubKey == byte[](new ScriptPubKeyP2PK(maker)));
        require(tx.outputs[self].value + refundTip >= tx.inputs[self].value);
    }

    // Arm`),
    ],
  },
  // ---- mutation control (from mutate.sh, KCC-20 auction check; the buy-side half of M2, KobCondBid)
  {
    id: 'M2',
    file: 'KobCondBid',
    test: 'v2_buy_lifecycle',
    expect: ['NB29'],
    note: 'control: buy stop auction time >= arming origin (require(t >= newArmed))',
    edits: [del('require(t >= newArmed);')],
  },
  // ---- fix pass (2026-09-30)
  {
    id: 'FXL3c',
    file: 'KobIfdAsk',
    test: T_RPT,
    expect: ['NRPB29'],
    note: 'the merge requires the exit terms [0..223) (maker, legs) to be the committed exitState',
    edits: [del('                require(x.slice(0, 223) == st.slice(0, 223));\n')],
  },
  {
    id: 'FXL3d',
    file: 'KobIfdAsk',
    test: T_RPT,
    expect: ['NRPB30'],
    note: 'the merge requires parent = this entry, rptPrice and rptPre = its own',
    edits: [rxDel('int rate = price - tip;', /^\s*require\(x\.slice\(321, 373\) == [\s\S]*?\);$/m)],
  },
  {
    id: 'FXL3e',
    file: 'KobIfdAsk',
    hold: { [T_RPT]: ['NRPB32', 'NRPB33'] },
    note: 'the merge requires the exit to run its settle with n = m (sigscript [1..9)); defence in depth in v3: a cancel (NRPB32) or an update (NRPB33) of the exit never starts with the 0x08 push either, so the first-byte check alone still refuses them (both removed: FXL3g)',
    edits: [del('                require(int(byte[8](OpTxInputScriptSigSubstr(k, 1, 9))) == m);\n')],
  },
  {
    id: 'FXL3g',
    file: 'KobIfdAsk',
    test: T_RPT,
    expect: ['NRPB32', 'NRPB33'],
    inputOnly: ['NRPB33'],
    note: 'both checks on the exit input removed (0x08 first byte and n = m): a cancel (NRPB32) or an arming update (NRPB33; its own parent guard still refuses it, so the entry input alone flips) of the exit is read as its settle',
    edits: [
      del('                require(OpTxInputScriptSigSubstr(k, 0, 1) == byte[](0x08));\n'),
      del('                require(int(byte[8](OpTxInputScriptSigSubstr(k, 1, 9))) == m);\n'),
    ],
  },
  {
    id: 'FXL3f',
    file: 'KobIfdAsk',
    test: T_RPT,
    expect: ['NRPB31'],
    note: 'a sell-out merge never costs the entry (floor = max(prefund, exit value - proceeds))',
    edits: [rep('if (all > floor) { floor = all; }', 'floor = all;')],
  },
  // ---- touch trigger (v2.6): KobCondBid touchBid (buy-stop leg armed inside its fill), touch (update: arm / trail)
  ...touchMutations({
    prefix: 'TB',
    file: 'KobCondBid',
    fn: 'function touchBid(int ev) : int {',
    what: 'touchBid (settle, buy-stop leg)',
    runs: {
      push08: { [T_BS]: ['NTB10'] },
      npos: { run: { [T_BS]: ['NTB11b'] }, inputOnly: ['NTB11b'] },
      tpl: { [T_BS]: ['NTB17'] },
      p2sh: { [T_BS]: ['NTB18'] },
      token: { [T_BS]: ['NTB07'] },
      scale: { [T_BS]: ['NTB08'] },
      mintouch: { [T_BS]: ['NTB09'] },
      slope: { [T_BS]: ['NTB05'] },
      cltv: { [T_BS]: ['NTB01', 'NTB03', 'NTB04'] },
      interval: { [T_BS]: ['NTB04'] },
      active: { [T_BS]: ['NTB03'] },
    },
  }),
  {
    id: 'TB-rule',
    file: 'KobCondBid',
    run: { [T_BS]: ['NTB19'], [T_NEG]: ['NB13'] },
    note: 'settle, unarmed buy-stop leg: the evidence bid quotes >= stopPrice',
    edits: [rxDel('    entry settle(', /^\s*require\(rp >= stopPrice\);$/m)],
  },
  ...touchMutations({
    prefix: 'TV',
    file: 'KobCondBid',
    fn: 'function touch(int ev, int tk) : (int, int) {',
    what: 'touch (update: arm on a bid, trail on an ask)',
    runs: {
      push08: { [T_BA]: ['NTV10', 'NTV11'], [T_BT]: ['NTW10'] },
      npos: { [T_BT]: ['NTW11'] },
      tpl: { [T_BA]: ['NTV17'], [T_BT]: ['NTW17'] },
      p2sh: { [T_BA]: ['NTV18'], [T_BT]: ['NTW18'] },
      token: { [T_BA]: ['NTV07'], [T_BT]: ['NTW07b'] },
      scale: { [T_BA]: ['NTV08'], [T_BT]: ['NTW08'] },
      mintouch: { [T_BA]: ['NTV09'], [T_BT]: ['NTW09'] },
      slope: { [T_BA]: ['NTV05'], [T_BT]: ['NTW05'] },
      cltv: { [T_BA]: ['NTV01', 'NTV03', 'NTV04'], [T_BT]: ['NTW01', 'NTW02', 'NTW03', 'NTW04'] },
      interval: { [T_BA]: ['NTV04'], [T_BT]: ['NTW04'] },
      active: { [T_BA]: ['NTV03'], [T_BT]: ['NTW03'] },
      tokdaa: { [T_BT]: ['NTW02'] },
      tkcov: { [T_BT]: ['NTW02b'] },
      owner: { [T_BT]: ['NTW15', 'NTW16'] },
    },
  }),
  {
    id: 'TV-rule',
    file: 'KobCondBid',
    test: T_BA,
    expect: ['NTV19'],
    note: 'update, arm: the evidence bid quotes >= stopPrice',
    edits: [rxDel('    entry update(int ev, int tk) {', /^\s*require\(rp >= stopPrice\);$/m)],
  },
  {
    id: 'TW-k',
    file: 'KobCondBid',
    test: T_BT,
    expect: ['NTW19', 'NTW19b'],
    note: 'update, trail: the evidence ask justifies k >= 1 steps (k = 0 drains keeperTip and resets trailWait, k < 0 moves the stop up)',
    edits: [del('            require(k >= 1);\n')],
  },
  {
    id: 'TW-cap',
    file: 'KobCondBid',
    test: T_LC,
    expect: ['NB22b'],
    note: 'update, trail: the ratchet stays above the limit leg (tpPrice) and 0',
    edits: [del('            if (kf < k) { k = kf; }\n')],
  },
  {
    id: 'TW-wait',
    file: 'KobCondBid',
    test: T_NEG,
    expect: ['NB20'],
    note: 'update, trail: at most once per trailWait DAA (CSV)',
    edits: [del('            require(this.ageDaa >= trailWait);\n')],
  },
  {
    id: 'TW-step',
    file: 'KobCondBid',
    hold: { [T_NEG]: ['NB24'], [T_BA]: ['NTV06'] },
    note: 'update, trail: trailStep > 0 (a non-trailing order divides by trailStep = 0 and fails anyway)',
    edits: [del('            require(trailStep > 0);\n')],
  },
  {
    id: 'TV-armed',
    file: 'KobCondBid',
    test: T_NEG,
    expect: ['NB19'],
    note: 'update only while unarmed',
    edits: [rxDel('    entry update(int ev, int tk) {', /^\s*require\(armed == 0\);$/m)],
  },
  {
    id: 'TV-own',
    file: 'KobCondBid',
    run: { [T_BA]: ['NTV23'], [T_BT]: ['NTW23'], [T_LC]: ['NS12'] },
    note: 'update spends no token input owned by the order (bounded scan; the evidence fill brings foreign token inputs)',
    edits: [rxDel('    entry update(int ev, int tk) {', /^\s*noStrays\(selfId\);$/m)],
  },
  // ---- touch trigger (v2.6): KobIfdAsk touchAsk (sell-stop entry armed inside its fill or by update)
  ...touchMutations({
    prefix: 'TK',
    file: 'KobIfdAsk',
    fn: 'function touchAsk(int ev, int tk) : int {',
    what: 'touchAsk (sell-stop entry)',
    runs: {
      push08: { [T_AF]: ['NTK10'], [T_AA]: ['NTL10'] },
      npos: { [T_AF]: ['NTK11'], [T_AA]: ['NTL11'] },
      tpl: { [T_AF]: ['NTK17'], [T_AA]: ['NTL17'] },
      p2sh: { [T_AF]: ['NTK18'], [T_AA]: ['NTL18'] },
      token: { [T_AF]: ['NTK07b'], [T_AA]: ['NTL07b'] },
      scale: { [T_AF]: ['NTK08'], [T_AA]: ['NTL08'] },
      mintouch: { [T_AF]: ['NTK09'], [T_AA]: ['NTL09'] },
      slope: { [T_AF]: ['NTK05'], [T_AA]: ['NTL05'] },
      cltv: { [T_AF]: ['NTK01', 'NTK02', 'NTK03', 'NTK04'], [T_AA]: ['NTL01', 'NTL02', 'NTL03', 'NTL04'] },
      interval: { [T_AF]: ['NTK04'], [T_AA]: ['NTL04'] },
      active: { [T_AF]: ['NTK03'], [T_AA]: ['NTL03'] },
      tokdaa: { [T_AF]: ['NTK02'], [T_AA]: ['NTL02'] },
      tkcov: { [T_AF]: ['NTK02b'], [T_AA]: ['NTL02b'] },
      owner: { [T_AF]: ['NTK15', 'NTK16'], [T_AA]: ['NTL15', 'NTL16'] },
    },
  }),
  {
    id: 'TK-auc',
    file: 'KobIfdAsk',
    test: T_LC,
    expect: ['NI18b'],
    note: 'sell-stop entry armed inside its fill with an auction: the fill sells at the stop (the auction opens at the trigger)',
    edits: [
      del(`                        if (bandDaa > 0) {
                            p = entryStop;
                        }
`),
    ],
  },
  {
    id: 'TK-rule',
    file: 'KobIfdAsk',
    test: T_AF,
    expect: ['NTK19'],
    note: 'settle, unarmed stop entry: the evidence ask quotes <= entryStop',
    edits: [rx('    entry settle(', /require\(touchAsk\(ev, tk\) <= entryStop\);/, 'require(touchAsk(ev, tk) >= 0);')],
  },
  {
    id: 'TL-rule',
    file: 'KobIfdAsk',
    test: T_AA,
    expect: ['NTL19'],
    note: 'update: the evidence ask quotes <= entryStop',
    edits: [rx('    entry update(int ev, int tk) {', /require\(touchAsk\(ev, tk\) <= entryStop\);/, 'require(touchAsk(ev, tk) >= 0);')],
  },
  {
    id: 'TL-own',
    file: 'KobIfdAsk',
    run: { [T_AA]: ['NTL23'], [T_LC]: ['NI22'] },
    note: 'update spends no token input owned by the entry (bounded scan; the evidence fill brings foreign token inputs)',
    edits: [del('        noStrays(selfId, -1);\n')],
  },
  // ---- update arms only an entry with a positive amount and its stop on the limit's side; a merge keeps an armed entry's band origin
  {
    id: 'TL-amount',
    file: 'KobIfdAsk',
    test: T_AA,
    expect: ['NTL24'],
    note: 'update: the entry has a positive amount to fill (an empty repeating entry would pay keeperTip for a useless arm)',
    edits: [rxDel('    entry update(int ev, int tk) {', /^\s*require\(amountLeft > 0\);$/m)],
  },
  {
    id: 'TL-limit',
    file: 'KobIfdAsk',
    test: T_AA,
    expect: ['NTL25'],
    note: "update: the stop is on the limit's side (entryStop >= price; else no fill can follow the arm)",
    edits: [rxDel('    entry update(int ev, int tk) {', /^\s*require\(entryStop >= price\);$/m)],
  },
  {
    id: 'TL-origin',
    file: 'KobIfdAsk',
    test: T_RPT,
    expect: ['NRPB35'],
    note: 'merge into an entry armed by update (armed 1, band): the continuation records the band origin (else the auction restarts)',
    edits: [rep('if (armed == 1 && bandDaa > 0) {', 'if (armed == 1 && bandDaa < 0) {')],
  },

  // ================================================================ protocol v3 checks (base units, rounding, minFill, tip, merge framing)
  // ---- KobCondBid
  {
    id: 'CB-minfill',
    file: 'KobCondBid',
    test: T_MF,
    expect: ['NV3CB01'],
    note: 'minFill: n >= minFill unless the fill takes all that is left',
    edits: [rxDel('    entry settle(byte[8] nb, int tokenTplIn, int leg, int ev, int t) {', /^\s*require\(n >= minFill \|\| n == amountLeft\);$/m)],
  },
  {
    id: 'CB-spend-floor',
    file: 'KobCondBid',
    test: T_RND,
    expect: ['NV3CB10', 'NV3CB11'],
    note: 'bid spend is the floor of n * (legPrice + tip) / scale (c = 0, maker pays the floor)',
    edits: [rep('int spend = quoteOf(n, legPrice + tip, 0);', 'int spend = quoteOf(n, legPrice + tip, scale - 1);')],
  },
  {
    id: 'CB-rearm-proceeds-ceil',
    file: 'KobCondBid',
    test: T_MB,
    expect: ['NV3CB12'],
    note: 'repeat merge: the maker receives the CEIL of its proceeds (c = scale - 1)',
    edits: [rep('int proceeds = quoteOf(n, rptPrice, scale - 1);', 'int proceeds = quoteOf(n, rptPrice, 0);')],
  },
  {
    id: 'CB-update-parent',
    file: 'KobCondBid',
    test: T_MP,
    expect: ['NV3CB41'],
    note: 'update (arm / trail) may not run next to the repeat entry (OpCovInputCount(parent) == 0)',
    edits: [rxDel('    entry update(int ev, int tk) {', /^\s*require\(OpCovInputCount\(parent\) == 0\);$/m)],
  },
  {
    id: 'CB-merge-0x08',
    file: 'KobCondBid',
    run: { [T_MP]: ['NV3CB40'] },
    inputOnly: ['NV3CB40'],
    note: 'repeat merge: the entry pin\'s fill argument must start with the 8-byte push 0x08 (a non-minimal 9-byte push is refused)',
    edits: [del('            require(OpTxInputScriptSigSubstr(pin, 0, 1) == byte[](0x08));')],
  },
  // ---- KobIfdAsk
  {
    id: 'IA-price-tip',
    file: 'KobIfdAsk',
    test: T_TIP,
    expect: ['NV3IA20', 'NV3IA21'],
    note: 'the all-in price is at least the tip (price >= tip), so a booked exit\'s rptPrice = price - tip is never negative',
    edits: [del('                require(price >= tip);')],
  },
  {
    id: 'IA-minfill',
    file: 'KobIfdAsk',
    test: T_MF,
    expect: ['NV3IA01'],
    note: 'minFill: n >= minFill unless the fill sells out',
    edits: [del('                require(n >= minFill || outAmount == 0);')],
  },
  {
    id: 'IA-proceeds-ceil',
    file: 'KobIfdAsk',
    test: T_RND,
    expect: ['NV3IA10'],
    note: 'the exit receives the CEIL of the seller proceeds (c = scale - 1)',
    edits: [rep('int proceeds = quoteOf(n, p - tip, scale - 1);', 'int proceeds = quoteOf(n, p - tip, 0);')],
  },
  {
    id: 'IA-pre-ceil',
    file: 'KobIfdAsk',
    test: T_RND,
    expect: ['NV3IA11'],
    note: 'the exit receives the CEIL of the prefund (c = scale - 1; pre also sets the entry floor, so the attack keeps the sompi in the entry)',
    edits: [rep('int pre = quoteOf(n, prefund, scale - 1);', 'int pre = quoteOf(n, prefund, 0);')],
  },
  {
    id: 'IA-merge-prefund-ceil',
    file: 'KobIfdAsk',
    test: T_MB,
    expect: ['NV3IA12'],
    note: 'repeat merge floor: the entry keeps at least in + CEIL(m * prefund / scale)',
    edits: [rep('                floor = tx.inputs[self].value + quoteOf(m, prefund, scale - 1);', '                floor = tx.inputs[self].value + quoteOf(m, prefund, 0);')],
  },
  {
    id: 'IA-booking-mergek',
    file: 'KobIfdAsk',
    test: T_MP,
    expect: ['NV3IA31'],
    note: 'a booked exit\'s amount stays below MERGE_K (2^53), so it fits the merge argument without overflowing into k',
    edits: [rxDel('    entry settle(byte[8] nb, int tokenIn,', /^\s*require\(n < MERGE_K\);$/m)],
  },
  {
    id: 'IA-merge-0x08',
    file: 'KobIfdAsk',
    run: { [T_MP]: ['NV3IA40'] },
    inputOnly: ['NV3IA40'],
    note: 'repeat merge: the exit\'s fill argument (input k) must start with the 8-byte push 0x08 (a non-minimal 9-byte push is refused)',
    edits: [del('                require(OpTxInputScriptSigSubstr(k, 0, 1) == byte[](0x08));')],
  },
];
