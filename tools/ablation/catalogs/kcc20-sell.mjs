// KCC-20 sell-first suite (protocol v3: amounts in base units, prices per whole token): contracts/v2.
// Ported from ablate.sh; scenario ids follow the current test names (NRP* / NRPB*, formerly NR* / NRB*).
// Touch trigger: the T*-* entries (lib/touch.mjs generates one per evidence check; see README).
// Protocol v3 checks (minFill, quoteOf rounding, price >= tip, the repeat merge pushes): the V-* entries, flipped
// by the V3* scenarios of the v3_* test fns.
//
// Audit completion (2026-10-13): every `require` of KobAsk, KobBid, KobCondAsk and KobIfdBid is an entry below (expect or
// hold; the AU-* entries and the scenarios NAU-* of the v3_audit_* test fns fill what the earlier entries left open) or one of
// the checks listed here. The unrolled stray scans have one entry per slot line (AU-A-S<k>, AU-B-S<k>, AU-IB-S<k>: a stray
// of the order at token slot k on the 8/8 token; AU-CA-S<k> likewise), the scan bounds an
// input-only entry each (a 9th token input: the KCC-20 program refuses it on its own input, so only the order flips).
// Planted inputs (NAU-A03 / A04, CA03 / CA04, B13, IB13: a UTXO carrying the token covenant id that no token program
// created) follow the pair suite's NP25 / NP26: consensus cannot produce them, the orders refuse them anyway.
// Defence in depth (hold + combined): fill n > 0 and the minFill rule (AU-A-13 / 13x, AU-CA-15 / 15x); the IFD exit
// script equality and its genesis id, the covenant id being derived from the script (AU-IB-20 / 20x); the exit template
// hash holds NI4 (also refused by the script equality) and flips NAU-IB34, a look-alike exit template.
//
// Checks without a witness attack (not in the catalog):
//  - cancel `require(sb.length == 65)` (KobAsk, KobBid, KobCondAsk, KobIfdBid): `sb[64]` of a shorter signature fails on its
//    own, a longer one is no Schnorr signature for checkSig.
//  - refund `require(refundTip >= 0)` (KobAsk, KobBid, KobCondAsk, KobIfdBid): its only use is
//    `tx.outputs[self].value + refundTip >= <carriers / escrow>`, so a negative refundTip only raises what the maker must
//    receive; no transaction becomes acceptable when the check is removed.
//  - KobCondAsk update `require(side == SIDE_BID)`: touch() returns SIDE_ASK or SIDE_BID only (SIDE_BID unless tk >= 0), so
//    the else branch of `side == SIDE_ASK` always has side == SIDE_BID.
import { del, rep, rx, rxDel } from '../lib/edits.mjs';
import { touchMutations } from '../lib/touch.mjs';
import { withPublicMint } from '../lib/pm.mjs';

export const suite = {
  name: 'kcc20-sell',
  family: 'KCC-20 v2',
  testBin: 'kob_v2_tests',
  srcDir: 'contracts/v2',
  templateMarker: null,
  expectedTemplates: 1,
};

const T_RPT = 'v2_repeat_ifd_attacks';
// AUD-EXT (closed by the template version after the audit): the evidence of another extension commitment, in the
// audit_pair_sec_tests binary (the KAS-quoted stops on the 8/8, 3/3 and public-mint programs)
const T_AUD = 'audit_pair_sec_tests::aud_ext_kas_stops_refuse_evidence_of_another_extension_commitment';
// touch trigger batteries, one test fn per trigger kind
const T_SET = 'v2_touch_cond_settle';
const T_ARM = 'v2_touch_cond_arm';
const T_TRL = 'v2_touch_cond_trail';
const T_IF = 'v2_touch_ifd_fill';
const T_IA = 'v2_touch_ifd_arm';
// Every touch battery runs on the 8/8 program (the test fns above) and on the reference 3/3 program (the same fn
// with _3x3). on3x3 adds the 3/3 twin of every battery run of a touchMutations spec, without the scenarios whose
// shape the 3/3 program refuses (SKIPPED in its output: NTA16 needs 3 token inputs and 4 token outputs).
const BATTERIES = new Set([T_SET, T_ARM, T_TRL, T_IF, T_IA]);
const NOT_ON_3x3 = new Set(['NTA16']);
function with3x3(run) {
  const out = { ...run };
  for (const [t, ids] of Object.entries(run)) {
    const k = BATTERIES.has(t) ? ids.filter(id => !NOT_ON_3x3.has(id)) : [];
    if (k.length) out[`${t}_3x3`] = k;
  }
  return out;
}
function on3x3(runs) {
  return Object.fromEntries(
    Object.entries(runs).map(([check, r]) => [
      check,
      r.run || r.hold ? { ...r, ...(r.run && { run: with3x3(r.run) }), ...(r.hold && { hold: with3x3(r.hold) }) } : with3x3(r),
    ]),
  );
}
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
// audit completion (2026-10-13): every require of the four templates
const T_AK = 'v2_lifecycle_auctions_and_kill';
const T_TCE = 'v2_negative_tif_cancel_expiry';
const T_LIM = 'v2_negative_limits_and_tips';
const T_TV = 'v2_negative_time_variants';
const T_AU_A = 'v3_audit_ask';
const T_AU_B = 'v3_audit_bid';
const T_AU_CA = 'v3_audit_cond_ask';
const T_AU_CAS = 'v3_audit_cond_ask_strays';
const T_AU_IB = 'v3_audit_ifd_bid';
const T_OI = 'v2_negative_oco_and_ifd';

export const mutations = withPublicMint([
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
  { id: "AU-CA-44", file: "KobCondAsk", run: { [T_AU_CAS]: ["NAU-CA39"] }, inputOnly: ["NAU-CA39"], note: "noStrays bound: at most MAX_TOK_IN = 8 token inputs (a 9th is not scanned; the KCC-20 program refuses a 9th input on its own, so only the order flips)", edits: [del("        require(cnt <= MAX_TOK_IN);\n")] },
  { id: "AU-CA-S0", file: "KobCondAsk", run: { [T_AU_CAS]: ["NAU-CAS0"] }, note: "noStrays scan slot 0: a stray of the conditional at token slot 0 rides to the taker", edits: [rep("require(byte[32](OpTxInputScriptSigSubstr(x0, e0, e0 + 32)) != me);", 'require(true);')] },
  { id: "AU-CA-S1", file: "KobCondAsk", run: { [T_AU_CAS]: ["NAU-CAS1"] }, note: "noStrays scan slot 1: a stray of the conditional at token slot 1 rides to the taker", edits: [rep("require(byte[32](OpTxInputScriptSigSubstr(x1, e1, e1 + 32)) != me);", 'require(true);')] },
  { id: "AU-CA-S2", file: "KobCondAsk", run: { [T_AU_CAS]: ["NAU-CAS2"] }, note: "noStrays scan slot 2: a stray of the conditional at token slot 2 rides to the taker", edits: [rep("require(byte[32](OpTxInputScriptSigSubstr(x2, e2, e2 + 32)) != me);", 'require(true);')] },
  { id: "AU-CA-S3", file: "KobCondAsk", run: { [T_AU_CAS]: ["NAU-CAS3"] }, note: "noStrays scan slot 3: a stray of the conditional at token slot 3 rides to the taker", edits: [rep("require(byte[32](OpTxInputScriptSigSubstr(x3, e3, e3 + 32)) != me);", 'require(true);')] },
  { id: "AU-CA-S4", file: "KobCondAsk", run: { [T_AU_CAS]: ["NAU-CAS4"] }, note: "noStrays scan slot 4: a stray of the conditional at token slot 4 rides to the taker", edits: [rep("require(byte[32](OpTxInputScriptSigSubstr(x4, e4, e4 + 32)) != me);", 'require(true);')] },
  { id: "AU-CA-S5", file: "KobCondAsk", run: { [T_AU_CAS]: ["NAU-CAS5"] }, note: "noStrays scan slot 5: a stray of the conditional at token slot 5 rides to the taker", edits: [rep("require(byte[32](OpTxInputScriptSigSubstr(x5, e5, e5 + 32)) != me);", 'require(true);')] },
  { id: "AU-CA-S6", file: "KobCondAsk", run: { [T_AU_CAS]: ["NAU-CAS6"] }, note: "noStrays scan slot 6: a stray of the conditional at token slot 6 rides to the taker", edits: [rep("require(byte[32](OpTxInputScriptSigSubstr(x6, e6, e6 + 32)) != me);", 'require(true);')] },
  { id: "AU-CA-S7", file: "KobCondAsk", run: { [T_AU_CAS]: ["NAU-CAS7"] }, note: "noStrays scan slot 7: a stray of the conditional at token slot 7 rides to the taker", edits: [rep("require(byte[32](OpTxInputScriptSigSubstr(x7, e7, e7 + 32)) != me);", 'require(true);')] },
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
    note: "booking: the exit's rptUntil counts from the entry UTXO's DAA (its last activity), never from the filler's t (the cycle dated by t instead)",
    edits: [
      rep(`                int idleEnd = OpTxInputDaaScore(self) + MAX_IDLE;
                xUntil`, `                int idleEnd = t + MAX_IDLE;
                xUntil`),
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
  {
    id: 'V-IB-scale',
    file: 'KobIfdBid',
    test: T_RPT,
    expect: ['NRP30'],
    note: "merge: the committed exit (exitState, compared with the exit's state) quotes at the entry's scale (else the exit returns budget(m) at its scale and the entry re-arms m at its own)",
    edits: [del('            require(int(byte[8](st.slice(118, 126))) == scale);\n')],
  },
  // ---- touch trigger: KobCondAsk touchAsk (stop leg armed inside its fill), touch (update: arm / trail)
  ...touchMutations({
    prefix: 'TA',
    file: 'KobCondAsk',
    fn: 'function touchAsk(int ev, int tk) : int {',
    what: 'touchAsk (settle, stop leg)',
    runs: on3x3({
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
      // the look-alike holder / non-P2SH input is also refused by the evidence ask's stray guard and the token program
      tktpl: { run: { [T_SET]: ['NTA02c'] }, inputOnly: ['NTA02c'] },
      tkp2sh: { run: { [T_SET]: ['NTA02d'] }, inputOnly: ['NTA02d'] },
      // AUD-EXT: only the evidence ask and its custody trade the other commitment (the token program refuses the mix)
      ext: { run: { [T_AUD]: ['NAUD-EXT-CA3'] }, inputOnly: ['NAUD-EXT-CA3'] },
    }),
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
    runs: on3x3({
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
      // AUD-EXT: an update next to fills of units of another commitment (every input accepts it without the check)
      ext: { [T_AUD]: ['NAUD-EXT-CA1'] },
      bidext: { [T_AUD]: ['NAUD-EXT-CA2'] },
    }),
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
    runs: on3x3({
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
      // AUD-EXT: in the entry's fill only the evidence bid moves (the token program refuses the mix)
      bidext: { run: { [T_AUD]: ['NAUD-EXT-IB1', 'NAUD-EXT-IB2'] }, inputOnly: ['NAUD-EXT-IB2'] },
    }),
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
  {
    id: 'V-CA-mergek',
    file: 'KobCondAsk',
    test: T_MRG,
    expect: ['V3CA09'],
    note: "re-arm: the exit sells n < 2^53 (the merge argument -(self * 2^53 + n) names this input and n only then: an exit of 2^53 + m at input i reads the merge of the booked exit at input i + 1 as its own)",
    edits: [del('                require(n < MERGE_K);\n')],
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

  // ================================================================ audit completion (2026-10-13)
  // ---- KobAsk / KobBid (every require; unwitnessable ones in the header)
  { id: "AU-A-1", file: "KobAsk", run: { [T_AU_A]: ["NAU-A03"] }, note: "tplRs: the custody input is the P2SH of the holder script it pushes (a planted UTXO of the token covenant id)", edits: [del("        require(tx.inputs[idx].scriptPubKey == byte[](new ScriptPubKeyP2SHFromRedeemScript(rs)));\n")] },
  { id: "AU-A-2", file: "KobAsk", run: { [T_AU_A]: ["NAU-A04"] }, note: "tplRs: the custody's template is the token's (a planted P2SH of a look-alike template)", edits: [del("        require(blake3(byte[](pre as byte[8]) + rs.slice(0, pre) + byte[](suf as byte[8]) + rs.slice(pre + stLen, size)) == tpl);\n")] },
  { id: "AU-A-3", file: "KobAsk", run: { [T_AU_A]: ["NAU-A22"] }, inputOnly: ["NAU-A22"], note: "noStrays bound: at most MAX_TOK_IN = 8 token inputs (a 9th is not scanned; the KCC-20 program refuses a 9th input on its own, so only the ask flips)", edits: [del("        require(cnt <= MAX_TOK_IN);\n")] },
  { id: "AU-A-S0", file: "KobAsk", run: { [T_AU_A]: ["NAU-AS0"] }, note: "noStrays scan slot 0: a stray of the ask at token slot 0 rides to the taker", edits: [rep("require(byte[32](OpTxInputScriptSigSubstr(x0, e0, e0 + 32)) != me);", 'require(true);')] },
  { id: "AU-A-S1", file: "KobAsk", run: { [T_AU_A]: ["NAU-AS1"] }, note: "noStrays scan slot 1: a stray of the ask at token slot 1 rides to the taker", edits: [rep("require(byte[32](OpTxInputScriptSigSubstr(x1, e1, e1 + 32)) != me);", 'require(true);')] },
  { id: "AU-A-S2", file: "KobAsk", run: { [T_AU_A]: ["NAU-AS2"] }, note: "noStrays scan slot 2: a stray of the ask at token slot 2 rides to the taker", edits: [rep("require(byte[32](OpTxInputScriptSigSubstr(x2, e2, e2 + 32)) != me);", 'require(true);')] },
  { id: "AU-A-S3", file: "KobAsk", run: { [T_AU_A]: ["NAU-AS3"] }, note: "noStrays scan slot 3: a stray of the ask at token slot 3 rides to the taker", edits: [rep("require(byte[32](OpTxInputScriptSigSubstr(x3, e3, e3 + 32)) != me);", 'require(true);')] },
  { id: "AU-A-S4", file: "KobAsk", run: { [T_AU_A]: ["NAU-AS4"] }, note: "noStrays scan slot 4: a stray of the ask at token slot 4 rides to the taker", edits: [rep("require(byte[32](OpTxInputScriptSigSubstr(x4, e4, e4 + 32)) != me);", 'require(true);')] },
  { id: "AU-A-S5", file: "KobAsk", run: { [T_AU_A]: ["NAU-AS5"] }, note: "noStrays scan slot 5: a stray of the ask at token slot 5 rides to the taker", edits: [rep("require(byte[32](OpTxInputScriptSigSubstr(x5, e5, e5 + 32)) != me);", 'require(true);')] },
  { id: "AU-A-S6", file: "KobAsk", run: { [T_AU_A]: ["NAU-AS6"] }, note: "noStrays scan slot 6: a stray of the ask at token slot 6 rides to the taker", edits: [rep("require(byte[32](OpTxInputScriptSigSubstr(x6, e6, e6 + 32)) != me);", 'require(true);')] },
  { id: "AU-A-S7", file: "KobAsk", run: { [T_AU_A]: ["NAU-AS7"] }, note: "noStrays scan slot 7: a stray of the ask at token slot 7 rides to the taker", edits: [rep("require(byte[32](OpTxInputScriptSigSubstr(x7, e7, e7 + 32)) != me);", 'require(true);')] },
  { id: "AU-A-4", file: "KobAsk", run: { [T_AU_A]: ["NAU-A01"] }, note: "the ask is the only input of its covenant id (a sibling UTXO of the id beside the fill)", edits: [del("        require(OpCovInputCount(selfId) == 1);\n")] },
  { id: "AU-A-5", file: "KobAsk", run: { [T_AU_A]: ["NAU-A02"] }, note: "the custody carries the token covenant id (not a UTXO of another token of the same program owned by the ask)", edits: [del("        require(OpInputCovenantId(tokenIn) == tokenCovId);\n")] },
  { id: "AU-A-6", file: "KobAsk", run: { [T_AU_A]: ["NAU-A05"] }, note: "the custody is owned by this ask's covenant id (not tokens of another id: the ask's real custody would be orphaned)", edits: [del("        require(byte[32](tin.slice(10, 42)) == selfId);\n")] },
  { id: "AU-A-7", file: "KobAsk", run: { [T_AU_A]: ["NAU-A06"] }, note: "the custody has owner scheme covenant id and borrowing disabled (0x040100)", edits: [del("        require(tin.slice(43, 46) == byte[](0x040100));\n")] },
  { id: "AU-A-8", file: "KobAsk", run: { [T_AU_A]: ["NAU-A07"] }, note: "the custody carries the order's extension commitment (another commitment is another token)", edits: [del("        require(byte[32](tin.slice(80, 112)) == extensionCommitment);\n")] },
  { id: "AU-A-9", file: "KobAsk", run: { [T_AK]: ["NC8","NC9","NC8b"], [T_TCE]: ["NC3","NC4"] }, note: "refund only once due: min(expiryDaa, 90 days idle, IOC / FOK kill)", edits: [del("            require(tx.daa >= idleEnd);\n")] },
  { id: "AU-A-10", file: "KobAsk", run: { [T_AU_A]: ["NAU-A08"] }, note: "a refund leaves no output bound to the ask's covenant id (a keeper's UTXO of the id would own every later stray)", edits: [rep("            require(refundTip >= 0);\n            require(OpCovOutputCount(selfId) == 0);\n", "            require(refundTip >= 0);\n")] },
  { id: "AU-A-11", file: "KobAsk", run: { [T_TCE]: ["NC6"] }, note: "refund: the maker gets the carriers minus at most refundTip", edits: [del("            require(tx.outputs[self].value + refundTip >= carriers);\n")] },
  { id: "AU-A-12", file: "KobAsk", run: { [T_TCE]: ["NT-ACT"] }, note: "no fill before activeFrom", edits: [del("            require(tx.daa >= activeFrom);\n")] },
  { id: "AU-A-13", file: "KobAsk", hold: { [T_AU_A]: ["NAU-A10"] }, note: "fill n > 0 (NAU-A10, n = -1, is also refused by n >= minFill || n == amountLeft; AU-A-13x removes both)", edits: [del("            require(n > 0);\n")] },
  { id: "AU-A-13x", file: "KobAsk", run: { [T_AU_A]: ["NAU-A10"] }, note: "n > 0 and the minFill rule removed together: a fill of n = -1 (the taker adds a unit, the maker is paid nothing)", edits: [del("            require(n > 0);\n"), del("            require(n >= minFill || n == amountLeft);\n")] },
  { id: "AU-A-14", file: "KobAsk", run: { [T_AU_A]: ["NAU-A11"] }, note: "scale > 0 (hostile field: an ask at a negative scale sells for nothing)", edits: [del("            require(scale > 0);\n")] },
  { id: "AU-A-15", file: "KobAsk", run: { [T_LIM]: ["NF7"] }, note: "tip >= 0 (hostile field)", edits: [del("            require(tip >= 0);\n")] },
  { id: "AU-A-16", file: "KobAsk", run: { [T_AU_A]: ["NAU-A09"] }, note: "n <= amountLeft: a fill cannot claim more than the ask holds (an evidence fill of 1 base unit claiming minTouch would arm a stop)", edits: [del("            require(n <= amountLeft);\n")] },
  { id: "AU-A-17", file: "KobAsk", run: { [T_TV]: ["NV1"] }, note: "TWAP: the UTXO is >= interval DAA old (CSV)", edits: [del("                require(this.ageDaa >= interval);\n")] },
  { id: "AU-A-18", file: "KobAsk", run: { [T_TV]: ["NV2"] }, note: "TWAP: n <= maxFill", edits: [del("            require(maxFill == 0 || n <= maxFill);\n")] },
  { id: "AU-A-19", file: "KobAsk", run: { [T_TV]: ["NV7"] }, note: "decay: slope > 0 (a rising ask would reward understated time)", edits: [del("                require(slope > 0);\n")] },
  { id: "AU-A-20", file: "KobAsk", run: { [T_AU_A]: ["NAU-A12"] }, note: "decay: decayStep > 0 (a negative step makes the price rise: would reward understated time)", edits: [del("                require(decayStep > 0);\n")] },
  { id: "AU-A-21", file: "KobAsk", run: { [T_AK]: ["NM3"], [T_TV]: ["NV5"] }, note: "decay: tx.daa >= t (CLTV proves the decay time)", edits: [del("                require(tx.daa >= t);\n")] },
  { id: "AU-A-22", file: "KobAsk", run: { [T_AU_A]: ["NAU-A13"] }, note: "decay: t >= origin (a time before the origin is refused; it could only keep or raise the price)", edits: [del("                require(t >= origin);\n")] },
  { id: "AU-A-23", file: "KobAsk", run: { [T_LIM]: ["NF9"] }, note: "positional payout: output i pays P2PK(maker)", edits: [del("            require(tx.outputs[self].scriptPubKey == byte[](new ScriptPubKeyP2PK(maker)));\n")] },
  { id: "AU-A-24", file: "KobAsk", run: { [T_AU_A]: ["NAU-A14"] }, note: "GTC rest: exactly one output bound to the ask's id (no second UTXO of the id for the taker)", edits: [del("                require(OpCovOutputCount(selfId) == 1);\n")] },
  { id: "AU-A-25", file: "KobAsk", run: { [T_AU_A]: ["NAU-A15"] }, note: "GTC rest: the continuation is this script with amountLeft - n (not a repriced ask)", edits: [del("                require(tx.outputs[selfOut].scriptPubKey == contSpk(amountLeft - n));\n")] },
  { id: "AU-A-26", file: "KobAsk", run: { [T_LIM]: ["NF10"] }, note: "GTC rest: the continuation keeps the order's carrier", edits: [del("                require(tx.outputs[selfOut].value >= tx.inputs[self].value);\n")] },
  { id: "AU-A-27", file: "KobAsk", run: { [T_AU_A]: ["NAU-A16"] }, note: "GTC rest: the rest custody keeps its carrier", edits: [rep("                require(tx.outputs[selfOut].value >= tx.inputs[self].value);\n                require(tx.outputs[tokOut].value >= tx.inputs[tokenIn].value);\n", "                require(tx.outputs[selfOut].value >= tx.inputs[self].value);\n")] },
  { id: "AU-A-28", file: "KobAsk", run: { [T_LIM]: ["NF1","NF2"], [T_TV]: ["NV6"], [T_RND]: ["V3A02"] }, note: "partial fill: the maker gets at least the all-in proceeds", edits: [del("                require(tx.outputs[self].value >= allIn);\n")] },
  { id: "AU-A-29", file: "KobAsk", run: { [T_AU_A]: ["NAU-A17"] }, note: "FOK: a fill that leaves tokens unsold is refused", edits: [del("                require(tif != TIF_FOK || outAmount == 0);\n")] },
  { id: "AU-A-30", file: "KobAsk", run: { [T_AU_A]: ["NAU-A18"] }, note: "termination (IOC, sold out): no output bound to the ask's id", edits: [del("                require(OpCovOutputCount(selfId) == 0);\n")] },
  { id: "AU-A-31", file: "KobAsk", run: { [T_AU_A]: ["NAU-A19"] }, note: "IOC return: the returned rest keeps the custody's carrier", edits: [del("                    require(tx.outputs[tokOut].value >= tx.inputs[tokenIn].value);\n")] },
  { id: "AU-A-32", file: "KobAsk", run: { [T_AK]: ["NM1"] }, note: "IOC: the maker gets the all-in proceeds plus the order carrier", edits: [del("                    require(tx.outputs[self].value >= allIn + tx.inputs[self].value);\n")] },
  { id: "AU-A-33", file: "KobAsk", run: { [T_AU_A]: ["NAU-A20"] }, note: "sold out: the maker gets the all-in proceeds plus both carriers", edits: [del("                    require(tx.outputs[self].value >= allIn + carriers);\n")] },
  { id: "AU-A-34", file: "KobAsk", run: { [T_TCE]: ["NT-IOC2"] }, note: "the one token output (rest, IOC return, refund) is pinned: the custody template with the expected state", edits: [del("            require(tx.outputs[outIdx].scriptPubKey == byte[](new ScriptPubKeyP2SHFromRedeemScript(\n                trs.slice(0, tplPrefixLen) + byte[](0x08) + byte[](outAmount as byte[8]) + byte[](0x20) + byte[](outOwner)\n                    + byte[](0x01) + byte[](outScheme) + byte[](0x010020) + tin.slice(47, 79)\n                    + byte[](0x20) + tin.slice(80, 112) + trs.slice(tplPrefixLen + 112, trs.length)\n            )));\n")] },
  { id: "AU-A-35", file: "KobAsk", run: { [T_AU_A]: ["NAU-A21"] }, note: "the pinned token output is bound to the token covenant (an unbound copy of the holder script would let the taker take the rest)", edits: [del("            require(OpOutputCovenantId(outIdx) == tokenCovId);\n")] },
  { id: "AU-A-36", file: "KobAsk", run: { [T_TCE]: ["NC1"] }, note: "cancel: SIGHASH_ALL only", edits: [del("        require(sb[64] == SIGHASH_ALL);\n")] },
  { id: "AU-A-37", file: "KobAsk", run: { [T_TCE]: ["NC2"] }, note: "cancel: the maker's signature", edits: [del("        require(checkSig(s, maker));\n")] },
  { id: "AU-B-1", file: "KobBid", run: { [T_AU_B]: ["NAU-B26"] }, inputOnly: ["NAU-B26"], note: "noStrays bound: at most MAX_TOK_IN = 8 token inputs (a 9th is not scanned; the KCC-20 program refuses a 9th input on its own, so only the bid flips)", edits: [del("        require(cnt <= MAX_TOK_IN);\n")] },
  { id: "AU-B-S0", file: "KobBid", run: { [T_AU_B]: ["NAU-BS0"] }, note: "noStrays scan slot 0: a stray of the bid at token slot 0 rides to the taker", edits: [rep("require(byte[32](OpTxInputScriptSigSubstr(x0, e0, e0 + 32)) != me);", 'require(true);')] },
  { id: "AU-B-S1", file: "KobBid", run: { [T_AU_B]: ["NAU-BS1"] }, note: "noStrays scan slot 1: a stray of the bid at token slot 1 rides to the taker", edits: [rep("require(byte[32](OpTxInputScriptSigSubstr(x1, e1, e1 + 32)) != me);", 'require(true);')] },
  { id: "AU-B-S2", file: "KobBid", run: { [T_AU_B]: ["NAU-BS2"] }, note: "noStrays scan slot 2: a stray of the bid at token slot 2 rides to the taker", edits: [rep("require(byte[32](OpTxInputScriptSigSubstr(x2, e2, e2 + 32)) != me);", 'require(true);')] },
  { id: "AU-B-S3", file: "KobBid", run: { [T_AU_B]: ["NAU-BS3"] }, note: "noStrays scan slot 3: a stray of the bid at token slot 3 rides to the taker", edits: [rep("require(byte[32](OpTxInputScriptSigSubstr(x3, e3, e3 + 32)) != me);", 'require(true);')] },
  { id: "AU-B-S4", file: "KobBid", run: { [T_AU_B]: ["NAU-BS4"] }, note: "noStrays scan slot 4: a stray of the bid at token slot 4 rides to the taker", edits: [rep("require(byte[32](OpTxInputScriptSigSubstr(x4, e4, e4 + 32)) != me);", 'require(true);')] },
  { id: "AU-B-S5", file: "KobBid", run: { [T_AU_B]: ["NAU-BS5"] }, note: "noStrays scan slot 5: a stray of the bid at token slot 5 rides to the taker", edits: [rep("require(byte[32](OpTxInputScriptSigSubstr(x5, e5, e5 + 32)) != me);", 'require(true);')] },
  { id: "AU-B-S6", file: "KobBid", run: { [T_AU_B]: ["NAU-BS6"] }, note: "noStrays scan slot 6: a stray of the bid at token slot 6 rides to the taker", edits: [rep("require(byte[32](OpTxInputScriptSigSubstr(x6, e6, e6 + 32)) != me);", 'require(true);')] },
  { id: "AU-B-S7", file: "KobBid", run: { [T_AU_B]: ["NAU-BS7"] }, note: "noStrays scan slot 7: a stray of the bid at token slot 7 rides to the taker", edits: [rep("require(byte[32](OpTxInputScriptSigSubstr(x7, e7, e7 + 32)) != me);", 'require(true);')] },
  { id: "AU-B-2", file: "KobBid", run: { [T_AU_B]: ["NAU-B01"] }, note: "the bid is the only input of its covenant id (a sibling UTXO of the id beside the fill)", edits: [rep("        int self = this.activeInputIndex;\n        byte[32] selfId = OpInputCovenantId(self);\n        require(OpCovInputCount(selfId) == 1);\n", "        int self = this.activeInputIndex;\n        byte[32] selfId = OpInputCovenantId(self);\n")] },
  { id: "AU-B-3", file: "KobBid", run: { [T_AU_B]: ["NAU-B02"] }, note: "no fill before activeFrom", edits: [del("        require(tx.daa >= activeFrom);\n")] },
  { id: "AU-B-4", file: "KobBid", run: { [T_AU_B]: ["NAU-B03"] }, note: "fill n > 0 (a zero fill would close an exhausted bid before its expiry)", edits: [del("        require(n > 0);\n")] },
  { id: "AU-B-5", file: "KobBid", run: { [T_AU_B]: ["NAU-B04"] }, note: "scale > 0 (hostile field: a negative scale turns the quote negative)", edits: [del("        require(scale > 0);\n")] },
  { id: "AU-B-6", file: "KobBid", run: { [T_AU_B]: ["NAU-B05"] }, note: "tip >= 0 (hostile field: the bid would pay below its quoted price)", edits: [del("        require(tip >= 0);\n")] },
  { id: "AU-B-7", file: "KobBid", run: { [T_AU_B]: ["NAU-B06"] }, note: "reserve >= 0 (hostile field: a negative reserve lets an underfunded bid fill)", edits: [del("        require(reserve >= 0);\n")] },
  { id: "AU-B-8", file: "KobBid", run: { [T_AU_B]: ["NAU-B07"] }, note: "deliveryCarrier >= 0 (hostile field)", edits: [del("        require(deliveryCarrier >= 0);\n")] },
  { id: "AU-B-9", file: "KobBid", run: { [T_TV]: ["NV3"] }, note: "DCA: the UTXO is >= interval DAA old (CSV)", edits: [del("            require(this.ageDaa >= interval);\n")] },
  { id: "AU-B-10", file: "KobBid", run: { [T_TV]: ["NV4"] }, note: "DCA: n <= maxFill", edits: [del("        require(maxFill == 0 || n <= maxFill);\n")] },
  { id: "AU-B-11", file: "KobBid", run: { [T_AU_B]: ["NAU-B08"] }, note: "rising: slope > 0 (a falling bid would reward understated time)", edits: [del("            require(slope > 0);\n")] },
  { id: "AU-B-12", file: "KobBid", run: { [T_AU_B]: ["NAU-B09"] }, note: "rising: decayStep > 0 (a negative step makes the price fall: would reward understated time)", edits: [del("            require(decayStep > 0);\n")] },
  { id: "AU-B-13", file: "KobBid", run: { [T_TV]: ["NV9"] }, note: "rising: tx.daa >= t (CLTV proves the time)", edits: [del("            require(tx.daa >= t);\n")] },
  { id: "AU-B-14", file: "KobBid", run: { [T_AU_B]: ["NAU-B10"] }, note: "rising: t >= origin (a time before the origin is refused; it could only keep or lower the price)", edits: [del("            require(t >= origin);\n")] },
  { id: "AU-B-15", file: "KobBid", run: { [T_AU_B]: ["NAU-B11"] }, note: "a fill never spends the reserve: left >= reserve", edits: [del("        require(left >= reserve);\n")] },
  { id: "AU-B-16", file: "KobBid", run: { [T_AU_B]: ["NAU-B12"] }, note: "the delivery template is read from a token input of this token (the bytes are also hash-pinned, so a same-program source of another covenant gains nothing; the check pins the source)", edits: [del("        require(OpInputCovenantId(tokenTplIn) == tokenCovId);\n")] },
  { id: "AU-B-17", file: "KobBid", run: { [T_AU_B]: ["NAU-B13"] }, note: "the delivery template's hash is the token's (a planted look-alike would give the maker an anyone-can-spend fake)", edits: [del("        require(blake3(byte[](tplPrefixLen as byte[8]) + tpre + byte[](tplSuffixLen as byte[8]) + tsuf) == tokenTplHash);\n")] },
  { id: "AU-B-18", file: "KobBid", run: { [T_AU_B]: ["NAU-B14"] }, note: "positional delivery: output i is the holder of exactly n base units owned by the maker", edits: [del("        require(tx.outputs[self].scriptPubKey == byte[](new ScriptPubKeyP2SHFromRedeemScript(\n            tpre + byte[](0x08) + byte[](nb) + byte[](0x20) + byte[](maker)\n                + byte[](0x0100010020) + byte[](0 as byte[8]) + byte[](0 as byte[8]) + byte[](0 as byte[8]) + byte[](0 as byte[8])\n                + byte[](0x20) + extensionCommitment + tsuf\n        )));\n")] },
  { id: "AU-B-19", file: "KobBid", run: { [T_AU_B]: ["NAU-B15"] }, note: "the delivery is bound to the token covenant (an unbound copy of the holder script delivers nothing)", edits: [del("        require(OpOutputCovenantId(self) == tokenCovId);\n")] },
  { id: "AU-B-20", file: "KobBid", run: { [T_AU_B]: ["NAU-B16"] }, note: "GTC continuation: exactly one output bound to the bid's id", edits: [del("            require(OpCovOutputCount(selfId) == 1);\n")] },
  { id: "AU-B-21", file: "KobBid", run: { [T_AU_B]: ["NAU-B17"] }, note: "GTC continuation: this script (not a bid of the taker)", edits: [del("            require(tx.outputs[selfOut].scriptPubKey == this.activeScriptPubKey);\n")] },
  { id: "AU-B-22", file: "KobBid", run: { [T_LIM]: ["NF5","NF6"] }, note: "GTC continuation: exactly the unused escrow", edits: [del("            require(tx.outputs[selfOut].value == left - deliveryCarrier);\n")] },
  { id: "AU-B-23", file: "KobBid", run: { [T_LIM]: ["NF4"], [T_RND]: ["V3B03"] }, note: "GTC: the delivery carries the carrier plus the budget not spent", edits: [del("            require(tx.outputs[self].value >= deliveryCarrier + used - allIn);\n")] },
  { id: "AU-B-24", file: "KobBid", run: { [T_AU_B]: ["NAU-B18"] }, note: "termination: no output bound to the bid's id", edits: [del("            require(OpCovOutputCount(selfId) == 0);\n")] },
  { id: "AU-B-25", file: "KobBid", run: { [T_AK]: ["NM4"], [T_LIM]: ["NF3"], [T_TV]: ["NV8"], [T_RND]: ["V3B03b"] }, note: "termination: everything not spent rides on the delivery", edits: [del("            require(tx.outputs[self].value >= inValue - allIn);\n")] },
  { id: "AU-B-26", file: "KobBid", run: { [T_AU_B]: ["NAU-B24"] }, note: "cancel: SIGHASH_ALL only", edits: [del("        require(sb[64] == SIGHASH_ALL);\n")] },
  { id: "AU-B-27", file: "KobBid", run: { [T_AU_B]: ["NAU-B25"] }, note: "cancel: the maker's signature", edits: [del("        require(checkSig(s, maker));\n")] },
  { id: "AU-B-28", file: "KobBid", run: { [T_AK]: ["NC10","NC10b"], [T_TCE]: ["NC3b","NC5"] }, note: "refund only once due: min(expiryDaa, 90 days idle, IOC / FOK kill)", edits: [del("        require(tx.daa >= idleEnd);\n")] },
  { id: "AU-B-29", file: "KobBid", run: { [T_AU_B]: ["NAU-B19"] }, note: "refund: the bid is the only input of its covenant id", edits: [rep("        require(tx.daa >= idleEnd);\n        byte[32] selfId = OpInputCovenantId(self);\n        require(OpCovInputCount(selfId) == 1);\n", "        require(tx.daa >= idleEnd);\n        byte[32] selfId = OpInputCovenantId(self);\n")] },
  { id: "AU-B-30", file: "KobBid", run: { [T_STRAY]: ["NS6"] }, note: "refund: no token input (strays owned by the id stay put)", edits: [del("        require(OpCovInputCount(tokenCovId) == 0);  // strays owned by this id stay put\n")] },
  { id: "AU-B-31", file: "KobBid", run: { [T_AU_B]: ["NAU-B21"] }, note: "refund: no output bound to the bid's id", edits: [rep("        require(OpCovInputCount(tokenCovId) == 0);  // strays owned by this id stay put\n        require(OpCovOutputCount(selfId) == 0);\n", "        require(OpCovInputCount(tokenCovId) == 0);  // strays owned by this id stay put\n")] },
  { id: "AU-B-32", file: "KobBid", run: { [T_TCE]: ["NC7"], [T_AU_B]: ["NAU-B22"] }, note: "refund: output i pays P2PK(maker)", edits: [del("        require(tx.outputs[self].scriptPubKey == byte[](new ScriptPubKeyP2PK(maker)));\n")] },
  { id: "AU-B-33", file: "KobBid", run: { [T_AU_B]: ["NAU-B23"] }, note: "refund: the maker gets the escrow minus at most refundTip", edits: [del("        require(tx.outputs[self].value + refundTip >= tx.inputs[self].value);\n")] },
  // ---- KobCondAsk
  { id: "AU-CA-1", file: "KobCondAsk", run: { [T_AU_CA]: ["NAU-CA03"] }, note: "tplRs: the custody input is the P2SH of the holder script it pushes (a planted UTXO of the token covenant id)", edits: [rep("    function tplRs(int idx, int pre, int stLen, int suf, byte[32] tpl) : byte[] {\n        int size = pre + stLen + suf;\n        int base = OpTxInputScriptSigLen(idx) - size;\n        byte[] rs = OpTxInputScriptSigSubstr(idx, base, base + size);\n        require(tx.inputs[idx].scriptPubKey == byte[](new ScriptPubKeyP2SHFromRedeemScript(rs)));\n", "    function tplRs(int idx, int pre, int stLen, int suf, byte[32] tpl) : byte[] {\n        int size = pre + stLen + suf;\n        int base = OpTxInputScriptSigLen(idx) - size;\n        byte[] rs = OpTxInputScriptSigSubstr(idx, base, base + size);\n")] },
  { id: "AU-CA-2", file: "KobCondAsk", run: { [T_AU_CA]: ["NAU-CA04"] }, note: "tplRs: the custody's template is the token's (a planted P2SH of a look-alike template)", edits: [rep("    function tplRs(int idx, int pre, int stLen, int suf, byte[32] tpl) : byte[] {\n        int size = pre + stLen + suf;\n        int base = OpTxInputScriptSigLen(idx) - size;\n        byte[] rs = OpTxInputScriptSigSubstr(idx, base, base + size);\n        require(tx.inputs[idx].scriptPubKey == byte[](new ScriptPubKeyP2SHFromRedeemScript(rs)));\n        require(blake3(byte[](pre as byte[8]) + rs.slice(0, pre) + byte[](suf as byte[8]) + rs.slice(pre + stLen, size)) == tpl);\n", "    function tplRs(int idx, int pre, int stLen, int suf, byte[32] tpl) : byte[] {\n        int size = pre + stLen + suf;\n        int base = OpTxInputScriptSigLen(idx) - size;\n        byte[] rs = OpTxInputScriptSigSubstr(idx, base, base + size);\n        require(tx.inputs[idx].scriptPubKey == byte[](new ScriptPubKeyP2SHFromRedeemScript(rs)));\n")] },
  { id: "AU-CA-3", file: "KobCondAsk", run: { [T_ARM]: ["NTU18"], [`${T_ARM}_3x3`]: ["NTU18"], [T_SET]: ["NTA18"], [`${T_SET}_3x3`]: ["NTA18"], [T_TRL]: ["NTT18"], [`${T_TRL}_3x3`]: ["NTT18"] }, note: "tplState (evidence and its custody): the input is the P2SH of the redeem script read (a non-P2SH evidence input)", edits: [rep("    function tplState(int idx, int pre, int stLen, int suf, byte[32] tpl) : byte[] {\n        int size = pre + stLen + suf;\n        int base = OpTxInputScriptSigLen(idx) - size;\n        byte[] rs = OpTxInputScriptSigSubstr(idx, base, base + size);\n        require(tx.inputs[idx].scriptPubKey == byte[](new ScriptPubKeyP2SHFromRedeemScript(rs)));\n", "    function tplState(int idx, int pre, int stLen, int suf, byte[32] tpl) : byte[] {\n        int size = pre + stLen + suf;\n        int base = OpTxInputScriptSigLen(idx) - size;\n        byte[] rs = OpTxInputScriptSigSubstr(idx, base, base + size);\n")] },
  { id: "AU-CA-4", file: "KobCondAsk", run: { [T_TM]: ["NT6"], [T_ARM]: ["NTU17"], [`${T_ARM}_3x3`]: ["NTU17"], [T_SET]: ["NTA17"], [`${T_SET}_3x3`]: ["NTA17"], [T_TRL]: ["NTT17"], [`${T_TRL}_3x3`]: ["NTT17"] }, note: "tplState (evidence and its custody): the template hash (a look-alike evidence order)", edits: [rep("    function tplState(int idx, int pre, int stLen, int suf, byte[32] tpl) : byte[] {\n        int size = pre + stLen + suf;\n        int base = OpTxInputScriptSigLen(idx) - size;\n        byte[] rs = OpTxInputScriptSigSubstr(idx, base, base + size);\n        require(tx.inputs[idx].scriptPubKey == byte[](new ScriptPubKeyP2SHFromRedeemScript(rs)));\n        require(blake3(byte[](pre as byte[8]) + rs.slice(0, pre) + byte[](suf as byte[8]) + rs.slice(pre + stLen, size)) == tpl);\n", "    function tplState(int idx, int pre, int stLen, int suf, byte[32] tpl) : byte[] {\n        int size = pre + stLen + suf;\n        int base = OpTxInputScriptSigLen(idx) - size;\n        byte[] rs = OpTxInputScriptSigSubstr(idx, base, base + size);\n        require(tx.inputs[idx].scriptPubKey == byte[](new ScriptPubKeyP2SHFromRedeemScript(rs)));\n")] },
  { id: "AU-CA-5", file: "KobCondAsk", run: { [T_AU_CA]: ["NAU-CA01"] }, note: "settle: the order is the only input of its covenant id (a sibling UTXO of the id)", edits: [rep("        int n = int(nb);\n        int self = this.activeInputIndex;\n        byte[32] selfId = OpInputCovenantId(self);\n        require(OpCovInputCount(selfId) == 1);\n", "        int n = int(nb);\n        int self = this.activeInputIndex;\n        byte[32] selfId = OpInputCovenantId(self);\n")] },
  { id: "AU-CA-6", file: "KobCondAsk", run: { [T_AU_CA]: ["NAU-CA02"] }, note: "the custody carries the token covenant id (not another token of the same program owned by the order)", edits: [del("        require(OpInputCovenantId(tokenIn) == tokenCovId);\n")] },
  { id: "AU-CA-7", file: "KobCondAsk", run: { [T_AU_CA]: ["NAU-CA05"] }, note: "the custody is owned by this order's covenant id", edits: [del("        require(byte[32](tin.slice(10, 42)) == selfId);\n")] },
  { id: "AU-CA-8", file: "KobCondAsk", run: { [T_AU_CA]: ["NAU-CA06"] }, note: "the custody has owner scheme covenant id and borrowing disabled (0x040100)", edits: [del("        require(tin.slice(43, 46) == byte[](0x040100));\n")] },
  { id: "AU-CA-9", file: "KobCondAsk", run: { [T_AU_CA]: ["NAU-CA07"] }, note: "the custody carries the order's extension commitment", edits: [del("        require(byte[32](tin.slice(80, 112)) == extensionCommitment);\n")] },
  { id: "AU-CA-10", file: "KobCondAsk", run: { [T_STRAY]: ["NS7b"] }, note: "the custody holds exactly amountLeft (a dust UTXO of the id cannot stand in for it)", edits: [del("        require(tinAmount == amountLeft);\n")] },
  { id: "AU-CA-11", file: "KobCondAsk", run: { [T_AU_CA]: ["NAU-CA08"] }, note: "refund only from min(expiryDaa, 90 days idle)", edits: [del("            require(tx.daa >= idleEnd);\n")] },
  { id: "AU-CA-12", file: "KobCondAsk", run: { [T_AU_CA]: ["NAU-CA09"] }, note: "a refund leaves no output bound to the order's id", edits: [rep("            require(refundTip >= 0);\n            require(OpCovOutputCount(selfId) == 0);\n", "            require(refundTip >= 0);\n")] },
  { id: "AU-CA-13", file: "KobCondAsk", run: { [T_AU_CA]: ["NAU-CA10"] }, note: "refund: the maker gets the carriers minus at most refundTip", edits: [del("            require(tx.outputs[self].value + refundTip >= carriers);\n")] },
  { id: "AU-CA-14", file: "KobCondAsk", run: { [T_AU_CA]: ["NAU-CA11"] }, note: "no fill before activeFrom", edits: [del("            require(tx.daa >= activeFrom);\n")] },
  { id: "AU-CA-15", file: "KobCondAsk", hold: { [T_AU_CA]: ["NAU-CA19"] }, note: "fill n > 0 (NAU-CA19, n = -1, is also refused by n >= minFill || n == amountLeft; AU-CA-15x removes both)", edits: [del("            require(n > 0);\n")] },
  { id: "AU-CA-15x", file: "KobCondAsk", run: { [T_AU_CA]: ["NAU-CA19"] }, note: "n > 0 and the minFill rule removed together: a fill of n = -1 (the taker adds a unit, the maker is paid nothing)", edits: [del("            require(n > 0);\n"), del("            require(n >= minFill || n == amountLeft);\n")] },
  { id: "AU-CA-16", file: "KobCondAsk", run: { [T_AU_CA]: ["NAU-CA12"] }, note: "scale > 0 (hostile field: a negative scale sells for nothing)", edits: [del("            require(scale > 0);\n")] },
  { id: "AU-CA-17", file: "KobCondAsk", run: { [T_AU_CA]: ["NAU-CA13"] }, note: "tip >= 0 (hostile field)", edits: [del("            require(tip >= 0);\n")] },
  { id: "AU-CA-18", file: "KobCondAsk", run: { [T_AU_CA]: ["NAU-CA22"] }, note: "n <= amountLeft (a fill claiming more than the custody holds)", edits: [del("            require(n <= amountLeft);\n")] },
  { id: "AU-CA-19", file: "KobCondAsk", run: { [T_AU_CA]: ["NAU-CA23"] }, note: "the TP leg needs tpPrice > 0 (a stop-only order would sell on the TP leg for nothing)", edits: [del("                require(tpPrice > 0);\n")] },
  { id: "AU-CA-20", file: "KobCondAsk", run: { [T_AU_CA]: ["NAU-CA24"] }, note: "leg is 0 or 1", edits: [del("                require(leg == 1);\n")] },
  { id: "AU-CA-21", file: "KobCondAsk", run: { [T_AU_CA]: ["NAU-CA25"] }, note: "the stop leg needs stopPrice > 0", edits: [del("                require(stopPrice > 0);\n")] },
  { id: "AU-CA-22", file: "KobCondAsk", run: { [T_AU_CA]: ["NAU-CA26"] }, note: "slipBps >= 0 (hostile field)", edits: [del("                require(slipBps >= 0);\n")] },
  { id: "AU-CA-23", file: "KobCondAsk", run: { [T_AU_CA]: ["NAU-CA27"] }, note: "slipBps <= 10000 (hostile field: a band wider than the stop price; outside an auction the leg price would also go negative)", edits: [del("                require(slipBps <= 10000);\n")] },
  { id: "AU-CA-24", file: "KobCondAsk", run: { [T_LC]: ["NA2"] }, note: "stop auction: tx.daa >= t (CLTV proves the auction time)", edits: [del("                        require(tx.daa >= t);\n")] },
  { id: "AU-CA-25", file: "KobCondAsk", run: { [T_AU_CA]: ["NAU-CA28"] }, note: "stopPrice <= MAX_STOP (the band product must fit; with slipBps 0 an oversized stop would sell)", edits: [del("                require(stopPrice <= MAX_STOP);\n")] },
  { id: "AU-CA-26", file: "KobCondAsk", run: { [T_AU_CA]: ["NAU-CA17"] }, note: "positional payout: output i pays P2PK(maker)", edits: [del("            require(tx.outputs[self].scriptPubKey == byte[](new ScriptPubKeyP2PK(maker)));\n")] },
  { id: "AU-CA-27", file: "KobCondAsk", run: { [T_AU_CA]: ["NAU-CA14"] }, note: "rest: exactly one output bound to the order's id", edits: [del("                require(OpCovOutputCount(selfId) == 1);\n")] },
  { id: "AU-CA-28", file: "KobCondAsk", run: { [T_AU_CA]: ["NAU-CA15"] }, note: "rest: the continuation is this script with the spliced windows (not a repriced order)", edits: [del("                require(tx.outputs[selfOut].scriptPubKey == contSpk(stopPrice, newArmed, amountLeft - n));\n")] },
  { id: "AU-CA-29", file: "KobCondAsk", run: { [T_AU_CA]: ["NAU-CA29"] }, note: "rest: the continuation keeps the order's carrier", edits: [del("                require(tx.outputs[selfOut].value >= tx.inputs[self].value);\n")] },
  { id: "AU-CA-30", file: "KobCondAsk", run: { [T_AU_CA]: ["NAU-CA16"] }, note: "rest: the rest custody keeps its carrier", edits: [del("                require(tx.outputs[tokOut].value >= tx.inputs[tokenIn].value);\n")] },
  { id: "AU-CA-31", file: "KobCondAsk", run: { [T_AU_CA]: ["NAU-CA18"] }, note: "sold out: no output bound to the order's id", edits: [del("                require(OpCovOutputCount(selfId) == 0);\n")] },
  { id: "AU-CA-32", file: "KobCondAsk", run: { [T_AU_CA]: ["NAU-CA31"] }, note: "re-arming sell-out: the maker gets the profit (all-in minus the budget)", edits: [del("                    require(tx.outputs[self].value >= allIn);\n")] },
  { id: "AU-CA-33", file: "KobCondAsk", run: { [T_AU_CA]: ["NAU-CA20"] }, note: "sold out: the maker gets the all-in proceeds plus both carriers", edits: [del("                    require(tx.outputs[self].value >= allIn + carriers);\n")] },
  { id: "AU-CA-34", file: "KobCondAsk", run: { [T_AU_CA]: ["NAU-CA30"] }, note: "the one token output (rest, refund) is pinned (template, amount, owner)", edits: [del("            require(tx.outputs[outIdx].scriptPubKey == byte[](new ScriptPubKeyP2SHFromRedeemScript(\n                trs.slice(0, tplPrefixLen) + byte[](0x08) + byte[](outAmount as byte[8]) + byte[](0x20) + byte[](outOwner)\n                    + byte[](0x01) + byte[](outScheme) + byte[](0x010020) + tin.slice(47, 79) + byte[](0x20) + tin.slice(80, 112)\n                    + trs.slice(tplPrefixLen + 112, trs.length)\n            )));\n")] },
  { id: "AU-CA-35", file: "KobCondAsk", run: { [T_AU_CA]: ["NAU-CA21"] }, note: "the pinned token output is bound to the token covenant", edits: [del("            require(OpOutputCovenantId(outIdx) == tokenCovId);\n")] },
  { id: "AU-CA-36", file: "KobCondAsk", run: { [T_AU_CA]: ["NAU-CA32"] }, note: "update: the order is the only input of its covenant id", edits: [rep("    entry update(int ev, int tk) {\n        int self = this.activeInputIndex;\n        byte[32] selfId = OpInputCovenantId(self);\n        require(OpCovInputCount(selfId) == 1);\n", "    entry update(int ev, int tk) {\n        int self = this.activeInputIndex;\n        byte[32] selfId = OpInputCovenantId(self);\n")] },
  { id: "AU-CA-37", file: "KobCondAsk", run: { [T_AU_CA]: ["NAU-CA33"] }, note: "update: stopPrice > 0 (an order without a stop leg is never armed or trailed)", edits: [rep("        require(armed == 0);\n        require(stopPrice > 0);\n", "        require(armed == 0);\n")] },
  { id: "AU-CA-38", file: "KobCondAsk", run: { [T_AU_CA]: ["NAU-CA34"] }, note: "update: not before activeFrom", edits: [rep("        require(keeperTip >= 0);\n        require(tx.daa >= activeFrom);\n", "        require(keeperTip >= 0);\n")] },
  { id: "AU-CA-39", file: "KobCondAsk", run: { [T_AU_CA]: ["NAU-CA35"] }, note: "update: exactly one output bound to the order's id", edits: [rep("        }\n        require(OpCovOutputCount(selfId) == 1);\n", "        }\n")] },
  { id: "AU-CA-40", file: "KobCondAsk", run: { [T_AU_CA]: ["NAU-CA36"] }, note: "update: the continuation changes only stopPrice / armed as the evidence justifies", edits: [del("        require(tx.outputs[selfOut].scriptPubKey == contSpk(newStop, newArmed, amountLeft));\n")] },
  { id: "AU-CA-41", file: "KobCondAsk", run: { [T_LC]: ["NK1"] }, note: "update: the keeper takes at most keeperTip", edits: [del("        require(tx.outputs[selfOut].value >= tx.inputs[self].value - keeperTip);\n")] },
  { id: "AU-CA-42", file: "KobCondAsk", run: { [T_AU_CA]: ["NAU-CA37"] }, note: "cancel: SIGHASH_ALL only", edits: [del("        require(sb[64] == SIGHASH_ALL);\n")] },
  { id: "AU-CA-43", file: "KobCondAsk", run: { [T_AU_CA]: ["NAU-CA38"] }, note: "cancel: the maker's signature", edits: [del("        require(checkSig(s, maker));\n")] },
  // ---- KobIfdBid
  { id: "AU-IB-1", file: "KobIfdBid", run: { [T_IF]: ["NTI18"], [`${T_IF}_3x3`]: ["NTI18"], [T_IA]: ["NTJ18"], [`${T_IA}_3x3`]: ["NTJ18"] }, note: "tplState (evidence bid, merge exit): the input is the P2SH of the redeem script read", edits: [del("        require(tx.inputs[idx].scriptPubKey == byte[](new ScriptPubKeyP2SHFromRedeemScript(rs)));\n")] },
  { id: "AU-IB-2", file: "KobIfdBid", run: { [T_IF]: ["NTI17"], [`${T_IF}_3x3`]: ["NTI17"], [T_IA]: ["NTJ17"], [`${T_IA}_3x3`]: ["NTJ17"] }, note: "tplState (evidence bid, merge exit): the template hash", edits: [del("        require(blake3(byte[](pre as byte[8]) + rs.slice(0, pre) + byte[](suf as byte[8]) + rs.slice(pre + stLen, size)) == tpl);\n")] },
  { id: "AU-IB-3", file: "KobIfdBid", run: { [T_AU_IB]: ["NAU-IB30"] }, inputOnly: ["NAU-IB30"], note: "noStrays bound: at most MAX_TOK_IN = 8 token inputs (a 9th is not scanned; the KCC-20 program refuses a 9th input on its own, so only the entry flips)", edits: [del("        require(cnt <= MAX_TOK_IN);\n")] },
  { id: "AU-IB-S0", file: "KobIfdBid", run: { [T_AU_IB]: ["NAU-IBS0"] }, note: "noStrays scan slot 0: a stray of the entry at token slot 0 rides to the taker", edits: [rep("require(byte[32](OpTxInputScriptSigSubstr(x0, e0, e0 + 32)) != me);", 'require(true);')] },
  { id: "AU-IB-S1", file: "KobIfdBid", run: { [T_AU_IB]: ["NAU-IBS1"] }, note: "noStrays scan slot 1: a stray of the entry at token slot 1 rides to the taker", edits: [rep("require(byte[32](OpTxInputScriptSigSubstr(x1, e1, e1 + 32)) != me);", 'require(true);')] },
  { id: "AU-IB-S2", file: "KobIfdBid", run: { [T_AU_IB]: ["NAU-IBS2"] }, note: "noStrays scan slot 2: a stray of the entry at token slot 2 rides to the taker", edits: [rep("require(byte[32](OpTxInputScriptSigSubstr(x2, e2, e2 + 32)) != me);", 'require(true);')] },
  { id: "AU-IB-S3", file: "KobIfdBid", run: { [T_AU_IB]: ["NAU-IBS3"] }, note: "noStrays scan slot 3: a stray of the entry at token slot 3 rides to the taker", edits: [rep("require(byte[32](OpTxInputScriptSigSubstr(x3, e3, e3 + 32)) != me);", 'require(true);')] },
  { id: "AU-IB-S4", file: "KobIfdBid", run: { [T_AU_IB]: ["NAU-IBS4"] }, note: "noStrays scan slot 4: a stray of the entry at token slot 4 rides to the taker", edits: [rep("require(byte[32](OpTxInputScriptSigSubstr(x4, e4, e4 + 32)) != me);", 'require(true);')] },
  { id: "AU-IB-S5", file: "KobIfdBid", run: { [T_AU_IB]: ["NAU-IBS5"] }, note: "noStrays scan slot 5: a stray of the entry at token slot 5 rides to the taker", edits: [rep("require(byte[32](OpTxInputScriptSigSubstr(x5, e5, e5 + 32)) != me);", 'require(true);')] },
  { id: "AU-IB-S6", file: "KobIfdBid", run: { [T_AU_IB]: ["NAU-IBS6"] }, note: "noStrays scan slot 6: a stray of the entry at token slot 6 rides to the taker", edits: [rep("require(byte[32](OpTxInputScriptSigSubstr(x6, e6, e6 + 32)) != me);", 'require(true);')] },
  { id: "AU-IB-S7", file: "KobIfdBid", run: { [T_AU_IB]: ["NAU-IBS7"] }, note: "noStrays scan slot 7: a stray of the entry at token slot 7 rides to the taker", edits: [rep("require(byte[32](OpTxInputScriptSigSubstr(x7, e7, e7 + 32)) != me);", 'require(true);')] },
  { id: "AU-IB-4", file: "KobIfdBid", run: { [T_AU_IB]: ["NAU-IB01"] }, note: "fill / merge: the entry is the only input of its covenant id", edits: [rep("        int n = int(nb);\n        int self = this.activeInputIndex;\n        byte[32] selfId = OpInputCovenantId(self);\n        require(OpCovInputCount(selfId) == 1);\n", "        int n = int(nb);\n        int self = this.activeInputIndex;\n        byte[32] selfId = OpInputCovenantId(self);\n")] },
  { id: "AU-IB-5", file: "KobIfdBid", run: { [T_AU_IB]: ["NAU-IB02"] }, note: "scale > 0 (hostile field)", edits: [del("        require(scale > 0);\n")] },
  { id: "AU-IB-6", file: "KobIfdBid", run: { [T_AU_IB]: ["NAU-IB03"] }, note: "tip >= 0 (hostile field)", edits: [del("        require(tip >= 0);\n")] },
  { id: "AU-IB-7", file: "KobIfdBid", run: { [T_AU_IB]: ["NAU-IB04"] }, note: "merge: the exit terms [189..244) (slipBps .. minRestDaa) are the committed exitState", edits: [del("            require(x.slice(189, 244) == st.slice(189, 244));\n")] },
  { id: "AU-IB-8", file: "KobIfdBid", run: { [T_AU_IB]: ["NAU-IB05"] }, note: "merge: the exit terms [252..271) (bandDaa, keeperTip) are the committed exitState", edits: [del("            require(x.slice(252, 271) == st.slice(252, 271));\n")] },
  { id: "AU-IB-9", file: "KobIfdBid", run: { [T_AU_IB]: ["NAU-IB06"] }, note: "merge: the exit's extension commitment is the entry's", edits: [del("            require(x.slice(330, 363) == byte[](0x20) + extensionCommitment);\n")] },
  { id: "AU-IB-10", file: "KobIfdBid", run: { [T_AU_IB]: ["NAU-IB07"] }, note: "no fill before activeFrom", edits: [del("            require(tx.daa >= activeFrom);\n")] },
  { id: "AU-IB-11", file: "KobIfdBid", run: { [T_AU_IB]: ["NAU-IB08"] }, note: "fill n > 0 (a zero fill of an empty repeating entry would book an empty exit and move carriers out of the escrow)", edits: [del("            require(n > 0);\n")] },
  { id: "AU-IB-12", file: "KobIfdBid", run: { [T_OI]: ["NI10"] }, note: "n <= amountLeft", edits: [del("            require(n <= amountLeft);\n")] },
  { id: "AU-IB-13", file: "KobIfdBid", run: { [T_AU_IB]: ["NAU-IB09"] }, note: "deliveryCarrier >= 0 (hostile field)", edits: [del("            require(deliveryCarrier >= 0);\n")] },
  { id: "AU-IB-14", file: "KobIfdBid", run: { [T_AU_IB]: ["NAU-IB10"] }, note: "exitCarrier >= 0 (hostile field)", edits: [del("            require(exitCarrier >= 0);\n")] },
  { id: "AU-IB-15", file: "KobIfdBid", run: { [T_IFD]: ["NI19"] }, note: "stop entry: entryStop <= price", edits: [del("                require(entryStop <= price);\n")] },
  { id: "AU-IB-16", file: "KobIfdBid", run: { [T_AU_IB]: ["NAU-IB31"] }, note: "stop auction: tx.daa >= t (CLTV)", edits: [del("                        require(tx.daa >= t);\n")] },
  { id: "AU-IB-17", file: "KobIfdBid", run: { [T_AU_IB]: ["NAU-IB32"] }, note: "stop auction: t >= origin", edits: [del("                        require(t >= newArmed);\n")] },
  { id: "AU-IB-18", file: "KobIfdBid", run: { [T_AU_IB]: ["NAU-IB33"] }, note: "repeat: a fill that is not booked leaves fewer re-arms than one minimum fill", edits: [del("                require(rptAmount <= 1 || rptAmount - 1 < minFill);\n")] },
  { id: "AU-IB-19", file: "KobIfdBid", run: { [T_AU_IB]: ["NAU-IB34"] }, hold: { [T_OI]: ["NI4"] }, note: "the exit is built from the KobCondAsk template (cPre / cSuf hash): a look-alike template around the committed state would make the exit and its tokens anyone-can-spend (NI4, the KobAsk template beside a KobCondAsk output, is also refused by the exit script equality)", edits: [del("            require(blake3(byte[](cPre.length as byte[8]) + cPre + byte[](cSuf.length as byte[8]) + cSuf) == COND_TPL);\n")] },
  { id: "AU-IB-20", file: "KobIfdBid", hold: { [T_OI]: ["NI2","NI12"] }, note: "output exitOut is the committed exit (defence in depth: the exit covenant id is derived from exitSpk, so a tampered exit output also fails the genesis id check; AU-IB-20x removes both)", edits: [del("            require(tx.outputs[exitOut].scriptPubKey == exitSpk);\n")] },
  { id: "AU-IB-21", file: "KobIfdBid", run: { [T_OI]: ["NI6","NI8"] }, hold: { [T_OI]: ["NI7"] }, note: "the exit's covenant id is a fresh genesis of this entry's outpoint (NI7, the entry's own id, is also refused by the continuation count)", edits: [del("            require(exitId == blake2bWithKey(\n                byte[](OpOutpointTxId(self)) + byte[](OpOutpointIndex(self) as byte[4]) + byte[](0x0100000000000000)\n                    + byte[](exitOut as byte[4]) + byte[](tx.outputs[exitOut].value as byte[8])\n                    + exitSpk.slice(0, 2) + byte[](0x2300000000000000) + exitSpk.slice(2, 37),\n                byte[](\"CovenantID\")\n            ));\n")] },
  { id: "AU-IB-20x", file: "KobIfdBid", run: { [T_OI]: ["NI2"] }, note: "the exit script equality and the genesis id check removed together: a tampered exit (stop leg removed) is accepted", edits: [del("            require(tx.outputs[exitOut].scriptPubKey == exitSpk);\n"), del("            require(exitId == blake2bWithKey(\n                byte[](OpOutpointTxId(self)) + byte[](OpOutpointIndex(self) as byte[4]) + byte[](0x0100000000000000)\n                    + byte[](exitOut as byte[4]) + byte[](tx.outputs[exitOut].value as byte[8])\n                    + exitSpk.slice(0, 2) + byte[](0x2300000000000000) + exitSpk.slice(2, 37),\n                byte[](\"CovenantID\")\n            ));\n")] },
  { id: "AU-IB-22", file: "KobIfdBid", run: { [T_OI]: ["NI5c"] }, note: "partial fill: the exit carries exitCarrier", edits: [del("                require(tx.outputs[exitOut].value >= exitCarrier);\n")] },
  { id: "AU-IB-23", file: "KobIfdBid", run: { [T_OI]: ["NI5"] }, note: "final fill: the exit gets everything left", edits: [del("                require(tx.outputs[exitOut].value >= left);\n")] },
  { id: "AU-IB-24", file: "KobIfdBid", run: { [T_AU_IB]: ["NAU-IB12"] }, note: "the delivery template is read from a token input of this token (bytes also hash-pinned; pins the source)", edits: [del("            require(OpInputCovenantId(tokenTplIn) == tokenCovId);\n")] },
  { id: "AU-IB-25", file: "KobIfdBid", run: { [T_AU_IB]: ["NAU-IB13"] }, note: "the delivery template's hash is the token's (a planted look-alike would give the exit an anyone-can-spend fake)", edits: [del("            require(blake3(byte[](tplPrefixLen as byte[8]) + tpre + byte[](tplSuffixLen as byte[8]) + tsuf) == tokenTplHash);\n")] },
  { id: "AU-IB-26", file: "KobIfdBid", run: { [T_OI]: ["NI3"] }, note: "positional delivery: output i holds n base units owned by the exit's covenant id", edits: [del("            require(tx.outputs[self].scriptPubKey == byte[](new ScriptPubKeyP2SHFromRedeemScript(\n                tpre + byte[](0x08) + byte[](nb) + byte[](0x20) + byte[](exitId)\n                    + byte[](0x0104010020) + byte[](0 as byte[8]) + byte[](0 as byte[8]) + byte[](0 as byte[8]) + byte[](0 as byte[8])\n                    + byte[](0x20) + extensionCommitment + tsuf\n            )));\n")] },
  { id: "AU-IB-27", file: "KobIfdBid", run: { [T_AU_IB]: ["NAU-IB14"] }, note: "the delivery is bound to the token covenant", edits: [del("            require(OpOutputCovenantId(self) == tokenCovId);\n")] },
  { id: "AU-IB-28", file: "KobIfdBid", run: { [T_AU_IB]: ["NAU-IB15"] }, note: "the delivery carries deliveryCarrier", edits: [del("            require(tx.outputs[self].value >= deliveryCarrier);\n")] },
  { id: "AU-IB-29", file: "KobIfdBid", run: { [T_AU_IB]: ["NAU-IB35"] }, note: "continuation: exactly one output bound to the entry's id", edits: [del("            require(OpCovOutputCount(selfId) == 1);\n")] },
  { id: "AU-IB-30", file: "KobIfdBid", run: { [T_OI]: ["NI5b"] }, note: "continuation: the escrow floor", edits: [del("            require(tx.outputs[c].value >= floor);\n")] },
  { id: "AU-IB-31", file: "KobIfdBid", run: { [T_AU_IB]: ["NAU-IB16"] }, note: "final fill: no output bound to the entry's id", edits: [del("            require(OpCovOutputCount(selfId) == 0);\n")] },
  { id: "AU-IB-32", file: "KobIfdBid", run: { [T_AU_IB]: ["NAU-IB17"] }, note: "update: the entry is the only input of its covenant id", edits: [rep("    entry update(int ev) {\n        int self = this.activeInputIndex;\n        byte[32] selfId = OpInputCovenantId(self);\n        require(OpCovInputCount(selfId) == 1);\n", "    entry update(int ev) {\n        int self = this.activeInputIndex;\n        byte[32] selfId = OpInputCovenantId(self);\n")] },
  { id: "AU-IB-33", file: "KobIfdBid", run: { [T_IFD]: ["NI20"] }, note: "update: only a stop entry is armed", edits: [del("        require(entryStop > 0);\n")] },
  { id: "AU-IB-34", file: "KobIfdBid", run: { [T_AU_IB]: ["NAU-IB18"] }, note: "update: only an unarmed entry is armed (an armed auction would restart)", edits: [del("        require(armed == 0);\n")] },
  { id: "AU-IB-35", file: "KobIfdBid", run: { [T_AU_IB]: ["NAU-IB19"] }, note: "update: keeperTip >= 0 (hostile field)", edits: [del("        require(keeperTip >= 0);\n")] },
  { id: "AU-IB-36", file: "KobIfdBid", run: { [T_AU_IB]: ["NAU-IB20"] }, note: "update: not before activeFrom", edits: [rep("        require(keeperTip >= 0);\n        require(tx.daa >= activeFrom);\n", "        require(keeperTip >= 0);\n")] },
  { id: "AU-IB-37", file: "KobIfdBid", run: { [T_AU_IB]: ["NAU-IB21"] }, note: "update: exactly one output bound to the entry's id", edits: [rep("        require(touchBid(ev) >= entryStop);\n        require(OpCovOutputCount(selfId) == 1);\n", "        require(touchBid(ev) >= entryStop);\n")] },
  { id: "AU-IB-38", file: "KobIfdBid", run: { [T_IFD]: ["NI23"] }, note: "update: the continuation changes only armed", edits: [del("        require(tx.outputs[c].scriptPubKey == contSpk(amountLeft, 1, rptAmount));\n")] },
  { id: "AU-IB-39", file: "KobIfdBid", run: { [T_IFD]: ["NI21"] }, note: "update: the keeper takes at most keeperTip", edits: [del("        require(tx.outputs[c].value >= tx.inputs[self].value - keeperTip);\n")] },
  { id: "AU-IB-40", file: "KobIfdBid", run: { [T_AU_IB]: ["NAU-IB22"] }, note: "cancel: SIGHASH_ALL only", edits: [del("        require(sb[64] == SIGHASH_ALL);\n")] },
  { id: "AU-IB-41", file: "KobIfdBid", run: { [T_AU_IB]: ["NAU-IB23"] }, note: "cancel: the maker's signature", edits: [del("        require(checkSig(s, maker));\n")] },
  { id: "AU-IB-42", file: "KobIfdBid", run: { [T_AU_IB]: ["NAU-IB24"] }, note: "refund only from min(expiryDaa, 90 days idle)", edits: [del("        require(tx.daa >= idleEnd);\n")] },
  { id: "AU-IB-43", file: "KobIfdBid", run: { [T_AU_IB]: ["NAU-IB25"] }, note: "refund: the entry is the only input of its covenant id", edits: [rep("        require(tx.daa >= idleEnd);\n        byte[32] selfId = OpInputCovenantId(self);\n        require(OpCovInputCount(selfId) == 1);\n", "        require(tx.daa >= idleEnd);\n        byte[32] selfId = OpInputCovenantId(self);\n")] },
  { id: "AU-IB-44", file: "KobIfdBid", run: { [T_AU_IB]: ["NAU-IB26"] }, note: "refund: no token input", edits: [del("        require(OpCovInputCount(tokenCovId) == 0);\n")] },
  { id: "AU-IB-45", file: "KobIfdBid", run: { [T_AU_IB]: ["NAU-IB27"] }, note: "refund: no output bound to the entry's id", edits: [rep("        require(OpCovInputCount(tokenCovId) == 0);\n        require(OpCovOutputCount(selfId) == 0);\n", "        require(OpCovInputCount(tokenCovId) == 0);\n")] },
  { id: "AU-IB-46", file: "KobIfdBid", run: { [T_AU_IB]: ["NAU-IB28"] }, note: "refund: output i pays P2PK(maker)", edits: [del("        require(tx.outputs[self].scriptPubKey == byte[](new ScriptPubKeyP2PK(maker)));\n")] },
  { id: "AU-IB-47", file: "KobIfdBid", run: { [T_AU_IB]: ["NAU-IB29"] }, note: "refund: the maker gets the escrow minus at most refundTip", edits: [del("        require(tx.outputs[self].value + refundTip >= tx.inputs[self].value);\n")] },
], { batteries: BATTERIES, skip: NOT_ON_3x3 });
