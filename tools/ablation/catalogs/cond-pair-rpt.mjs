// KobCondPair repeat / merge checks (contracts/v2/KobCondPair.sil), the ones a KobIfdPair re-arm (merge) exercises on the
// EXIT input. Test binary: kob_ifd_pair_tests (the re-arm scenarios). The other KobCondPair checks (plain stop / limit /
// trailing fills, the evidence battery, strays, custody, non-repeat continuation) are the `cond-pair` catalog of the other
// suite agent. Scenario ids NCR.. (and the NIX.. merge scenarios of ifd_merge_rules); each runs on both mixed pairs.
import { del, rep, rx } from '../lib/edits.mjs';

export const suite = {
  name: 'cond-pair-rpt',
  family: 'KobCondPair repeat / merge (exit of KobIfdPair)',
  testBin: 'kob_ifd_pair_tests',
  srcDir: 'contracts/v2',
  templateMarker: null,
  expectedTemplates: 1,
};

const F = 'KobCondPair';
const T = 'cond_rpt_profit';
const TR = 'cond_rpt_rules';
const A_UPDATE = '        if (update) {\n            // UPDATE: never next to the repeat entry';
// the exit's check of the entry's merge push
const PUSH_X = '                    require(int(byte[8](OpTxInputScriptSigSubstr(pin, 1, 9))) == 0 - (self * MERGE_K + n));\n';
// the S output pin (refund, rest or return)
const S_PIN = `                require(tx.outputs[outIdx].scriptPubKey == byte[](new ScriptPubKeyP2SHFromRedeemScript(
                    trs.slice(0, sPre) + tokenState(sFamily, outAmount, outOwner, rest, gx) + trs.slice(sPre + sLen, trs.length)
                )));
`;

const MAKER_DELIVERY = `                require(tx.outputs[self].scriptPubKey == byte[](new ScriptPubKeyP2SHFromRedeemScript(
                    oPre + tokenState(oFam, dOut, byte[32](maker), false, oGx) + oSuf
                )));
`;

export const mutations = [
  // ---- the re-arm pays the maker its profit
  { id: 'CR1', file: F, test: T, expect: ['NCR1'], note: 'a re-arming take-profit pays the maker exactly its profit dOut at output i (ASK exit: tOut - budget). Removing the maker-delivery pin lets the re-arm skim the profit.', edits: [del(MAKER_DELIVERY)] },
  // ---- a booked exit takes profit without its entry only from rptUntil
  { id: 'CR2', file: F, test: 'cond_rpt_until', expect: ['NCRU'], note: 'a booked exit (parent set) takes profit on leg 0 without its entry present only once tx.daa >= rptUntil; before that it must run together with its entry merge', edits: [del('                            require(tx.daa >= rptUntil);\n')] },
  // ---- completion (p2t_ifd2): the repeat rules of the exit (test fns cond_rpt_rules, ifd_merge_rules)
  { id: 'CR3', file: F, test: TR, expect: ['NCR4'], note: 'update: a booked exit is never armed next to its repeat entry (here: next to a fill of the entry)', edits: [rx(A_UPDATE, /require\(OpCovInputCount\(parent\) == 0\);/, '')] },
  { id: 'CR4', file: F, test: TR, expect: ['NCR5'], note: "the stop leg never re-arms: a booked exit's stop fill is refused next to its entry (here its merge: the matcher would fund a re-arm after a stop-loss)", edits: [del('                        require(present == 0);\n')] },
  { id: 'CR6', file: F, test: TR, expect: ['NCR9'], inputOnly: ['NCR9'], note: "the entry's sigscript starts with an 8-byte push (a 9-byte push carrying the merge argument refused; the entry's own byte[8] argument refuses it too)", edits: [del('                    require(OpTxInputScriptSigSubstr(pin, 0, 1) == byte[](0x08));\n')] },
  { id: 'CR7', file: F, hold: { ifd_merge_rules: ['NIX7e'] }, note: "the entry's pushed merge argument names this exit and its n. DEFENCE IN DEPTH with the entry's check of this exit's push (KobIfdPair IF110): an entry merging m != n stays rejected by the entry (NIX7e); ifd-pair IF110c removes both", edits: [del(PUSH_X)] },
  { id: 'CR8', file: F, test: TR, expect: ['NCR6'], inputOnly: ['NCR6'], note: 're-arm: the maker profit is positive (a committed take-profit at the entry price gives 0: refused; a 0-amount token output is also refused by the KRON program, so on KCC/KRON only the exit input flips)', edits: [del('                    require(dOut > 0);\n')] },
  { id: 'CR9', file: F, hold: { [TR]: ['NCR7'] }, note: 'a sold-out re-arm leaves no S: DEFENCE IN DEPTH with the S-output pin (outIdx = output i, where the profit output sits: the two pins collide). CR9c removes both', edits: [del('                        require(outAmount == 0);\n')] },
  { id: 'CR9c', file: F, test: TR, expect: ['NCR7'], note: 'outAmount == 0 AND the S-output pins (script, covenant) removed: a look-alike exit holding one A more than its amountLeft sells out and the extra A goes to the matcher', edits: [del('                        require(outAmount == 0);\n'), del(S_PIN), del('                require(OpOutputCovenantId(outIdx) == sCovId);\n')] },
  { id: 'CR10', file: F, test: TR, expect: ['NCR8'], note: "a sold-out re-arm: the maker's profit output carries deliveryCarrier (the rest of the exit's KAS is the entry's)", edits: [rep('                        require(outAmount == 0);\n                        require(tx.outputs[self].value >= deliveryCarrier);\n', '                        require(outAmount == 0);\n')] },
  { id: 'CR11', file: F, test: 'ifd_merge_rules', expect: ['NIX9'], note: "the exit's S stray guard keeps the entry's xc honest: xc named at a genuine A UTXO owned by the exit (with one sompi of KAS) passes the entry's heldGx, only the exit refuses it as a stray (duplicate of cond-pair G1, witnessed by a merge)", edits: [del('        noStrays(sCovId, selfId, cust, sOff);\n')] },
];


// Coverage of the repeat / merge requires of KobCondPair.settle (the other KobCondPair checks: cond-pair.mjs):
//  update `OpCovInputCount(parent) == 0` CR3; leg 0 without the entry `tx.daa >= rptUntil` CR2; a stop fill's
//  `present == 0` CR4; the entry's first push 0x08 CR6; the entry's merge argument CR7 (hold) + ifd-pair IF110c
//  (combined); `dOut > 0` CR8; the re-arming sell-out's `outAmount == 0` CR9 (hold) + CR9c (combined) and its profit
//  output's deliveryCarrier CR10; the maker's profit pin CR1. CR11 re-witnesses the exit's S stray guard (cond-pair G1)
//  through a merge's xc.
//
// Not in the catalog:
//  * `require(n < MERGE_K)` in the re-arm branch: not reachable by a genuine booked exit. KobIfdPair books an exit only
//    for n < 2^53 (ifd-pair IF115, witnessed by NIF12k), and a booked exit's amountLeft only shrinks. A look-alike exit
//    (any KobCondPair whose committed bytes equal the entry's, funded by whoever creates it) with amountLeft >= 2^53
//    could, without this check, name the entry's push of a second look-alike at input k + 1. It would re-arm without
//    the entry crediting its own budget. That budget is the look-alike's own custody, i.e. the attacker's own tokens, so
//    no third party's value is at stake. The witness needs two KCC-20 look-alike exits of more than 2^53 base units in
//    one transaction, and it was not built (left as a leftover). Hardening only.
