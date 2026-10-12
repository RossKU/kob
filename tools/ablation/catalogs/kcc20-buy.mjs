// KCC-20 buy-side lifecycle suite (protocol v3, base units, no lot layouts): contracts/v2.
// Scenario ids follow the current test names (NRP* / NRPB*; new v3 attacks V3* / NV3*).
// Touch trigger: the T*-* entries (lib/touch.mjs generates one per evidence check; see README). The touch keys
// renamed in v3: `unit` -> `scale`, `units` -> `mintouch` (the evidence quotes per the same scale; n >= minTouch base units).
//
// Audit completion (2026-10-13): every `require` of KobCondBid and KobIfdAsk is an entry below (expect or hold; the AU-CB /
// AU-IA entries and the scenarios NAU-CB* / NAU-IA* of the v3_buy_audit_* test fns fill what the earlier entries left open)
// or one of the checks listed here. The unrolled stray scans have one entry per slot line (AU-CB-S<k>, AU-IA-S<k>: a stray of
// the order at token slot k on the 8/8 token, with the positive controls PAU-CB02 / PAU-IA01 of the full-slot shape), the
// scan bounds an input-only entry each (a 9th token input: the KCC-20 program refuses it on its own input, so only the order
// flips). Planted inputs (NAU-IA01 / IA02, CB14: a UTXO carrying the token covenant id that no token program created)
// follow the pair suite's NP25 / NP26: consensus cannot produce them, the orders refuse them anyway. Three checks are
// defence in depth across two templates; their hold scenario is judged at the other template's input (which still refuses
// it) and a combined entry removes both checks (`also`): KobCondBid update `scale > 0` (the evidence KobAsk refuses its own
// negative scale: AU-CB-34 / 34x), KobIfdAsk merge `scale > 0` (the merged exit refuses its own negative scale: AU-IA-13 /
// 13x) and merge `m > 0` (the exit refuses its own zero fill: AU-IA-14 / 14x). The exit spk pin and the exit genesis id of
// KobIfdAsk bind the same script (AU-IA-28 held, AU-IA-29 also flips the genesis attacks, AU-IA-28x removes both).
//
// Checks without a witness attack (not in the catalog):
//  - cancel `require(sb.length == 65)` (KobCondBid, KobIfdAsk): `sb[64]` of a shorter signature fails on its own, a longer
//    one is no Schnorr signature for checkSig.
//  - `require(refundTip >= 0)` (KobCondBid refund; KobIfdAsk refund settle(nb = 0) and close): its only use is
//    `tx.outputs[self].value + refundTip >= <escrow / carriers>`, so a negative refundTip only raises what the maker must
//    receive (the keeper takes less); no transaction becomes acceptable when the check is removed.
//  - KobCondBid update `require(side == SIDE_ASK)`: touch() returns SIDE_ASK or SIDE_BID only (SIDE_ASK when tk >= 0), so
//    the else branch of `side == SIDE_BID` always has side == SIDE_ASK.
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
// audit completion (2026-10-13): every require of KobCondBid / KobIfdAsk
const T_AU_CB = 'v3_buy_audit_cond_bid';
const T_AU_IA = 'v3_buy_audit_ifd_ask';

// The touch batteries run on the 8/8 variant (T_BS ... T_AA) and on the 3/3 reference program KCC20Ref, the tradable
// KCC-20 program (their `_3x3` twins): every run of a touch test fn below is expected on both programs. NTK16 (the
// custody of a second ask filled in the same transaction) needs more than 3 token inputs / outputs of one covenant and
// runs on 8/8 only.
const ON3 = Object.fromEntries([T_BS, T_BA, T_BT, T_AF, T_AA].map(t => [t, `${t}_3x3`]));
const NOT_ON3 = new Set(['NTK16']);
/** A run / hold map with each touch test fn's ids also expected on its 3/3 twin. */
function on3(run) {
  const o = { ...run };
  for (const [t, ids] of Object.entries(run)) {
    const k = ON3[t] ? ids.filter(id => !NOT_ON3.has(id)) : [];
    if (k.length) o[ON3[t]] = k;
  }
  return o;
}
/** touchMutations runs, every one on both programs. */
function on3Runs(runs) {
  return Object.fromEntries(
    Object.entries(runs).map(([c, r]) => [
      c,
      r.run || r.hold ? { ...r, ...(r.run && { run: on3(r.run) }), ...(r.hold && { hold: on3(r.hold) }) } : on3(r),
    ]),
  );
}

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
    note: "booking: the exit's rptUntil counts from the entry UTXO's DAA (its last activity), never from the filler's t (the cycle dated by t instead)",
    edits: [
      rep(`                    int idleEnd = OpTxInputDaaScore(self) + MAX_IDLE;
                    xUntil`, `                    int idleEnd = t + MAX_IDLE;
                    xUntil`),
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
    runs: on3Runs({
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
    }),
  }),
  {
    id: 'TB-rule',
    file: 'KobCondBid',
    run: on3({ [T_BS]: ['NTB19'], [T_NEG]: ['NB13'] }),
    note: 'settle, unarmed buy-stop leg: the evidence bid quotes >= stopPrice',
    edits: [rxDel('    entry settle(', /^\s*require\(rp >= stopPrice\);$/m)],
  },
  ...touchMutations({
    prefix: 'TV',
    file: 'KobCondBid',
    fn: 'function touch(int ev, int tk) : (int, int) {',
    what: 'touch (update: arm on a bid, trail on an ask)',
    runs: on3Runs({
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
      tktpl: { run: { [T_BT]: ['NTW02c'] }, inputOnly: ['NTW02c'] },
      tkp2sh: { run: { [T_BT]: ['NTW02d'] }, inputOnly: ['NTW02d'] },
    }),
  }),
  {
    id: 'TV-rule',
    file: 'KobCondBid',
    run: on3({ [T_BA]: ['NTV19'] }),
    note: 'update, arm: the evidence bid quotes >= stopPrice',
    edits: [rxDel('    entry update(int ev, int tk) {', /^\s*require\(rp >= stopPrice\);$/m)],
  },
  {
    id: 'TW-k',
    file: 'KobCondBid',
    run: on3({ [T_BT]: ['NTW19', 'NTW19b'] }),
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
    hold: on3({ [T_NEG]: ['NB24'], [T_BA]: ['NTV06'] }),
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
    run: on3({ [T_BA]: ['NTV23'], [T_BT]: ['NTW23'], [T_LC]: ['NS12'] }),
    note: 'update spends no token input owned by the order (bounded scan; the evidence fill brings foreign token inputs)',
    edits: [rxDel('    entry update(int ev, int tk) {', /^\s*noStrays\(selfId\);$/m)],
  },
  // ---- touch trigger (v2.6): KobIfdAsk touchAsk (sell-stop entry armed inside its fill or by update)
  ...touchMutations({
    prefix: 'TK',
    file: 'KobIfdAsk',
    fn: 'function touchAsk(int ev, int tk) : int {',
    what: 'touchAsk (sell-stop entry)',
    runs: on3Runs({
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
      tktpl: { run: { [T_AF]: ['NTK02c'], [T_AA]: ['NTL02c'] }, inputOnly: ['NTK02c', 'NTL02c'] },
      tkp2sh: { run: { [T_AF]: ['NTK02d'], [T_AA]: ['NTL02d'] }, inputOnly: ['NTK02d', 'NTL02d'] },
    }),
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
    run: on3({ [T_AF]: ['NTK19'] }),
    note: 'settle, unarmed stop entry: the evidence ask quotes <= entryStop',
    edits: [rx('    entry settle(', /require\(touchAsk\(ev, tk\) <= entryStop\);/, 'require(touchAsk(ev, tk) >= 0);')],
  },
  {
    id: 'TL-rule',
    file: 'KobIfdAsk',
    run: on3({ [T_AA]: ['NTL19'] }),
    note: 'update: the evidence ask quotes <= entryStop',
    edits: [rx('    entry update(int ev, int tk) {', /require\(touchAsk\(ev, tk\) <= entryStop\);/, 'require(touchAsk(ev, tk) >= 0);')],
  },
  {
    id: 'TL-own',
    file: 'KobIfdAsk',
    run: on3({ [T_AA]: ['NTL23'], [T_LC]: ['NI22'] }),
    note: 'update spends no token input owned by the entry (bounded scan; the evidence fill brings foreign token inputs)',
    edits: [del('        noStrays(selfId, -1);\n')],
  },
  // ---- update arms only an entry with a positive amount and its stop on the limit's side; a merge keeps an armed entry's band origin
  {
    id: 'TL-amount',
    file: 'KobIfdAsk',
    run: on3({ [T_AA]: ['NTL24'] }),
    note: 'update: the entry has a positive amount to fill (an empty repeating entry would pay keeperTip for a useless arm)',
    edits: [rxDel('    entry update(int ev, int tk) {', /^\s*require\(amountLeft > 0\);$/m)],
  },
  {
    id: 'TL-limit',
    file: 'KobIfdAsk',
    run: on3({ [T_AA]: ['NTL25'] }),
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
  {
    id: 'CB-rearm-mergek',
    file: 'KobCondBid',
    test: T_MP,
    expect: ['NV3CB43'],
    note: 're-arm: n < MERGE_K (2^53), so the merge argument -(self * 2^53 + n) names this input and n (a TP fill of 2^53 + m would read as the merge of the booked exit at self + 1)',
    edits: [del('            require(n < MERGE_K);\n')],
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
  {
    id: 'IA-merge-scale',
    file: 'KobIfdAsk',
    test: T_RPT,
    expect: ['NRPB36'],
    note: "repeat merge: the committed exit quotes at the entry's scale (an exit at scale 999 refused: the two would round the merge differently)",
    edits: [del('                require(int(byte[8](st.slice(151, 159))) == scale);\n')],
  },
  // ---- 2026-10-06: the refund of an empty (repeating) entry
  {
    id: 'IA-refund-held',
    file: 'KobIfdAsk',
    test: 'empty_repeating_ifda_refund_is_refused',
    expect: ['SR1b', 'SR1c'],
    note: 'refund (settle nb = 0) only while tokens are held: an empty repeating entry with a zero-amount custody would pin no output, so close() is its end',
    edits: [rxDel('    entry settle(byte[8] nb, int tokenIn,', /^\s*require\(amountLeft > 0\);$/m)],
  },

  // ================================================================ audit completion (2026-10-13)
  // ---- KobCondBid (every require; unwitnessable ones in the header)
{ id: "AU-CB-1", file: "KobCondBid", run: on3({ [T_BS]: ["NTB18"], [T_BA]: ["NTV18"], [T_BT]: ["NTW18"] }), note: "tplState: the evidence input is the P2SH of the redeem script it pushes (the generated TB-p2sh / TV-p2sh entries swap the call; this removes the check in the shared function)", edits: [del("        require(tx.inputs[idx].scriptPubKey == byte[](new ScriptPubKeyP2SHFromRedeemScript(rs)));\n")] },
  { id: "AU-CB-2", file: "KobCondBid", run: on3({ [T_BS]: ["NTB17"], [T_BA]: ["NTV17"], [T_BT]: ["NTW17"] }), note: "tplState: the evidence's template is the KobAsk / KobBid template (the check in the shared function)", edits: [del("        require(blake3(byte[](pre as byte[8]) + rs.slice(0, pre) + byte[](suf as byte[8]) + rs.slice(pre + stLen, size)) == tpl);\n")] },
  { id: "AU-CB-3", file: "KobCondBid", run: { [T_AU_CB]: ["NAU-CB30"] }, inputOnly: ["NAU-CB30"], note: "noStrays bound: at most MAX_TOK_IN = 8 token inputs (a 9th is not scanned; the KCC-20 program refuses a 9th input on its own, so only the order flips)", edits: [del("        require(cnt <= MAX_TOK_IN);\n")] },
  { id: "AU-CB-S0", file: "KobCondBid", run: { [T_AU_CB]: ["NAU-CBS0"] }, note: "noStrays scan slot 0: a stray of the order at token slot 0 rides to the seller", edits: [rep("require(byte[32](OpTxInputScriptSigSubstr(x0, e0, e0 + 32)) != me);", 'require(true);')] },
  { id: "AU-CB-S1", file: "KobCondBid", run: { [T_AU_CB]: ["NAU-CBS1"] }, note: "noStrays scan slot 1: a stray of the order at token slot 1 rides to the seller", edits: [rep("require(byte[32](OpTxInputScriptSigSubstr(x1, e1, e1 + 32)) != me);", 'require(true);')] },
  { id: "AU-CB-S2", file: "KobCondBid", run: { [T_AU_CB]: ["NAU-CBS2"] }, note: "noStrays scan slot 2: a stray of the order at token slot 2 rides to the seller", edits: [rep("require(byte[32](OpTxInputScriptSigSubstr(x2, e2, e2 + 32)) != me);", 'require(true);')] },
  { id: "AU-CB-S3", file: "KobCondBid", run: { [T_AU_CB]: ["NAU-CBS3"] }, note: "noStrays scan slot 3: a stray of the order at token slot 3 rides to the seller", edits: [rep("require(byte[32](OpTxInputScriptSigSubstr(x3, e3, e3 + 32)) != me);", 'require(true);')] },
  { id: "AU-CB-S4", file: "KobCondBid", run: { [T_AU_CB]: ["NAU-CBS4"] }, note: "noStrays scan slot 4: a stray of the order at token slot 4 rides to the seller", edits: [rep("require(byte[32](OpTxInputScriptSigSubstr(x4, e4, e4 + 32)) != me);", 'require(true);')] },
  { id: "AU-CB-S5", file: "KobCondBid", run: { [T_AU_CB]: ["NAU-CBS5"] }, note: "noStrays scan slot 5: a stray of the order at token slot 5 rides to the seller", edits: [rep("require(byte[32](OpTxInputScriptSigSubstr(x5, e5, e5 + 32)) != me);", 'require(true);')] },
  { id: "AU-CB-S6", file: "KobCondBid", run: { [T_AU_CB]: ["NAU-CBS6"] }, note: "noStrays scan slot 6: a stray of the order at token slot 6 rides to the seller", edits: [rep("require(byte[32](OpTxInputScriptSigSubstr(x6, e6, e6 + 32)) != me);", 'require(true);')] },
  { id: "AU-CB-S7", file: "KobCondBid", run: { [T_AU_CB]: ["NAU-CBS7"] }, note: "noStrays scan slot 7: a stray of the order at token slot 7 rides to the seller", edits: [rep("require(byte[32](OpTxInputScriptSigSubstr(x7, e7, e7 + 32)) != me);", 'require(true);')] },
  { id: "AU-CB-4", file: "KobCondBid", run: { [T_AU_CB]: ["NAU-CB01"] }, note: "settle: the order is the only input of its covenant id (a sibling UTXO of the id beside the fill)", edits: [rep("        int n = int(nb);\n        int self = this.activeInputIndex;\n        byte[32] selfId = OpInputCovenantId(self);\n        require(OpCovInputCount(selfId) == 1);\n", "        int n = int(nb);\n        int self = this.activeInputIndex;\n        byte[32] selfId = OpInputCovenantId(self);\n")] },
  { id: "AU-CB-5", file: "KobCondBid", run: { [T_AU_CB]: ["NAU-CB02"] }, note: "settle: no fill before activeFrom", edits: [rep("        noStrays(selfId);\n        require(tx.daa >= activeFrom);\n", "        noStrays(selfId);\n")] },
  { id: "AU-CB-6", file: "KobCondBid", run: { [T_AU_CB]: ["NAU-CB03"] }, note: "settle: n > 0 (with a maker-set minFill 0 the minFill rule admits n = 0: an empty delivery drains deliveryCarrier per call)", edits: [rep("        require(tx.daa >= activeFrom);\n        require(n > 0);\n", "        require(tx.daa >= activeFrom);\n")] },
  { id: "AU-CB-7", file: "KobCondBid", run: { [T_NEG]: ["NB14"] }, note: "settle: n <= amountLeft (a fill of 5 from an order of 4)", edits: [del("        require(n <= amountLeft);\n")] },
  { id: "AU-CB-8", file: "KobCondBid", run: { [T_AU_CB]: ["NAU-CB04"] }, note: "settle: scale > 0 (hostile field: at scale -1000 the spend turns negative, the seller funds the delivery)", edits: [rep("        require(n >= minFill || n == amountLeft);\n        require(scale > 0);\n", "        require(n >= minFill || n == amountLeft);\n")] },
  { id: "AU-CB-9", file: "KobCondBid", run: { [T_AU_CB]: ["NAU-CB05"] }, note: "settle: tip >= 0 (hostile field: a negative tip pays below the leg price)", edits: [del("        require(tip >= 0);\n")] },
  { id: "AU-CB-10", file: "KobCondBid", run: { [T_AU_CB]: ["NAU-CB06"] }, note: "settle: deliveryCarrier >= 0 (hostile field: the delivery carries nothing)", edits: [del("        require(deliveryCarrier >= 0);\n")] },
  { id: "AU-CB-11", file: "KobCondBid", run: { [T_NEG]: ["NB6"] }, note: "settle, limit leg: tpPrice > 0 (a stop-only order filled on its limit leg at price 0)", edits: [del("            require(tpPrice > 0);\n")] },
  { id: "AU-CB-12", file: "KobCondBid", run: { [T_AU_CB]: ["NAU-CB07"] }, note: "settle: leg is 0 or 1 (leg 2 would run as the stop leg)", edits: [del("            require(leg == 1);\n")] },
  { id: "AU-CB-13", file: "KobCondBid", run: { [T_AU_CB]: ["NAU-CB08"] }, note: "settle, stop leg: stopPrice > 0 (a limit-only order filled on a stop leg at price 0)", edits: [del("            require(stopPrice > 0);\n")] },
  { id: "AU-CB-14", file: "KobCondBid", run: { [T_AU_CB]: ["NAU-CB09"] }, note: "settle, stop leg: slipBps >= 0 (hostile field: a negative band)", edits: [del("            require(slipBps >= 0);\n")] },
  { id: "AU-CB-15", file: "KobCondBid", run: { [T_NEG]: ["NB26"] }, note: "settle, stop leg: slipBps <= 10000 (a band above 100%)", edits: [del("            require(slipBps <= 10000);\n")] },
  { id: "AU-CB-16", file: "KobCondBid", run: { [T_AU_CB]: ["NAU-CB10"] }, note: "settle, armed auction: tx.daa >= t (CLTV; a future t raises the buy price)", edits: [del("                    require(tx.daa >= t);\n")] },
  { id: "AU-CB-17", file: "KobCondBid", run: { [T_AU_CB]: ["NAU-CB11"] }, note: "settle, stop leg: stopPrice <= MAX_STOP (the band product stopPrice * 10000 fits; with band 0 a larger stop fills)", edits: [del("            require(stopPrice <= MAX_STOP);\n")] },
  { id: "AU-CB-18", file: "KobCondBid", run: { [T_AU_CB]: ["NAU-CB12"] }, note: "re-arm: output i pays the maker's profit to P2PK(maker)", edits: [del("            require(tx.outputs[self].scriptPubKey == byte[](new ScriptPubKeyP2PK(maker)));\n")] },
  { id: "AU-CB-19", file: "KobCondBid", run: { [T_AU_CB]: ["NAU-CB13"] }, note: "delivery: the template source input carries the token covenant id (same-program bytes of another covenant are hash-identical; the check pins the source)", edits: [del("            require(OpInputCovenantId(tokenTplIn) == tokenCovId);\n")] },
  { id: "AU-CB-20", file: "KobCondBid", run: { [T_AU_CB]: ["NAU-CB14"] }, note: "delivery: the template bytes hash to the token's template (a planted look-alike would give the maker an anyone-can-spend fake)", edits: [del("            require(blake3(byte[](tplPrefixLen as byte[8]) + tpre + byte[](tplSuffixLen as byte[8]) + tsuf) == tokenTplHash);\n")] },
  { id: "AU-CB-21", file: "KobCondBid", run: { [T_NEG]: ["NB18"] }, note: "delivery: output i is the holder of exactly n base units owned by the maker", edits: [del("            require(tx.outputs[self].scriptPubKey == byte[](new ScriptPubKeyP2SHFromRedeemScript(\n                tpre + byte[](0x08) + byte[](nb) + byte[](0x20) + byte[](byte[32](maker))\n                    + byte[](0x0100010020) + byte[](0 as byte[8]) + byte[](0 as byte[8]) + byte[](0 as byte[8]) + byte[](0 as byte[8])\n                    + byte[](0x20) + extensionCommitment + tsuf\n            )));\n")] },
  { id: "AU-CB-22", file: "KobCondBid", run: { [T_AU_CB]: ["NAU-CB15"] }, note: "delivery: bound to the token covenant (an unbound copy of the holder script delivers nothing)", edits: [del("            require(OpOutputCovenantId(self) == tokenCovId);\n")] },
  { id: "AU-CB-23", file: "KobCondBid", run: { [T_AU_CB]: ["NAU-CB16"] }, note: "partial fill: exactly one output bound to the order's id", edits: [del("            require(OpCovOutputCount(selfId) == 1);\n")] },
  { id: "AU-CB-24", file: "KobCondBid", run: { [T_NEG]: ["NB15","NB16"] }, note: "partial fill: the continuation is this script with amountLeft - n and the new armed state", edits: [del("            require(tx.outputs[selfOut].scriptPubKey == contSpk(stopPrice, amountLeft - n, newArmed));\n")] },
  { id: "AU-CB-25", file: "KobCondBid", run: { [T_NEG]: ["NB1","NB4"] }, note: "partial fill: the continuation keeps the escrow minus the spend and the delivery carrier", edits: [del("            require(tx.outputs[selfOut].value >= keep);\n")] },
  { id: "AU-CB-26", file: "KobCondBid", run: { [T_NEG]: ["NB2"] }, note: "partial fill: the delivery carries deliveryCarrier", edits: [del("                require(tx.outputs[self].value >= deliveryCarrier);\n")] },
  { id: "AU-CB-27", file: "KobCondBid", run: { [T_AU_CB]: ["NAU-CB17"] }, note: "termination: no output bound to the order's id", edits: [del("            require(OpCovOutputCount(selfId) == 0);\n")] },
  { id: "AU-CB-28", file: "KobCondBid", run: { [T_RND]: ["NV3CB10"] }, note: "termination: everything not spent rides on the delivery", edits: [del("                require(tx.outputs[self].value >= left);\n")] },
  { id: "AU-CB-29", file: "KobCondBid", run: { [T_AU_CB]: ["NAU-CB18"] }, note: "update: the order is the only input of its covenant id", edits: [rep("    entry update(int ev, int tk) {\n        int self = this.activeInputIndex;\n        byte[32] selfId = OpInputCovenantId(self);\n        require(OpCovInputCount(selfId) == 1);\n", "    entry update(int ev, int tk) {\n        int self = this.activeInputIndex;\n        byte[32] selfId = OpInputCovenantId(self);\n")] },
  { id: "AU-CB-30", file: "KobCondBid", run: { [T_AU_CB]: ["NAU-CB19"] }, note: "update: stopPrice > 0 (a limit-only order armed: the keeper is paid for nothing)", edits: [rep("        require(armed == 0);\n        require(stopPrice > 0);\n", "        require(armed == 0);\n")] },
  { id: "AU-CB-31", file: "KobCondBid", run: { [T_AU_CB]: ["NAU-CB20"] }, note: "update: not before activeFrom", edits: [rep("        require(keeperTip >= 0);\n        require(tx.daa >= activeFrom);\n", "        require(keeperTip >= 0);\n")] },
  { id: "AU-CB-32", file: "KobCondBid", run: { [T_AU_CB]: ["NAU-CB21"] }, note: "update: exactly one output bound to the order's id", edits: [rep("        }\n        require(OpCovOutputCount(selfId) == 1);\n", "        }\n")] },
  { id: "AU-CB-33", file: "KobCondBid", run: { [T_NEG]: ["NB21","NB21b"] }, note: "update: the continuation is this script with the justified stop and armed state", edits: [del("        require(tx.outputs[selfOut].scriptPubKey == contSpk(newStop, amountLeft, newArmed));\n")] },
  { id: "AU-CB-34", file: "KobCondBid", hold: { [T_AU_CB]: ["NAU-CB22"] }, note: "update: scale > 0. DEFENCE IN DEPTH: the evidence must quote at the same (negative) scale, and the evidence KobAsk refuses its own negative scale (NAU-CB22, judged at the evidence input); AU-CB-34x removes both", edits: [rep("        }\n        require(scale > 0);\n", "        }\n")] },
  { id: "AU-CB-34x", file: "KobCondBid", run: { [T_AU_CB]: ["NAU-CB22"] }, note: "update scale > 0 and the evidence KobAsk's scale > 0 removed together: at scale -1000 the keeper takes keeperTip out of the buying budget", edits: [rep("        }\n        require(scale > 0);\n", "        }\n")], also: [{ file: "KobAsk", edits: [del("            require(scale > 0);\n")] }] },
  { id: "AU-CB-35", file: "KobCondBid", run: { [T_NEG]: ["NB25"], [T_LC]: ["NB30","NB30b"] }, note: "update: the continuation keeps the buying budget (the keeper's tip only from the excess, at most keeperTip)", edits: [del("        require(tx.outputs[selfOut].value >= floorOut);\n")] },
  { id: "AU-CB-36", file: "KobCondBid", run: { [T_AU_CB]: ["NAU-CB23"] }, note: "cancel: SIGHASH_ALL only", edits: [del("        require(sb[64] == SIGHASH_ALL);\n")] },
  { id: "AU-CB-37", file: "KobCondBid", run: { [T_AU_CB]: ["NAU-CB24"] }, note: "cancel: the maker's signature", edits: [del("        require(checkSig(s, maker));\n")] },
  { id: "AU-CB-38", file: "KobCondBid", run: { [T_AU_CB]: ["NAU-CB25"] }, note: "refund only once due: min(expiryDaa, 90 days idle)", edits: [del("        require(tx.daa >= idleEnd);\n")] },
  { id: "AU-CB-39", file: "KobCondBid", run: { [T_AU_CB]: ["NAU-CB26"] }, note: "refund: the order is the only input of its covenant id", edits: [rep("        require(tx.daa >= idleEnd);\n        byte[32] selfId = OpInputCovenantId(self);\n        require(OpCovInputCount(selfId) == 1);\n", "        require(tx.daa >= idleEnd);\n        byte[32] selfId = OpInputCovenantId(self);\n")] },
  { id: "AU-CB-40", file: "KobCondBid", run: { [T_LC]: ["NS11"] }, note: "refund: no token input (strays owned by the id stay put)", edits: [del("        require(OpCovInputCount(tokenCovId) == 0);\n")] },
  { id: "AU-CB-41", file: "KobCondBid", run: { [T_AU_CB]: ["NAU-CB27"] }, note: "refund: no output bound to the order's id", edits: [rep("        require(OpCovInputCount(tokenCovId) == 0);\n        require(OpCovOutputCount(selfId) == 0);\n", "        require(OpCovInputCount(tokenCovId) == 0);\n")] },
  { id: "AU-CB-42", file: "KobCondBid", run: { [T_AU_CB]: ["NAU-CB28"] }, note: "refund: output i pays P2PK(maker)", edits: [rep("        require(refundTip >= 0);\n        require(tx.outputs[self].scriptPubKey == byte[](new ScriptPubKeyP2PK(maker)));\n", "        require(refundTip >= 0);\n")] },
  { id: "AU-CB-43", file: "KobCondBid", run: { [T_AU_CB]: ["NAU-CB29"] }, note: "refund: the maker gets the escrow minus at most refundTip", edits: [del("        require(tx.outputs[self].value + refundTip >= tx.inputs[self].value);\n")] },
  // ---- KobIfdAsk (every require; unwitnessable ones in the header)
  { id: "AU-IA-1", file: "KobIfdAsk", run: { [T_AU_IA]: ["NAU-IA01"] }, note: "tplRs: the custody input is the P2SH of the holder script it pushes (a planted UTXO of the token covenant id)", edits: [rep("    function tplRs(int idx, int pre, int stLen, int suf, byte[32] tpl) : byte[] {\n        int size = pre + stLen + suf;\n        int base = OpTxInputScriptSigLen(idx) - size;\n        byte[] rs = OpTxInputScriptSigSubstr(idx, base, base + size);\n        require(tx.inputs[idx].scriptPubKey == byte[](new ScriptPubKeyP2SHFromRedeemScript(rs)));\n", "    function tplRs(int idx, int pre, int stLen, int suf, byte[32] tpl) : byte[] {\n        int size = pre + stLen + suf;\n        int base = OpTxInputScriptSigLen(idx) - size;\n        byte[] rs = OpTxInputScriptSigSubstr(idx, base, base + size);\n")] },
  { id: "AU-IA-2", file: "KobIfdAsk", run: { [T_AU_IA]: ["NAU-IA02"] }, note: "tplRs: the custody's template is the token's (a planted P2SH of a look-alike template)", edits: [rep("    function tplRs(int idx, int pre, int stLen, int suf, byte[32] tpl) : byte[] {\n        int size = pre + stLen + suf;\n        int base = OpTxInputScriptSigLen(idx) - size;\n        byte[] rs = OpTxInputScriptSigSubstr(idx, base, base + size);\n        require(tx.inputs[idx].scriptPubKey == byte[](new ScriptPubKeyP2SHFromRedeemScript(rs)));\n        require(blake3(byte[](pre as byte[8]) + rs.slice(0, pre) + byte[](suf as byte[8]) + rs.slice(pre + stLen, size)) == tpl);\n", "    function tplRs(int idx, int pre, int stLen, int suf, byte[32] tpl) : byte[] {\n        int size = pre + stLen + suf;\n        int base = OpTxInputScriptSigLen(idx) - size;\n        byte[] rs = OpTxInputScriptSigSubstr(idx, base, base + size);\n        require(tx.inputs[idx].scriptPubKey == byte[](new ScriptPubKeyP2SHFromRedeemScript(rs)));\n")] },
  { id: "AU-IA-3", file: "KobIfdAsk", run: on3({ [T_AF]: ["NTK18"], [T_AA]: ["NTL18"] }), note: "tplState: the evidence input is the P2SH of the redeem script it pushes (the check in the shared function)", edits: [rep("    function tplState(int idx, int pre, int stLen, int suf, byte[32] tpl) : byte[] {\n        int size = pre + stLen + suf;\n        int base = OpTxInputScriptSigLen(idx) - size;\n        byte[] rs = OpTxInputScriptSigSubstr(idx, base, base + size);\n        require(tx.inputs[idx].scriptPubKey == byte[](new ScriptPubKeyP2SHFromRedeemScript(rs)));\n", "    function tplState(int idx, int pre, int stLen, int suf, byte[32] tpl) : byte[] {\n        int size = pre + stLen + suf;\n        int base = OpTxInputScriptSigLen(idx) - size;\n        byte[] rs = OpTxInputScriptSigSubstr(idx, base, base + size);\n")] },
  { id: "AU-IA-4", file: "KobIfdAsk", run: on3({ [T_AF]: ["NTK17"], [T_AA]: ["NTL17"] }), note: "tplState: the evidence's template is the KobAsk template (the check in the shared function)", edits: [rep("    function tplState(int idx, int pre, int stLen, int suf, byte[32] tpl) : byte[] {\n        int size = pre + stLen + suf;\n        int base = OpTxInputScriptSigLen(idx) - size;\n        byte[] rs = OpTxInputScriptSigSubstr(idx, base, base + size);\n        require(tx.inputs[idx].scriptPubKey == byte[](new ScriptPubKeyP2SHFromRedeemScript(rs)));\n        require(blake3(byte[](pre as byte[8]) + rs.slice(0, pre) + byte[](suf as byte[8]) + rs.slice(pre + stLen, size)) == tpl);\n", "    function tplState(int idx, int pre, int stLen, int suf, byte[32] tpl) : byte[] {\n        int size = pre + stLen + suf;\n        int base = OpTxInputScriptSigLen(idx) - size;\n        byte[] rs = OpTxInputScriptSigSubstr(idx, base, base + size);\n        require(tx.inputs[idx].scriptPubKey == byte[](new ScriptPubKeyP2SHFromRedeemScript(rs)));\n")] },
  { id: "AU-IA-5", file: "KobIfdAsk", run: { [T_AU_IA]: ["NAU-IA37"] }, inputOnly: ["NAU-IA37"], note: "noStrays bound: at most MAX_TOK_IN = 8 token inputs (a 9th is not scanned; the KCC-20 program refuses a 9th input on its own, so only the entry flips)", edits: [del("        require(cnt <= MAX_TOK_IN);\n")] },
  { id: "AU-IA-S0", file: "KobIfdAsk", run: { [T_AU_IA]: ["NAU-IAS0"] }, note: "noStrays scan slot 0: a stray of the entry at token slot 0 rides to the taker", edits: [rep("require(byte[32](OpTxInputScriptSigSubstr(x0, e0, e0 + 32)) != me);", 'require(true);')] },
  { id: "AU-IA-S1", file: "KobIfdAsk", run: { [T_AU_IA]: ["NAU-IAS1"] }, note: "noStrays scan slot 1: a stray of the entry at token slot 1 rides to the taker", edits: [rep("require(byte[32](OpTxInputScriptSigSubstr(x1, e1, e1 + 32)) != me);", 'require(true);')] },
  { id: "AU-IA-S2", file: "KobIfdAsk", run: { [T_AU_IA]: ["NAU-IAS2"] }, note: "noStrays scan slot 2: a stray of the entry at token slot 2 rides to the taker", edits: [rep("require(byte[32](OpTxInputScriptSigSubstr(x2, e2, e2 + 32)) != me);", 'require(true);')] },
  { id: "AU-IA-S3", file: "KobIfdAsk", run: { [T_AU_IA]: ["NAU-IAS3"] }, note: "noStrays scan slot 3: a stray of the entry at token slot 3 rides to the taker", edits: [rep("require(byte[32](OpTxInputScriptSigSubstr(x3, e3, e3 + 32)) != me);", 'require(true);')] },
  { id: "AU-IA-S4", file: "KobIfdAsk", run: { [T_AU_IA]: ["NAU-IAS4"] }, note: "noStrays scan slot 4: a stray of the entry at token slot 4 rides to the taker", edits: [rep("require(byte[32](OpTxInputScriptSigSubstr(x4, e4, e4 + 32)) != me);", 'require(true);')] },
  { id: "AU-IA-S5", file: "KobIfdAsk", run: { [T_AU_IA]: ["NAU-IAS5"] }, note: "noStrays scan slot 5: a stray of the entry at token slot 5 rides to the taker", edits: [rep("require(byte[32](OpTxInputScriptSigSubstr(x5, e5, e5 + 32)) != me);", 'require(true);')] },
  { id: "AU-IA-S6", file: "KobIfdAsk", run: { [T_AU_IA]: ["NAU-IAS6"] }, note: "noStrays scan slot 6: a stray of the entry at token slot 6 rides to the taker", edits: [rep("require(byte[32](OpTxInputScriptSigSubstr(x6, e6, e6 + 32)) != me);", 'require(true);')] },
  { id: "AU-IA-S7", file: "KobIfdAsk", run: { [T_AU_IA]: ["NAU-IAS7"] }, note: "noStrays scan slot 7: a stray of the entry at token slot 7 rides to the taker", edits: [rep("require(byte[32](OpTxInputScriptSigSubstr(x7, e7, e7 + 32)) != me);", 'require(true);')] },
  { id: "AU-IA-6", file: "KobIfdAsk", run: { [T_AU_IA]: ["NAU-IA03"] }, note: "settle: the entry is the only input of its covenant id", edits: [rep("        int n = int(nb);\n        int self = this.activeInputIndex;\n        byte[32] selfId = OpInputCovenantId(self);\n        require(OpCovInputCount(selfId) == 1);\n", "        int n = int(nb);\n        int self = this.activeInputIndex;\n        byte[32] selfId = OpInputCovenantId(self);\n")] },
  { id: "AU-IA-7", file: "KobIfdAsk", run: { [T_AU_IA]: ["NAU-IA04"] }, note: "settle: the custody carries the token covenant id (not a UTXO of another token of the same program owned by the entry)", edits: [del("        require(OpInputCovenantId(tokenIn) == tokenCovId);\n")] },
  { id: "AU-IA-8", file: "KobIfdAsk", run: { [T_AU_IA]: ["NAU-IA05"] }, note: "settle: the custody is owned by this entry's covenant id (not tokens of another id: the real custody would be orphaned)", edits: [del("            require(byte[32](tin.slice(10, 42)) == selfId);\n")] },
  { id: "AU-IA-9", file: "KobIfdAsk", run: { [T_AU_IA]: ["NAU-IA06"] }, note: "settle: the custody has owner scheme covenant id and borrowing disabled (0x040100)", edits: [del("            require(tin.slice(43, 46) == byte[](0x040100));\n")] },
  { id: "AU-IA-10", file: "KobIfdAsk", run: { [T_AU_IA]: ["NAU-IA07"] }, note: "settle: the custody carries the committed exit's extension commitment (another commitment is another token)", edits: [del("            require(ext == byte[32](byte[](exitState).slice(118, 150)));\n")] },
  { id: "AU-IA-11", file: "KobIfdAsk", run: { [T_AU_IA]: ["NAU-IA08"] }, note: "refund (nb = 0) only once due: min(expiryDaa, 90 days idle)", edits: [del("            require(tx.daa >= idleEnd);\n")] },
  { id: "AU-IA-12", file: "KobIfdAsk", run: { [T_AU_IA]: ["NAU-IA09"] }, note: "refund: the maker gets the carriers minus at most refundTip", edits: [del("            require(tx.outputs[self].value + refundTip >= carriers);\n")] },
  { id: "AU-IA-13", file: "KobIfdAsk", hold: { [T_AU_IA]: ["NAU-IA36"] }, note: "merge: scale > 0. DEFENCE IN DEPTH: the merged exit quotes at the entry scale (checked), and that KobCondBid refuses its own negative scale (NAU-IA36, judged at the exit input); AU-IA-13x removes both", edits: [rep("                // input k\n                require(scale > 0);\n", "                // input k\n")] },
  { id: "AU-IA-13x", file: "KobIfdAsk", run: { [T_AU_IA]: ["NAU-IA36"] }, note: "merge scale > 0 and the exit's scale > 0 removed together: an entry at scale -1000 merges its booked exit", edits: [rep("                // input k\n                require(scale > 0);\n", "                // input k\n")], also: [{ file: "KobCondBid", edits: [rep("        require(n >= minFill || n == amountLeft);\n        require(scale > 0);\n", "        require(n >= minFill || n == amountLeft);\n")] }] },
  { id: "AU-IA-14", file: "KobIfdAsk", hold: { [T_AU_IA]: ["NAU-IA10"] }, note: "merge: m > 0. DEFENCE IN DEPTH: m = 0 needs the exit at input k to settle n = 0, which the exit refuses (n > 0; NAU-IA10, judged at the exit input); AU-IA-14x removes both", edits: [del("                require(m > 0);\n")] },
  { id: "AU-IA-14x", file: "KobIfdAsk", run: { [T_AU_IA]: ["NAU-IA10"] }, note: "merge m > 0 and the exit's n > 0 removed together: a no-op merge of m = 0 (argument -(1 * 2^53))", edits: [del("                require(m > 0);\n")], also: [{ file: "KobCondBid", edits: [rep("        require(tx.daa >= activeFrom);\n        require(n > 0);\n", "        require(tx.daa >= activeFrom);\n")] }] },
  { id: "AU-IA-15", file: "KobIfdAsk", run: { [T_AU_IA]: ["NAU-IA11"] }, note: "merge: the exit terms slipBps .. minRestDaa (state [231..286)) are the committed exitState", edits: [del("                require(x.slice(231, 286) == st.slice(231, 286));\n")] },
  { id: "AU-IA-15b", file: "KobIfdAsk", run: { [T_AU_IA]: ["NAU-IA38"] }, note: "merge: the push opcode of the exit armed field (state 294) is the committed one (a look-alike exit pushing armed as OP_DATA_7 + OP_NOP runs)", edits: [del("                require(x.slice(294, 295) == st.slice(294, 295));\n")] },
  { id: "AU-IA-16", file: "KobIfdAsk", run: { [T_AU_IA]: ["NAU-IA12"] }, note: "merge: the exit terms bandDaa / keeperTip (state [303..321)) are the committed exitState", edits: [del("                require(x.slice(303, 321) == st.slice(303, 321));\n")] },
  { id: "AU-IA-17", file: "KobIfdAsk", run: { [T_AU_IA]: ["NAU-IA13"] }, note: "fill: no fill before activeFrom", edits: [del("                require(tx.daa >= activeFrom);\n")] },
  { id: "AU-IA-18", file: "KobIfdAsk", run: { [T_AU_IA]: ["NAU-IA14"] }, note: "fill: scale > 0 (hostile field: at scale -1000 the proceeds turn negative, the taker takes KAS from the entry)", edits: [rep("                require(tx.daa >= activeFrom);\n                require(scale > 0);\n", "                require(tx.daa >= activeFrom);\n")] },
  { id: "AU-IA-19", file: "KobIfdAsk", run: { [T_NEG]: ["NI12"] }, note: "fill: tip >= 0 (hostile field)", edits: [del("                require(tip >= 0);\n")] },
  { id: "AU-IA-20", file: "KobIfdAsk", run: { [T_AU_IA]: ["NAU-IA15"] }, note: "fill: prefund >= 0 (hostile field: the exit gets less than its proceeds)", edits: [del("                require(prefund >= 0);\n")] },
  { id: "AU-IA-21", file: "KobIfdAsk", run: { [T_AU_IA]: ["NAU-IA16"] }, note: "fill: exitCarrier >= 0 (hostile field)", edits: [del("                require(exitCarrier >= 0);\n")] },
  { id: "AU-IA-22", file: "KobIfdAsk", run: { [T_AU_IA]: ["NAU-IA17"] }, note: "fill: n <= amountLeft (11 whole sold from a 10-whole custody)", edits: [del("                require(n <= amountLeft);\n")] },
  { id: "AU-IA-23", file: "KobIfdAsk", run: { [T_LC]: ["NI21"] }, note: "fill: a stop entry has entryStop >= price", edits: [del("                    require(entryStop >= price);\n")] },
  { id: "AU-IA-24", file: "KobIfdAsk", run: { [T_AU_IA]: ["NAU-IA18"] }, note: "fill, armed auction: tx.daa >= t (CLTV; a future t lowers the sell price)", edits: [del("                            require(tx.daa >= t);\n")] },
  { id: "AU-IA-25", file: "KobIfdAsk", run: { [T_AU_IA]: ["NAU-IA19"] }, note: "fill, armed auction: t >= origin (a time before the origin is refused; it could only raise the price)", edits: [del("                            require(t >= newArmed);\n")] },
  { id: "AU-IA-26", file: "KobIfdAsk", run: { [T_RPT]: ["NRPB6b"] }, note: "booking: a fill takes at most rptAmount - 1 while a minimum fill of re-arms is left", edits: [del("                    require(rptAmount <= 1 || rptAmount - 1 < minFill);\n")] },
  { id: "AU-IA-27", file: "KobIfdAsk", run: { [T_AU_IA]: ["NAU-IA20"] }, hold: { [T_NEG]: ["NI10"] }, note: "the exit template hashes to COND_BID_TPL (an anyone-can-spend template around the exit state); holds NI10 (another template under a KobCondBid output is also refused by the spk pin)", edits: [del("                require(blake3(byte[](cPre.length as byte[8]) + cPre + byte[](cSuf.length as byte[8]) + cSuf) == COND_BID_TPL);\n")] },
  { id: "AU-IA-28", file: "KobIfdAsk", hold: { [T_NEG]: ["NI1","NI2","NI2b"] }, note: "the exit output is exactly the built exit script. DEFENCE IN DEPTH: the genesis id is recomputed from the built script too, so another script fails it; AU-IA-28x removes both", edits: [del("                require(tx.outputs[exitOut].scriptPubKey == exitSpk);\n")] },
  { id: "AU-IA-29", file: "KobIfdAsk", run: { [T_NEG]: ["NI3","NI3b","NI5"] }, hold: { [T_NEG]: ["NI1","NI2","NI2b"] }, note: "the exit is the fresh one-output genesis authorised by this input (a two-output group, another authoriser, a continuation of an attacker covenant); holds NI1 / NI2 / NI2b (the spk pin)", edits: [del("                require(OpOutputCovenantId(exitOut) == blake2bWithKey(\n                    byte[](OpOutpointTxId(self)) + byte[](OpOutpointIndex(self) as byte[4]) + byte[](1 as byte[8])\n                        + byte[](exitOut as byte[4]) + byte[](tx.outputs[exitOut].value as byte[8]) + exitSpk.slice(0, 2)\n                        + byte[](35 as byte[8]) + exitSpk.slice(2, 37),\n                    byte[](\"CovenantID\")\n                ));\n")] },
  { id: "AU-IA-28x", file: "KobIfdAsk", run: { [T_NEG]: ["NI1","NI2","NI2b"] }, note: "the exit spk pin and the genesis id removed together: a tampered exit (amountLeft 5 for 4, the stop removed, a wider band) is created", edits: [del("                require(tx.outputs[exitOut].scriptPubKey == exitSpk);\n"), del("                require(OpOutputCovenantId(exitOut) == blake2bWithKey(\n                    byte[](OpOutpointTxId(self)) + byte[](OpOutpointIndex(self) as byte[4]) + byte[](1 as byte[8])\n                        + byte[](exitOut as byte[4]) + byte[](tx.outputs[exitOut].value as byte[8]) + exitSpk.slice(0, 2)\n                        + byte[](35 as byte[8]) + exitSpk.slice(2, 37),\n                    byte[](\"CovenantID\")\n                ));\n")] },
  { id: "AU-IA-30", file: "KobIfdAsk", run: { [T_AU_IA]: ["NAU-IA21"] }, note: "partial fill: the rest custody keeps its carrier", edits: [del("                        require(tx.outputs[tokOut].value >= tx.inputs[tokenIn].value);\n")] },
  { id: "AU-IA-31", file: "KobIfdAsk", run: { [T_NEG]: ["NI6"], [T_RND]: ["NV3IA10"] }, note: "partial fill: the exit holds the proceeds, the prefund and exitCarrier", edits: [del("                    require(tx.outputs[exitOut].value >= proceeds + pre + exitCarrier);\n")] },
  { id: "AU-IA-32", file: "KobIfdAsk", run: { [T_NEG]: ["NI7"] }, note: "sold out: the last exit takes the proceeds and everything left", edits: [del("                    require(tx.outputs[exitOut].value >= proceeds + carriers);\n")] },
  { id: "AU-IA-33", file: "KobIfdAsk", run: { [T_AU_IA]: ["NAU-IA22"] }, note: "continuation: exactly one output bound to the entry's id", edits: [del("            require(OpCovOutputCount(selfId) == 1);\n")] },
  { id: "AU-IA-34", file: "KobIfdAsk", run: { [T_NEG]: ["NI8"] }, note: "continuation: holds at least its floor", edits: [del("            require(tx.outputs[c].value >= floor);\n")] },
  { id: "AU-IA-35", file: "KobIfdAsk", run: { [T_AU_IA]: ["NAU-IA23"] }, note: "termination: no output bound to the entry's id", edits: [del("            require(OpCovOutputCount(selfId) == 0);\n")] },
  { id: "AU-IA-36", file: "KobIfdAsk", run: { [T_NEG]: ["NI9"] }, note: "the one token output (rest, refund, merged custody) is pinned: the custody template with the expected state", edits: [del("            require(tx.outputs[outIdx].scriptPubKey == byte[](new ScriptPubKeyP2SHFromRedeemScript(\n                trs.slice(0, tplPrefixLen) + byte[](0x08) + byte[](outAmount as byte[8]) + byte[](0x20) + byte[](outOwner)\n                    + byte[](0x01) + byte[](outScheme) + byte[](0x010020) + guard + byte[](0x20) + ext\n                    + trs.slice(tplPrefixLen + 112, trs.length)\n            )));\n")] },
  { id: "AU-IA-37", file: "KobIfdAsk", run: { [T_AU_IA]: ["NAU-IA24"] }, note: "the pinned token output is bound to the token covenant (an unbound copy of the holder script would let the taker take the rest)", edits: [del("            require(OpOutputCovenantId(outIdx) == tokenCovId);\n")] },
  { id: "AU-IA-38", file: "KobIfdAsk", run: { [T_RPT]: ["NRPB27"] }, note: "close only once due: min(expiryDaa, 90 days idle)", edits: [rep("        if (expiryDaa < idleEnd) { idleEnd = expiryDaa; }\n        require(tx.daa >= idleEnd);\n", "        if (expiryDaa < idleEnd) { idleEnd = expiryDaa; }\n")] },
  { id: "AU-IA-39", file: "KobIfdAsk", run: { [T_AU_IA]: ["NAU-IA25"] }, note: "close: the entry is the only input of its covenant id", edits: [rep("        require(tx.daa >= idleEnd);\n        byte[32] selfId = OpInputCovenantId(self);\n        require(OpCovInputCount(selfId) == 1);\n", "        require(tx.daa >= idleEnd);\n        byte[32] selfId = OpInputCovenantId(self);\n")] },
  { id: "AU-IA-40", file: "KobIfdAsk", run: { [T_AU_IA]: ["NAU-IA26"] }, note: "close: no output bound to the entry's id", edits: [rep("        require(OpCovInputCount(tokenCovId) == 0);\n        require(OpCovOutputCount(selfId) == 0);\n", "        require(OpCovInputCount(tokenCovId) == 0);\n")] },
  { id: "AU-IA-41", file: "KobIfdAsk", run: { ['empty_repeating_ifda_refund_is_refused']: ["SR1e"] }, note: "close: output i pays P2PK(maker)", edits: [del("        require(tx.outputs[self].scriptPubKey == byte[](new ScriptPubKeyP2PK(maker)));\n")] },
  { id: "AU-IA-42", file: "KobIfdAsk", run: { [T_AU_IA]: ["NAU-IA27"] }, note: "close: the maker gets the carrier minus at most refundTip", edits: [del("        require(tx.outputs[self].value + refundTip >= tx.inputs[self].value);\n")] },
  { id: "AU-IA-43", file: "KobIfdAsk", run: { [T_AU_IA]: ["NAU-IA28"] }, note: "update: the entry is the only input of its covenant id", edits: [rep("    entry update(int ev, int tk) {\n        int self = this.activeInputIndex;\n        byte[32] selfId = OpInputCovenantId(self);\n        require(OpCovInputCount(selfId) == 1);\n", "    entry update(int ev, int tk) {\n        int self = this.activeInputIndex;\n        byte[32] selfId = OpInputCovenantId(self);\n")] },
  { id: "AU-IA-44", file: "KobIfdAsk", run: { [T_AU_IA]: ["NAU-IA29"] }, note: "update: entryStop > 0 (a limit entry at price 0 armed next to an ask fill quoting 0: the keeper is paid for nothing)", edits: [del("        require(entryStop > 0);\n")] },
  { id: "AU-IA-45", file: "KobIfdAsk", run: { [T_AU_IA]: ["NAU-IA30"] }, note: "update: only while unarmed (a re-arm pays the keeper again)", edits: [del("        require(armed == 0);\n")] },
  { id: "AU-IA-46", file: "KobIfdAsk", run: { [T_AU_IA]: ["NAU-IA31"] }, note: "update: not before activeFrom", edits: [rep("        require(keeperTip >= 0);\n        require(tx.daa >= activeFrom);\n", "        require(keeperTip >= 0);\n")] },
  { id: "AU-IA-47", file: "KobIfdAsk", run: { [T_AU_IA]: ["NAU-IA32"] }, note: "update: exactly one output bound to the entry's id", edits: [rep("        require(touchAsk(ev, tk) <= entryStop);\n        require(OpCovOutputCount(selfId) == 1);\n", "        require(touchAsk(ev, tk) <= entryStop);\n")] },
  { id: "AU-IA-48", file: "KobIfdAsk", run: { [T_AU_IA]: ["NAU-IA33"] }, note: "update: the continuation is this script, armed (not a repriced entry)", edits: [del("        require(tx.outputs[c].scriptPubKey == contSpk(1, amountLeft, rptAmount));\n")] },
  { id: "AU-IA-49", file: "KobIfdAsk", run: { [T_LC]: ["NI23"] }, note: "update: the keeper takes at most keeperTip", edits: [del("        require(tx.outputs[c].value >= tx.inputs[self].value - keeperTip);\n")] },
  { id: "AU-IA-50", file: "KobIfdAsk", run: { [T_AU_IA]: ["NAU-IA34"] }, note: "cancel: SIGHASH_ALL only", edits: [del("        require(sb[64] == SIGHASH_ALL);\n")] },
  { id: "AU-IA-51", file: "KobIfdAsk", run: { [T_AU_IA]: ["NAU-IA35"] }, note: "cancel: the maker's signature", edits: [del("        require(checkSig(s, maker));\n")] },
  { id: "AU-CB-44", file: "KobCondBid", run: { [T_AU_CB]: ["NAU-CB31"] }, note: "update: keeperTip >= 0 (hostile field: a negative keeperTip, its floor clamped to the escrow, would arm as keeperTip 0; as M4 / NK2 for KobCondAsk)", edits: [del("        require(keeperTip >= 0);\n")] },
  { id: "AU-IA-52", file: "KobIfdAsk", run: { [T_AU_IA]: ["NAU-IA39"] }, note: "update: keeperTip >= 0 (hostile field: the keeper tops the entry up; as M4 / NK2 for KobCondAsk)", edits: [del("        require(keeperTip >= 0);\n")] },
];
