// KCC-20 sell-first suite (protocol v3: amounts in base units, prices per whole token): contracts/v2.
// Ported from ablate.sh; scenario ids follow the current test names (NRP* / NRPB*, formerly NR* / NRB*).
// Touch trigger: the T*-* entries (lib/touch.mjs generates one per evidence check; see README).
// Protocol v3 checks (minFill, quoteOf rounding, price >= tip, the repeat merge pushes): the V-* entries, flipped
// by the V3* scenarios of the v3_* test fns.
import { del, rep, rx, rxDel } from '../lib/edits.mjs';
import { touchMutations } from '../lib/touch.mjs';

export const suite = {
  name: 'kcc20-sell',
  family: 'KCC-20 v2',
  testBin: 'kob_v2_tests',
  srcDir: 'contracts/v2',
  templateMarker: null,
  expectedTemplates: 1,
};

const T_RPT = 'v2_repeat_ifd_attacks';
// touch trigger batteries, one test fn per trigger kind
const T_SET = 'v2_touch_cond_settle';
const T_ARM = 'v2_touch_cond_arm';
const T_TRL = 'v2_touch_cond_trail';
const T_IF = 'v2_touch_ifd_fill';
const T_IA = 'v2_touch_ifd_arm';
const T_TM = 'v2_negative_trigger_manipulation';
const T_LC = 'v2_lifecycle_stop_auction_keeper_trailing';
const T_STRAY = 'v2_lifecycle_stray_custody';
const T_IFD = 'v2_lifecycle_ifd_stop_entry';
// protocol v3
const T_MIN = 'v3_min_fill';
const T_RND = 'v3_rounding';
const T_TIP = 'v3_tip';
const T_FOK = 'v3_fok';
const T_MRG = 'v3_merge_push';

export const mutations = [
  // ---- KobCondAsk
  {
    id: 'A1',
    file: 'KobCondAsk',
    test: T_RPT,
    expect: ['NRP10'],
    note: 'TP without the entry only from rptUntil',
    edits: [
      del('                        require(tx.daa >= rptUntil);'),
    ],
  },
  {
    id: 'A2',
    file: 'KobCondAsk',
    test: T_RPT,
    expect: ['NRP13'],
    note: 'stop leg refuses the entry (present == 0)',
    edits: [
      del('                    require(present == 0);'),
    ],
  },
  {
    id: 'A3',
    file: 'KobCondAsk',
    run: { [T_RPT]: ['NRP14'] },
    hold: { [T_RPT]: ['NRP28'], [T_MRG]: ['V3CA07'] },
    note: "entry's merge argument = -(own index * 2^53 + n) (NRP28: the entry's arming update starts with a minimal index push and is also refused by the 0x08 first-byte check, see V-CA-08x; V3CA07: an 8-byte negative ev in the entry's update is refused by the entry itself)",
    edits: [
      del('                require(int(byte[8](OpTxInputScriptSigSubstr(pin, 1, 9))) == 0 - (self * MERGE_K + n));'),
    ],
  },
  {
    id: 'A4',
    file: 'KobCondAsk',
    test: T_RPT,
    expect: ['NRP11'],
    note: 'maker payout floor (partial re-arming TP: maker >= proceeds - budget)',
    edits: [
      rep(`                require(tx.outputs[self].value >= allIn);
                outOwner = selfId;`, '                outOwner = selfId;'),
    ],
  },
  {
    id: 'A5',
    file: 'KobCondAsk',
    run: { [T_RPT]: ['NRP12'], [T_RND]: ['V3CA04'] },
    note: 'sell-out: entry gets the budget and the exit carriers',
    edits: [
      del('                    require(tx.outputs[OpCovOutputIdx(parent, 0)].value >= tx.inputs[pin].value + rptBudget + carriers);'),
    ],
  },
  // ---- KobIfdBid
  {
    id: 'B1',
    file: 'KobIfdBid',
    test: T_RPT,
    hold: { [T_RPT]: ['NRP21'] },
    note: 'exit is a genuine KobCondAsk (template + P2SH check) (the crafted input also fails the exit-terms comparison, confirmed on its own by FXL3a/b, so removing the template check alone holds)',
    edits: [
      rep('            byte[] x = tplState(k, COND_PRE, EXIT_STATE_LEN, COND_SUF, COND_TPL);', '            byte[] x = OpTxInputScriptSigSubstr(k, OpTxInputScriptSigLen(k) - EXIT_STATE_LEN - COND_SUF, OpTxInputScriptSigLen(k) - COND_SUF);'),
    ],
  },
  {
    id: 'B2',
    file: 'KobIfdBid',
    test: T_RPT,
    hold: { [T_RPT]: ['NRP20'] },
    note: "exit's parent = this entry (the parent check is part of the exit-tail comparison with rptPrice; a plain exit also fails on rptPrice, so removing the parent alone holds)",
    edits: [
      rep('byte[](0x20) + byte[](selfId) + byte[](0x08)', 'byte[](0x20) + x.slice(280, 312) + byte[](0x08)'),
    ],
  },
  {
    id: 'B3',
    file: 'KobIfdBid',
    run: { [T_MRG]: ['V3IB07'] },
    hold: { [T_RPT]: ['NRP22'] },
    note: "exit sells exactly m (its sigscript n; V3IB07: the exit refunds, n = 0) (NRP22: the exit's update starts with a minimal index push, refused by the 0x08 first-byte check)",
    edits: [
      del('            require(int(byte[8](OpTxInputScriptSigSubstr(k, 1, 9))) == m);'),
    ],
  },
  {
    id: 'B4',
    file: 'KobIfdBid',
    test: T_RPT,
    expect: ['NRP23'],
    note: 'm > 0',
    edits: [
      del('            require(m > 0);'),
    ],
  },
  {
    id: 'B5',
    file: 'KobIfdBid',
    test: T_RPT,
    expect: ['NRP17'],
    note: 'continuation SPK equality (fill/merge)',
    edits: [
      del('            require(tx.outputs[c].scriptPubKey == contSpk(newLeft, newArmed, newRpt));'),
    ],
  },
  {
    id: 'B5b',
    file: 'KobIfdBid',
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
    id: 'B6',
    file: 'KobIfdBid',
    test: T_RPT,
    expect: ['NRP15'],
    note: 'merge floor (the budget of m)',
    edits: [
      del('            floor = tx.inputs[self].value + quoteOf(m, rate, scale - 1);'),
    ],
  },
  {
    id: 'B7',
    file: 'KobIfdBid',
    test: T_RPT,
    expect: ['NRP24'],
    note: 'stray scan in merge (shares the fill guard)',
    edits: [
      rep(`        noStrays(selfId);
        require(scale > 0);`, '        require(scale > 0);'),
    ],
  },
  {
    id: 'B8',
    file: 'KobIfdBid',
    test: T_RPT,
    expect: ['NRP6'],
    note: 'booking time t <= lockTime (CLTV)',
    edits: [
      rep(`                require(tx.daa >= t);
                int utxoDaa`, '                int utxoDaa'),
    ],
  },
  {
    id: 'B9',
    file: 'KobIfdBid',
    test: T_RPT,
    expect: ['NRP7'],
    note: 'repeating entry never terminates on a fill',
    edits: [
      rep('            if (newLeft > 0 || rptAmount > 0) {', '            if (newLeft > 0) {'),
    ],
  },
  // ---- mutation controls (from mutate.sh, KCC-20 auction / keeperTip checks; the harness patching it needed is now KOB_ABLATION mode)
  {
    id: 'M2',
    file: 'KobCondAsk',
    test: T_LC,
    expect: ['NA3'],
    note: 'control: stop auction time >= arming origin (require(t >= newArmed))',
    edits: [del('require(t >= newArmed);')],
  },
  {
    id: 'M4',
    file: 'KobCondAsk',
    test: T_LC,
    expect: ['NK2'],
    note: 'control: keeperTip >= 0 on update',
    edits: [del('require(keeperTip >= 0);')],
  },
  // ---- fix pass (2026-09-30)
  {
    id: 'FXE1',
    file: 'KobAsk',
    test: 'v2_fix_pass_regressions',
    expect: ['FXE1', 'FXE2'],
    note: 'the IOC return is positional (tokOut == tokenIn): no shared remainder, no remainder that is also a bid delivery',
    edits: [del('                    require(tokOut == tokenIn);\n')],
  },
  {
    id: 'FXL3a',
    file: 'KobIfdBid',
    test: T_RPT,
    expect: ['NRP25', 'NRP27'],
    note: 'the merge requires the exit terms [0..181) (maker, legs) to be the committed exitState',
    edits: [del('            require(x.slice(0, 181) == st.slice(0, 181));\n')],
  },
  {
    id: 'FXL3b',
    file: 'KobIfdBid',
    test: T_RPT,
    expect: ['NRP26'],
    note: 'the merge requires parent = this entry and rptPrice = its budget rate (price + tip)',
    edits: [rxDel('int rate = price + tip;', /^\s*require\(x\.slice\(279, 322\) == [^\n]*\);$/m)],
  },
  // ---- touch trigger: KobCondAsk touchAsk (stop leg armed inside its fill), touch (update: arm / trail)
  ...touchMutations({
    prefix: 'TA',
    file: 'KobCondAsk',
    fn: 'function touchAsk(int ev, int tk) : int {',
    what: 'touchAsk (settle, stop leg)',
    runs: {
      push08: { [T_SET]: ['NTA10'] },
      npos: { [T_SET]: ['NTA11'] },
      tpl: { [T_SET]: ['NTA17'] },
      p2sh: { [T_SET]: ['NTA18'] },
      token: { [T_SET]: ['NTA07b'] },
      scale: { [T_SET]: ['NTA08'] },
      mintouch: { [T_SET]: ['NTA09'] },
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
    file: 'KobCondAsk',
    run: { [T_SET]: ['NTA19'], [T_TM]: ['NT3'] },
    note: 'settle, unarmed stop leg: the evidence ask quotes <= stopPrice',
    edits: [rxDel('    entry settle(', /^\s*require\(rp <= stopPrice\);$/m)],
  },
  ...touchMutations({
    prefix: 'TU',
    file: 'KobCondAsk',
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
    file: 'KobCondAsk',
    test: T_ARM,
    expect: ['NTU19'],
    note: 'update, arm: the evidence ask quotes <= stopPrice',
    edits: [rxDel('    entry update(int ev, int tk) {', /^\s*require\(rp <= stopPrice\);$/m)],
  },
  {
    id: 'TT-k',
    file: 'KobCondAsk',
    run: { [T_TRL]: ['NTT19', 'NTT19b'] },
    note: 'update, trail: the evidence bid justifies k >= 1 steps (rp >= stop + step + gap; k = 0 drains keeperTip and resets trailWait, k < 0 moves the stop down)',
    edits: [del('            require(k >= 1);\n')],
  },
  {
    id: 'TT-cap',
    file: 'KobCondAsk',
    test: T_LC,
    expect: ['NT18b'],
    note: 'update, trail: the ratchet is capped below tpPrice',
    edits: [del('                if (kt < k) { k = kt; }\n')],
  },
  {
    id: 'TT-wait',
    file: 'KobCondAsk',
    test: T_TM,
    expect: ['NT15'],
    note: 'update, trail: at most once per trailWait DAA (CSV)',
    edits: [del('            require(this.ageDaa >= trailWait);\n')],
  },
  {
    id: 'TT-step',
    file: 'KobCondAsk',
    hold: { [T_TM]: ['NT19'], [T_ARM]: ['NTU06'] },
    note: 'update, trail: trailStep > 0 (a non-trailing order divides by trailStep = 0 and fails anyway)',
    edits: [del('            require(trailStep > 0);\n')],
  },
  {
    id: 'TU-armed',
    file: 'KobCondAsk',
    test: T_TM,
    expect: ['NT14'],
    note: 'update only while unarmed',
    edits: [rxDel('    entry update(int ev, int tk) {', /^\s*require\(armed == 0\);$/m)],
  },
  {
    id: 'TU-own',
    file: 'KobCondAsk',
    run: { [T_ARM]: ['NTU23'], [T_TRL]: ['NTT23'], [T_TM]: ['NT13'] },
    note: 'update spends no token input owned by the order (bounded scan; the evidence fill brings foreign token inputs)',
    edits: [del('        noStrays(selfId, -1);\n')],
  },
  // ---- touch trigger: KobIfdBid touchBid (buy-stop entry armed inside its fill or by update)
  ...touchMutations({
    prefix: 'TI',
    file: 'KobIfdBid',
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
    file: 'KobIfdBid',
    test: T_IFD,
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
    file: 'KobIfdBid',
    test: T_IF,
    expect: ['NTI19'],
    note: 'fill, unarmed stop entry: the evidence bid quotes >= entryStop',
    edits: [rx('    entry fill(', /require\(touchBid\(ev\) >= entryStop\);/, 'require(touchBid(ev) >= 0);')],
  },
  {
    id: 'TJ-rule',
    file: 'KobIfdBid',
    test: T_IA,
    expect: ['NTJ19'],
    note: 'update: the evidence bid quotes >= entryStop',
    edits: [rx('    entry update(int ev) {', /require\(touchBid\(ev\) >= entryStop\);/, 'require(touchBid(ev) >= 0);')],
  },
  {
    id: 'TJ-own',
    file: 'KobIfdBid',
    test: T_IA,
    expect: ['NTJ23'],
    note: 'update spends no token input owned by the entry (bounded scan; the evidence fill brings foreign token inputs)',
    edits: [rxDel('    entry update(int ev) {', /^\s*noStrays\(selfId\);$/m)],
  },
  // ---- update arms only an entry with an amount to buy and its stop on the limit's side; a merge keeps an armed entry's band origin
  {
    id: 'TJ-amount',
    file: 'KobIfdBid',
    test: T_IA,
    expect: ['NTJ24'],
    note: 'update: the entry has an amount to fill (an empty repeating entry would pay keeperTip for a useless arm)',
    edits: [rxDel('    entry update(int ev) {', /^\s*require\(amountLeft > 0\);$/m)],
  },
  {
    id: 'TJ-limit',
    file: 'KobIfdBid',
    test: T_IA,
    expect: ['NTJ25'],
    note: "update: the stop is on the limit's side (entryStop <= price; else no fill can follow the arm)",
    edits: [rxDel('    entry update(int ev) {', /^\s*require\(entryStop <= price\);$/m)],
  },
  {
    id: 'TJ-origin',
    file: 'KobIfdBid',
    test: T_RPT,
    expect: ['NRP29'],
    note: 'merge into an entry armed by update (armed 1, band): the continuation records the band origin (else the auction restarts)',
    edits: [rep('if (armed == 1 && bandDaa > 0) {', 'if (armed == 1 && bandDaa < 0) {')],
  },

  // ================================================================ protocol v3 checks
  // ---- KobAsk
  {
    id: 'V-A-minfill',
    file: 'KobAsk',
    test: T_MIN,
    expect: ['V3A01'],
    note: 'minFill: n >= minFill unless the fill takes everything left',
    edits: [del('            require(n >= minFill || n == amountLeft);\n')],
  },
  {
    id: 'V-A-ceil',
    file: 'KobAsk',
    test: T_RND,
    expect: ['V3A02'],
    note: "the maker's proceeds are rounded up (quoteOf c = scale - 1 -> 0)",
    edits: [rep('int allIn = quoteOf(n, p - tip, scale - 1);', 'int allIn = quoteOf(n, p - tip, 0);')],
  },
  {
    id: 'V-A-tip',
    file: 'KobAsk',
    test: T_TIP,
    expect: ['V3A03', 'V3A04'],
    note: 'the price (also a decayed one) covers the tip: p >= tip (else the all-in proceeds are negative)',
    edits: [del('            require(p >= tip);\n')],
  },
  {
    id: 'V-A-custody',
    file: 'KobAsk',
    test: T_STRAY,
    expect: ['NS2'],
    note: 'the custody holds exactly amountLeft base units (a dust UTXO sent to the ask cannot stand in for it; NS1 is also refused by the stray scan)',
    edits: [del('        require(tinAmount == amountLeft);\n')],
  },
  // ---- KobBid
  {
    id: 'V-B-minfill',
    file: 'KobBid',
    test: T_MIN,
    expect: ['V3B01'],
    note: 'minFill: n >= minFill unless the bid ends (less than one minimum fill of buying power left)',
    edits: [del('        require(n >= minFill || !canContinue);\n')],
  },
  {
    id: 'V-B-minpos',
    file: 'KobBid',
    test: T_MIN,
    expect: ['V3B02'],
    note: 'minFill > 0 (a zero minimum would let fills of one base unit drain the escrow into delivery carriers)',
    edits: [del('        require(minFill > 0);\n')],
  },
  {
    id: 'V-B-floor',
    file: 'KobBid',
    test: T_RND,
    expect: ['V3B03', 'V3B03b'],
    note: "the maker's spend is rounded down (quoteOf c = 0 -> scale - 1)",
    edits: [rep('int allIn = quoteOf(n, p + tip, 0);', 'int allIn = quoteOf(n, p + tip, scale - 1);')],
  },
  {
    id: 'V-B-ceil',
    file: 'KobBid',
    test: T_RND,
    expect: ['V3B04'],
    note: 'the escrow budget a fill consumes is rounded up (quoteOf c = scale - 1 -> 0): split fills never buy more than the escrow funds',
    edits: [rep('int used = quoteOf(n, pMax + tip, scale - 1);', 'int used = quoteOf(n, pMax + tip, 0);')],
  },
  {
    id: 'V-B-fok',
    file: 'KobBid',
    test: T_FOK,
    expect: ['V3B06'],
    note: 'FOK: the fill leaves less buying power than one minimum fill',
    edits: [del('            require(tif != TIF_FOK || !canContinue);\n')],
  },
  // ---- KobCondAsk
  {
    id: 'V-CA-minfill',
    file: 'KobCondAsk',
    test: T_MIN,
    expect: ['V3CA01'],
    note: 'minFill: n >= minFill unless the fill takes everything left',
    edits: [del('            require(n >= minFill || n == amountLeft);\n')],
  },
  {
    id: 'V-CA-ceil',
    file: 'KobCondAsk',
    test: T_RND,
    expect: ['V3CA02'],
    note: "the maker's proceeds are rounded up (quoteOf c = scale - 1 -> 0)",
    edits: [rep('int allIn = quoteOf(n, legPrice - tip, scale - 1);', 'int allIn = quoteOf(n, legPrice - tip, 0);')],
  },
  {
    id: 'V-CA-tip',
    file: 'KobCondAsk',
    test: T_TIP,
    expect: ['V3CA03a', 'V3CA03b', 'V3CA03c'],
    note: 'the leg price covers the tip: legPrice >= tip (TP leg, stop leg, stop auction)',
    edits: [del('            require(legPrice >= tip);\n')],
  },
  {
    id: 'V-CA-rb',
    file: 'KobCondAsk',
    test: T_RND,
    expect: ['V3CA04'],
    note: 'repeat merge: the budget returned to the entry is rounded up (quoteOf c = scale - 1 -> 0; the entry rounds the same way)',
    edits: [rep('rptBudget = quoteOf(n, rptPrice, scale - 1);', 'rptBudget = quoteOf(n, rptPrice, 0);')],
  },
  {
    id: 'V-CA-08',
    file: 'KobCondAsk',
    run: { [T_MRG]: ['V3CA08'] },
    inputOnly: ['V3CA08'],
    hold: { [T_RPT]: ['NRP28'], [T_MRG]: ['V3CA07'] },
    note: "repeat merge: the entry's sigscript starts with 0x08 (V3CA08: the merge argument as a 9-byte push, bytes [1..9) = the merge value; the entry refuses its own 9-byte argument, so the exit's flip is input-only) (NRP28, a minimal index push, is also refused by the merge-value comparison; V3CA07, a negative 8-byte index, by the entry's own update; V-CA-08x removes both)",
    edits: [del('                require(OpTxInputScriptSigSubstr(pin, 0, 1) == byte[](0x08));\n')],
  },
  {
    id: 'V-CA-08x',
    file: 'KobCondAsk',
    test: T_RPT,
    expect: ['NRP28'],
    note: "repeat merge: the 0x08 first byte and the merge value of the entry's sigscript removed together (the entry's arming update read as a merge)",
    edits: [
      del('                require(OpTxInputScriptSigSubstr(pin, 0, 1) == byte[](0x08));\n'),
      del('                require(int(byte[8](OpTxInputScriptSigSubstr(pin, 1, 9))) == 0 - (self * MERGE_K + n));'),
    ],
  },
  {
    id: 'V-CA-parent',
    file: 'KobCondAsk',
    run: { [T_MRG]: ['V3CA05'] },
    hold: { [T_RPT]: ['NRP22'] },
    note: "update never next to the repeat entry (OpCovInputCount(parent) == 0): an 8-byte ev push would alias the merge amount (NRP22, a minimal push, is also refused by the entry's 0x08 check)",
    edits: [del('        require(OpCovInputCount(parent) == 0);\n')],
  },
  // ---- KobIfdBid
  {
    id: 'V-IB-minfill',
    file: 'KobIfdBid',
    run: { [T_MIN]: ['V3IB01'], [T_IFD]: ['NI18'] },
    note: 'minFill: n >= minFill unless the fill takes everything left',
    edits: [del('            require(n >= minFill || n == amountLeft);\n')],
  },
  {
    id: 'V-IB-floor',
    file: 'KobIfdBid',
    test: T_RND,
    expect: ['V3IB02', 'V3IB02b'],
    note: "the maker's spend is rounded down (quoteOf c = 0 -> scale - 1)",
    edits: [rep('int spend = quoteOf(n, p + tip, 0);', 'int spend = quoteOf(n, p + tip, scale - 1);')],
  },
  {
    id: 'V-IB-mergek',
    file: 'KobIfdBid',
    test: T_MRG,
    expect: ['V3IB03'],
    note: 'booking: a booked exit amount is below 2^53 (the merge argument -(k * 2^53 + m) must name it)',
    edits: [del('                require(n < MERGE_K);\n')],
  },
  {
    id: 'V-IB-mceil',
    file: 'KobIfdBid',
    test: T_RND,
    expect: ['V3IB05'],
    note: 'merge floor: the budget of m is rounded up (quoteOf c = scale - 1 -> 0; the exit rounds the same way)',
    edits: [rep('floor = tx.inputs[self].value + quoteOf(m, rate, scale - 1);', 'floor = tx.inputs[self].value + quoteOf(m, rate, 0);')],
  },
  {
    id: 'V-IB-08',
    file: 'KobIfdBid',
    run: { [T_MRG]: ['V3IB06'] },
    hold: { [T_RPT]: ['NRP22'] },
    note: "merge: the exit's sigscript starts with 0x08 (a refund whose nb = 0 is pushed with OP_PUSHDATA1 reads 8 at [1..9)) (NRP22: the exit's minimal update push also fails the amount comparison)",
    edits: [del('            require(OpTxInputScriptSigSubstr(k, 0, 1) == byte[](0x08));\n')],
  },
];
