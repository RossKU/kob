// KobIfdPair pair order (contracts/v2/KobIfdPair.sil): IF-DONE / repeat entry, both sides and both families.
// Test binary: kob_ifd_pair_tests. Scenario ids NI..; each attack flips on both mixed pairs (KCC/KRON, KRON/KCC).
// The exit-side (KobCondPair) repeat / merge checks a merge scenario of this binary flips live in cond-pair-rpt.mjs.
// Ids with a `k` / `r` suffix exist only where the token read is KCC-20 / KRON; NIH4 / NIP1 run on KCC/KCC (two tokens of
// one program: only the covenant id tells them apart), NIS8 on a 16-input KCC-20 program, NIF12k on KCC/KRON.
//
// Coverage: every `require(` of KobIfdPair.sil (111, the eight noStrays slot lines included) has an entry below, except:
//  - fill branch `require(n > 0)`: unreachable. The branch is entered only when nb is neither 0 (refund / update) nor
//    negative (merge), so n > 0 always holds there.
//  - cancel `require(sb.length == 65)`: not independently load-bearing. A shorter signature fails `sb[64]` (index out of
//    range), and a longer one is no Schnorr signature for checkSig, so removing it accepts no transaction.
// Defence in depth (hold entries plus one combined entry each): the exit spk pin / genesis id (IF9 / IF10 / IF9b), the
// exit prefix hash (IF11), pinTok's spk / covenant pins for a custody moved off its index (IF8b / IF47 / IF8c), the
// entry <-> exit push cross-check (IF110 / cond-pair-rpt CR7 / IF110c), m > 0 next to a take-profit (IF104 / IF104c).
// Reachable only by the order's own hostile state (maker-set fields the builders refuse), witnessed as such: side 3,
// a negative scale, family 3, negative tip / carriers / prefund / keeperTip / refundTip, price 0.
import { del, rep, rx } from '../lib/edits.mjs';

export const suite = {
  name: 'ifd-pair',
  family: 'KobIfdPair pair order',
  testBin: 'kob_ifd_pair_tests',
  srcDir: 'contracts/v2',
  templateMarker: null,
  expectedTemplates: 1,
};

const F = 'KobIfdPair';
const T_BUY = 'ifd_fill_buy';
const T_SELL = 'ifd_fill_sell';
const T_CUST = 'ifd_custody';
const T_STRAY = 'ifd_strays';
const T_GEN = 'ifd_exit_genesis';
const T_ALIAS = 'ifd_aliasing';
const T_ENC = 'ifd_encoding';
const T_UPD = 'ifd_update';
const T_REF = 'ifd_refund';
const T_FR = 'ifd_fill_rules';
const T_OUT = 'ifd_outputs';
const T_HOS = 'ifd_hostile';
const T_UR = 'ifd_update_rules';
const T_RR = 'ifd_refund_rules';
const T_EVT = 'ifd_ev_template';
const T_CM = 'ifd_custody_marks';
const T_SS = 'ifd_stray_slots';
const T_EVB = 'ifd_ev_battery';
const T_MR = 'ifd_merge_rules';
const T_MRF = 'ifd_merge_refund';
const T_LIM = 'ifd_limits';
// anchors of function-scoped edits
const A_TPL = '    function tplState(int idx, int pre, int stLen, int suf, byte[32] tpl) : byte[] {';
const A_RD = '    function rd(int ev, bool ask, bool pair, byte[32] tok, int fam, int sc, int kSuf, byte[32] oTok, int oFam, int osc, int oSuf, int tk, int minN) : int {';
const A_EVLEG = '    function evLeg(int ev, bool ask, byte[32] tok, int fam, int sc, int tk, int tokSuf, int minN) : int {';
const A_STOP = '    function stopTouch(int evA, int evB, int tk, int evMode) {';
// the entry's check of the exit's push (KobIfdPair) and the exit's check of the entry's push (KobCondPair)
const PUSH_M = '                require(int(byte[8](OpTxInputScriptSigSubstr(k, 1, 9))) == m);\n';
const PUSH_X = '                    require(int(byte[8](OpTxInputScriptSigSubstr(pin, 1, 9))) == 0 - (self * MERGE_K + n));\n';

const GENID = `                require(exitId == blake2bWithKey(
                    byte[](OpOutpointTxId(self)) + byte[](OpOutpointIndex(self) as byte[4]) + byte[](1 as byte[8])
                        + byte[](exitOut as byte[4]) + byte[](tx.outputs[exitOut].value as byte[8]) + exitSpk.slice(0, 2)
                        + byte[](35 as byte[8]) + exitSpk.slice(2, 37),
                    byte[]("CovenantID")
                ));
`;

const PIN_SPK = /require\(tx\.outputs\[o\]\.scriptPubKey == byte\[\]\(new ScriptPubKeyP2SHFromRedeemScript\([\s\S]*?\)\)\);/;

export const mutations = [
  // ---- fill arithmetic (per side)
  { id: 'IF1', file: F, test: T_BUY, expect: ['NI01'], note: 'buy-first: the B released is at most floor(n * p / scale(A))', edits: [del('                    require(amt <= quoteOf(n, p, aScale, 0));\n')] },
  { id: 'IF2', file: F, test: T_SELL, expect: ['NI05'], note: 'sell-first: the proceeds put into the exit custody are at least ceil(n * p / scale(A))', edits: [del('                    require(amt >= quoteOf(n, p, aScale, aScale - 1));\n')] },
  // ---- carriers / continuation
  { id: 'IF3', file: F, run: { [T_BUY]: ['NI03'], [T_SELL]: ['NI06'] }, note: 'the exit custody (output i) carries deliveryCarrier', edits: [del('                require(tx.outputs[self].value >= deliveryCarrier);\n')] },
  { id: 'IF4', file: F, run: { [T_BUY]: ['NI04'], [T_SELL]: ['NI07'], ifd_merge_rules: ['NIM5'] }, note: 'the continuation holds at least its floor (also the update keeper floor, NI30)', edits: [del('            require(tx.outputs[c].value >= floor);\n')] },
  { id: 'IF12', file: F, test: T_UPD, expect: ['NI30'], note: 'an arming keeper takes at most keeperTip from the entry UTXO', edits: [rep('                floor = tx.inputs[self].value - keeperTip;', '                floor = tx.inputs[self].value - keeperTip - keeperTip;')] },
  // ---- exact custodies
  { id: 'IF5', file: F, test: T_CUST, expect: ['NI09', 'NI10', 'NI11'], note: 'each custody (A, B escrow, B prefund) holds exactly its state amount (heldGx)', edits: [del('        require(got == amount);\n')] },
  // ---- strays of both tokens (fill, refund, update)
  { id: 'IF6', file: F, test: T_STRAY, expect: ['NI12', 'NI15'], note: 'no input of A owned by this id other than the A custody (fill, update)', edits: [del('        noStrays(aCovId, selfId, ac, aOff);\n')] },
  { id: 'IF7', file: F, run: { [T_STRAY]: ['NI13', 'NI14'], [T_ENC]: ['NI25'] }, note: 'no input of B owned by this id other than the B custody (fill, refund, update spends none)', edits: [del('        noStrays(bCovId, selfId, bc, bOff);\n')] },
  // ---- the exit (fresh genesis, committed state, custody)
  { id: 'IF8', file: F, run: { [T_GEN]: ['NI17'], [T_ALIAS]: ['NI23'] }, note: "the exit's custody of n (buy) / proceeds+prefund (sell) is pinned at output i (pinTok)", edits: [del('                pinTok(self, xSrc, xT, xH, xP, xL, xS, xF, xCust, exitId, true, zg + byte[](xE));\n')] },
  { id: 'IF9', file: F, hold: { [T_GEN]: ['NI18'] }, note: 'the exit output equals the committed exit state (amountLeft := n, custody, repeat fields): DEFENCE IN DEPTH with the genesis id IF10 and the consensus genesis binding. Any deviation of the exit spk changes its consensus covenant id, so the output cannot both carry a wrong spk and the id blake2b(exitSpk) requires (IF9b removes both and flips NI18).', edits: [del('                require(tx.outputs[exitOut].scriptPubKey == exitSpk);\n')] },
  { id: 'IF9b', file: F, test: T_GEN, expect: ['NI18'], note: 'the exit spk pin and the genesis id together bind amountLeft := n (removing both lets the exit commit amountLeft = n + 1)', edits: [del('                require(tx.outputs[exitOut].scriptPubKey == exitSpk);\n'), del(GENID)] },
  { id: 'IF10', file: F, test: T_GEN, expect: ['NI19'], hold: { [T_GEN]: ['NI18'] }, note: 'the exit is the fresh single-output genesis this input derives (keyed blake2b over the outpoint and the one output): a two-output genesis group has a different consensus id. Holds NI18 (the spk pin IF9 also catches an edited amountLeft).', edits: [del(GENID)] },
  { id: 'IF11', file: F, hold: { [T_GEN]: ['NI20'] }, note: 'the exit prefix hashes to COND_TPL: DEFENCE IN DEPTH. A wrong prefix also changes the built exitSpk (caught by IF9) and the genesis id (IF10), so it is triple-guarded; removing this check alone leaves NI20 rejected.', edits: [del('                require(blake3(byte[](cPre.length as byte[8]) + cPre + byte[](cSuf.length as byte[8]) + cSuf) == COND_TPL);\n')] },
  // ---- encoding / self id
  { id: 'IF13', file: F, test: T_ENC, expect: ['NI26'], note: 'update: upd is 0 or 1 (upd == 1 in the update branch)', edits: [del('            require(upd == 1);\n')] },
  { id: 'IF14', file: F, test: T_ENC, expect: ['NI28'], inputOnly: ['NI28'], note: 'the entry is the only input of its covenant id (the attack plants a second input of the id; that sibling input also rejects, so only the order input flips)', edits: [del('        require(OpCovInputCount(selfId) == 1);\n')] },
  // ---- merge: the entry's custody growth
  { id: 'IF17', file: F, test: 'ifd_merge', expect: ['NIM1'], note: 'a merge into an empty buy-first entry funds the NEW B escrow with exitCarrier', edits: [del('                        require(tx.outputs[bOut].value >= exitCarrier);\n')] },
  { id: 'IF18', file: F, test: 'ifd_merge', expect: ['NIM2'], note: 'a merge into an empty sell-first entry funds the NEW A custody with exitCarrier', edits: [del('                        require(tx.outputs[aOut].value >= exitCarrier);\n')] },
  { id: 'IF19', file: F, test: 'ifd_merge', expect: ['NIM3'], note: "the merged buy-first escrow grows by exactly budget(m) (the B output is pinned to custody + budget)", edits: [del('            pinTok(bIdx, bTplIn, bCovId, bTplHash, bPre, bLen, bSuf, bFamily, bNew, own, cov, bGx);\n')] },
  { id: 'IF20', file: F, test: 'ifd_merge', expect: ['NIM4'], note: 'the merged sell-first A custody grows by exactly m (the A output is pinned to amountLeft + m)', edits: [del('            pinTok(aIdx, aTplIn, aCovId, aTplHash, aPre, aLen, aSuf, aFamily, aNew, own, cov, aGx);\n')] },
  // ---- stop-entry trigger evidence (exposure >= minRestDaa)
  { id: 'IF21', file: F, test: 'ifd_evidence', expect: ['NEV1', 'NEV3'], note: 'rd(): the first evidence read (a pair order in mode 1, the A leg in mode 0) was exposed >= minRestDaa before the fill', edits: [rx('    function rd(int ev, bool ask, bool pair,', /require\(tx\.daa >= exposed \+ minRestDaa\);/, '')] },
  { id: 'IF22', file: F, test: 'ifd_evidence', expect: ['NEV2'], note: 'evLeg(): the mode-0 B leg (a KAS-book order of B) was exposed >= minRestDaa before the fill', edits: [rx('    function evLeg(int ev, bool ask, byte[32] tok,', /require\(tx\.daa >= exposed \+ minRestDaa\);/, '')] },
  // ---- refund
  { id: 'IF15', file: F, test: T_REF, expect: ['NI33'], note: 'refund only from the expiry / 90-day idle time', edits: [del('                require(tx.daa >= idleEnd);\n')] },
  { id: 'IF16', file: F, test: T_REF, expect: ['NI34'], note: 'refund pays the maker everything but refundTip', edits: [del('                require(tx.outputs[self].value + refundTip >= tx.inputs[self].value);\n')] },

  // ================================================================ completion (p2t_ifd2)
  // ---- fill rules (ifd_fill_rules)
  { id: 'IF23', file: F, test: T_FR, expect: ['NIF01'], note: 'a fill waits for activeFrom (CLTV)', edits: [rep('                require(tx.daa >= activeFrom);\n                require(n > 0);\n', '                require(n > 0);\n')] },
  { id: 'IF24', file: F, test: T_FR, expect: ['NIF02'], note: 'n <= amountLeft (a fill of amountLeft + 1 would write an exit of more than the entry holds / buys)', edits: [del('                require(n <= amountLeft);\n')] },
  { id: 'IF25', file: F, test: T_FR, expect: ['NIF03'], note: 'n >= minFill unless the fill takes everything left (one base unit below minFill refused)', edits: [del('                require(n >= minFill || n == amountLeft);\n')] },
  { id: 'IF26', file: F, test: T_FR, expect: ['NIF04'], note: 'tip >= 0 (hostile field: a negative tip raises the continuation floor; the filler funds it)', edits: [del('                require(tip >= 0);\n')] },
  { id: 'IF27', file: F, test: T_FR, expect: ['NIF05'], note: 'deliveryCarrier >= 0 (hostile field)', edits: [del('                require(deliveryCarrier >= 0);\n')] },
  { id: 'IF28', file: F, test: T_FR, expect: ['NIF06'], note: 'exitCarrier >= 0 (hostile field)', edits: [del('                require(exitCarrier >= 0);\n')] },
  { id: 'IF29', file: F, test: T_FR, expect: ['NIF07'], note: 'fill: a buy-stop entry has entryStop <= price', edits: [del('                        require(entryStop <= price);\n')] },
  { id: 'IF30', file: F, test: T_FR, expect: ['NIF08'], note: 'fill: a sell-stop entry has entryStop >= price', edits: [del('                        require(entryStop >= price);\n')] },
  { id: 'IF31', file: F, test: T_FR, expect: ['NIF09'], note: 'the auction time t is proven by CLTV (tx.daa >= t)', edits: [rep('                            require(tx.daa >= t);\n                            require(t >= newArmed);\n', '                            require(t >= newArmed);\n')] },
  { id: 'IF32', file: F, test: T_FR, expect: ['NIF10'], note: 'the auction time t is not before the auction origin', edits: [del('                            require(t >= newArmed);\n')] },
  { id: 'IF33', file: F, test: T_FR, expect: ['NIF11'], note: 'the entry quote p is positive (a price-0 entry never trades)', edits: [del('                require(p > 0);\n')] },
  { id: 'IF34', file: F, test: T_FR, expect: ['NIF13'], note: 'a booking dates its cycle by a CLTV-proven t (rptUntil cannot be pushed out)', edits: [rep('                    require(tx.daa >= t);\n                    int utxoDaa', '                    int utxoDaa')] },
  { id: 'IF35', file: F, test: T_FR, expect: ['NIF14'], note: 'buy-first: the B released is not negative (sign check)', edits: [del('                    require(amt >= 0);\n')] },
  { id: 'IF36', file: F, test: T_FR, expect: ['NIF15'], note: 'buy-first: the release does not exceed the escrow (bNew >= 0; sign check)', edits: [del('                    require(bNew >= 0);\n')] },
  { id: 'IF37', file: F, test: T_FR, expect: ['NIF16'], note: 'sell-first: prefund >= 0 (hostile field: a negative pre(n) moves the exit custody into the prefund)', edits: [del('                    require(prefund >= 0);\n')] },
  { id: 'IF38', file: F, test: T_FR, expect: ['NIF17'], note: 'sell-first: a continuing fill takes pre(n) from a prefund custody that holds it', edits: [del('                        require(used <= custody);\n')] },
  // ---- outputs (ifd_outputs)
  { id: 'IF39', file: F, test: T_OUT, expect: ['NIO4'], note: 'a continuing fill: the exit UTXO carries exitCarrier', edits: [del('                    require(tx.outputs[exitOut].value >= exitCarrier);\n')] },
  { id: 'IF40', file: F, test: T_OUT, expect: ['NIO5'], note: 'a continuing fill: the A custody rest keeps its carrier', edits: [rep('\n                            require(tx.outputs[ac].value >= aCar);\n', '\n')] },
  { id: 'IF41', file: F, test: T_OUT, expect: ['NIO6'], note: 'a continuing fill: the B custody rest keeps its carrier', edits: [rep('\n                            require(tx.outputs[bc].value >= bCar);\n', '\n')] },
  { id: 'IF42', file: F, test: T_OUT, expect: ['NIO7'], note: 'a terminating buy-first fill: the escrow return to the maker keeps its carrier', edits: [rep('                        require(tx.outputs[bc].value >= bCar);\n                        own = byte[32](maker);', '                        own = byte[32](maker);')] },
  { id: 'IF43', file: F, test: T_OUT, expect: ['NIO8'], note: 'a terminating fill: the last exit takes all the KAS left', edits: [del('                    require(tx.outputs[exitOut].value >= left);\n')] },
  { id: 'IF44', file: F, test: T_OUT, expect: ['NIO1'], note: 'a continuing entry has exactly one output bound to its id', edits: [del('            require(OpCovOutputCount(selfId) == 1);\n')] },
  { id: 'IF45', file: F, test: T_OUT, expect: ['NIO2'], note: 'the continuation is this script with the new mutable window', edits: [del('            require(tx.outputs[c].scriptPubKey == contSpk(newArmed, newLeft, bNew, newRpt));\n')] },
  { id: 'IF46', file: F, test: T_OUT, expect: ['NIO3'], note: 'a terminated entry leaves no output bound to its id', edits: [del('            require(OpCovOutputCount(selfId) == 0);\n')] },
  { id: 'IF47', file: F, test: T_OUT, expect: ['NIO9'], hold: { [T_ALIAS]: ['NI23'], [T_OUT]: ['NIO10'] }, note: 'pinTok: the pinned token output is bound to the token covenant (an unbound look-alike of the exit custody refused). Holds NI23 / NIO10 (a custody moved off its index: the spk pin IF8b also refuses the plain output there; IF8c removes both)', edits: [del('        require(OpOutputCovenantId(o) == tok);\n')] },
  { id: 'IF8b', file: F, run: { [T_GEN]: ['NI17'], ifd_merge: ['NIM3', 'NIM4'] }, hold: { [T_ALIAS]: ['NI23'], [T_OUT]: ['NIO10'] }, note: 'pinTok: the pinned token output has exactly the token state on the token template (amount, owner). Holds NI23 / NIO10 (a custody moved off its index: the covenant-id pin IF47 also refuses the plain output there)', edits: [rx('    function pinTok(', PIN_SPK, '')] },
  { id: 'IF8c', file: F, run: { [T_ALIAS]: ['NI23'], [T_OUT]: ['NIO10'] }, note: 'pinTok spk pin AND covenant-id pin removed together: a custody (the exit custody, the escrow rest) may then sit at any index (IF8b / IF47 each alone hold)', edits: [rx('    function pinTok(', PIN_SPK, ''), del('        require(OpOutputCovenantId(o) == tok);\n')] },
  // ---- hostile state fields, cancel (ifd_hostile)
  { id: 'IF48', file: F, test: T_HOS, expect: ['NIH5'], note: 'side is 1 or 2 (hostile side 3 read as sell-first)', edits: [del('        if (!buy) { require(side == SIDE_ASK); }\n')] },
  { id: 'IF49', file: F, test: T_HOS, expect: ['NIH6'], note: 'scale(A) > 0 (a hostile negative scale makes every quote negative; zero divides by zero anyway)', edits: [del('        require(aScale > 0);\n')] },
  { id: 'IF50k', file: F, test: T_HOS, expect: ['NIH7k'], note: 'aFamily is 1 or 2 (hostile family 3 read as KCC-20)', edits: [del('            require(aFamily == FAM_KCC20);\n')] },
  { id: 'IF51k', file: F, test: T_HOS, expect: ['NIH8k'], note: 'bFamily is 1 or 2 (hostile family 3 read as KCC-20)', edits: [del('            require(bFamily == FAM_KCC20);\n')] },
  { id: 'IF52', file: F, test: T_HOS, expect: ['NIC1'], note: 'cancel: SIGHASH_ALL only', edits: [del('        require(sb[64] == SIGHASH_ALL);\n')] },
  { id: 'IF53', file: F, test: T_HOS, expect: ['NIC2'], note: 'cancel: the maker signs', edits: [rep('        require(checkSig(s, maker));', '        require(sb.length == 65);')] },
  // ---- update (ifd_update_rules)
  { id: 'IF54', file: F, test: T_UR, expect: ['NIU1'], note: 'update: only a stop entry (entryStop > 0)', edits: [del('                require(entryStop > 0);\n')] },
  { id: 'IF55', file: F, test: T_UR, expect: ['NIU2'], note: 'update: a buy-stop entry has entryStop <= price', edits: [rep('\n                    require(entryStop <= price);\n', '\n')] },
  { id: 'IF56', file: F, test: T_UR, expect: ['NIU3'], note: 'update: a sell-stop entry has entryStop >= price', edits: [rep('\n                    require(entryStop >= price);\n', '\n')] },
  { id: 'IF57', file: F, test: T_UR, expect: ['NIU4'], note: 'update: the entry has something left to trade', edits: [del('                require(amountLeft > 0);\n')] },
  { id: 'IF58', file: F, test: T_UR, expect: ['NIU5'], note: 'update: only an unarmed entry (an armed one would restart its auction at armed = 1)', edits: [del('                require(armed == 0);\n')] },
  { id: 'IF59', file: F, test: T_UR, expect: ['NIU6'], note: 'update: keeperTip >= 0 (hostile field)', edits: [del('                require(keeperTip >= 0);\n')] },
  { id: 'IF60', file: F, test: T_UR, expect: ['NIU7'], note: 'update: waits for activeFrom', edits: [rep('                require(keeperTip >= 0);\n                require(tx.daa >= activeFrom);\n', '                require(keeperTip >= 0);\n')] },
  { id: 'IF61', file: F, test: T_UR, expect: ['NIU8'], hold: { [T_ENC]: ['NI27'] }, note: 'upd 1 needs nb == 0: a stop fill run as an update (no custody spent, the matcher pays the B, the entry state shrinks below its UTXO) is refused. NI27 (an update tx with nb > 0, no exit) stays rejected by the exit checks.', edits: [del('            require(n == 0);\n')] },
  // ---- refund (ifd_refund_rules)
  { id: 'IF62', file: F, test: T_RR, expect: ['NIR1'], note: 'refund: refundTip >= 0 (hostile field: the keeper would top up the maker)', edits: [del('                require(refundTip >= 0);\n')] },
  { id: 'IF63', file: F, test: T_RR, expect: ['NIR2'], note: 'refund: the A custody returns with its carrier', edits: [del('                if (aHeld > 0) { require(tx.outputs[ac].value >= aCar); }\n')] },
  { id: 'IF64', file: F, test: T_RR, expect: ['NIR3'], note: 'refund: the B custody returns with its carrier', edits: [del('                if (custody > 0) { require(tx.outputs[bc].value >= bCar); }\n')] },
  { id: 'IF65', file: F, test: T_RR, expect: ['NIR4'], note: "refund: the KAS at output i is the maker's", edits: [del('                require(tx.outputs[self].scriptPubKey == byte[](new ScriptPubKeyP2PK(maker)));\n')] },
  // ---- tplState (every authenticated read: custodies, the exit custody xc, the evidence, the merge's exit)
  { id: 'IF66', file: F, test: T_EVT, expect: ['NE24'], note: 'tplState: the input is a P2SH spend of the redeem script it carries (a planted non-P2SH UTXO of the evidence id with the genuine KobAsk bytes refused)', edits: [rx(A_TPL, /require\(tx\.inputs\[idx\]\.scriptPubKey == byte\[\]\(new ScriptPubKeyP2SHFromRedeemScript\(rs\)\)\);/, '')] },
  { id: 'IF67', file: F, test: T_EVT, expect: ['NE23'], note: 'tplState: the redeem script is the template (hash of prefix and suffix; a look-alike evidence with the genuine state refused)', edits: [rx(A_TPL, /require\(blake3\([\s\S]*?\) == tpl\);/, '')] },
  // ---- heldGx (the custodies; also the exit custody xc of a sell-out merge)
  { id: 'IF68', file: F, test: T_CM, expect: ['NIH4'], note: 'heldGx: the custody input carries the custody token (KCC/KCC: the B custody named at the A custody input of the same program, the real prefund left behind)', edits: [del('        require(OpInputCovenantId(idx) == tok);\n')] },
  { id: 'IF69r', file: F, test: T_CM, expect: ['NIH1r'], note: 'heldGx KRON: the custody is owned by this entry (not another covenant)', edits: [del('            require(byte[32](st.slice(1, 33)) == me);\n')] },
  { id: 'IF70r', file: F, test: T_CM, expect: ['NIH2r'], inputOnly: ['NIH2r'], note: 'heldGx KRON: the custody owner is a covenant id (id_type 2); an id_type-3 custody is also refused by the KRON program (no key), so only the entry input flips', edits: [del('            require(st.slice(33, 36) == byte[](0x010208));\n')] },
  { id: 'IF71r', file: F, test: T_CM, expect: ['NIH3r'], note: 'heldGx KRON: the custody is not a minter', edits: [del('            require(st.slice(44, 46) == byte[](0x0100));\n')] },
  { id: 'IF72k', file: F, test: T_CM, expect: ['NIH1k'], note: 'heldGx KCC-20: the custody is owned by this entry (not another covenant)', edits: [del('            require(byte[32](st.slice(10, 42)) == me);\n')] },
  { id: 'IF73k', file: F, test: T_CM, expect: ['NIH2k'], note: 'heldGx KCC-20: the custody is covenant-owned (scheme 0x04) with borrowing disabled', edits: [del('            require(st.slice(43, 46) == byte[](0x040100));\n')] },
  { id: 'IF74', file: F, test: T_CM, expect: ['NIP1'], note: 'pinTok: the template source input carries the token (KCC/KCC: a B input as the A template source)', edits: [del('        require(OpInputCovenantId(src) == tok);\n')] },
  { id: 'IF75', file: F, test: T_CM, expect: ['NIP2'], inputOnly: ['NIP2'], note: 'pinTok: the template bytes read from the source hash to the token template (a planted look-alike source; the A program also refuses the output written on its bytes, so only the entry input flips)', edits: [del('        require(blake3(byte[](pre as byte[8]) + p + byte[](suf as byte[8]) + x) == th);\n')] },
  // ---- the stray scan, slot by slot (a refund of a buy-first entry; the KCC-20 token for slots 4..7, a 16-input program for the bound)
  { id: 'IF76', file: F, test: T_SS, expect: ['NIS8'], note: 'noStrays: at most MAX_TOK_IN (8) inputs of the token (the scan is unrolled for 8; a 16-input KCC-20 program carries a 9th)', edits: [del('        require(cnt <= MAX_TOK_IN);\n')] },
  ...[0, 1, 2, 3, 4, 5, 6, 7].map(j => ({ id: `IF77s${j}`, file: F, test: T_SS, expect: [`NIS${j}`], note: `noStrays: the input at token-input slot ${j} is not owned by this entry`, edits: [del(`require(byte[32](OpTxInputScriptSigSubstr(x${j}, e${j}, e${j} + 32)) != me);`)] })),
  // ---- the stop-entry evidence: rd() (the A leg in mode 0, the pair order in mode 1)
  { id: 'IF85', file: F, test: T_EVB, expect: ['NE03a0', 'NE03b0', 'NE03a1', 'NE03b1'], hold: { [T_EVB]: ['NE16a0', 'NE16b0', 'NE16a1', 'NE16b1'] }, note: 'rd: the evidence sigscript starts with the 8-byte fill push (a cancel whose signature reads as a fill refused; an index at a P2PK input also fails the template read)', edits: [rx(A_RD, /require\(OpTxInputScriptSigSubstr\(ev, 0, 1\) == byte\[\]\(0x08\)\);/, '')] },
  { id: 'IF86', file: F, test: T_EVB, expect: ['NE01a0', 'NE01b0', 'NE01a1', 'NE01b1'], inputOnly: ['NE01a0', 'NE01b0', 'NE01a1', 'NE01b1'], note: 'rd: the evidence is filled (n > 0; an entry with minTouch 0 would otherwise arm on an unfilled order: the n = 0 push is its refund / update encoding, which the evidence order itself refuses here before expiry, so only the entry input flips)', edits: [rx(A_RD, /require\(n > 0\);/, '')] },
  { id: 'IF87', file: F, test: T_EVB, expect: ['NE07a0', 'NE07b0', 'NE07a1', 'NE07b1'], note: 'rd: the A / pair evidence fills at least minTouch', edits: [rx(A_RD, /require\(n >= minN\);/, '')] },
  { id: 'IF88', file: F, test: T_EVB, expect: ['NE09a0', 'NE09b0', 'NE09a1', 'NE09b1', 'NE11a0', 'NE11b0', 'NE11a1', 'NE11b1'], hold: { [T_EVB]: ['NE13a1', 'NE13b1'] }, note: "rd: the evidence's token and scale (mode 1: side, S, T and both scales) are the entry's. NE13 (the opposite pair order) stays rejected: its custody is the other token (the tk read)", edits: [rx(A_RD, /require\(got == want\);/, '')] },
  { id: 'IF89', file: F, test: T_EVB, expect: ['NE05a0', 'NE05b0', 'NE05a1', 'NE05b1'], note: 'rd: the evidence is not decaying (slope 0)', edits: [rx(A_RD, /require\(int\(byte\[8\]\(o\.slice\(208 \+ w, 216 \+ w\)\)\) == 0\);/, '')] },
  { id: 'IF90', file: F, test: T_EVB, expect: ['NE18a0', 'NE18a1', 'NE18b1'], note: "rd: the ask / pair evidence's custody tk carries its S token", edits: [rx(A_RD, /require\(OpInputCovenantId\(tk\) == cTok\);/, '')] },
  { id: 'IF91', file: F, test: T_EVB, expect: ['NE19a0', 'NE19a1', 'NE19b1'], note: 'rd: tk is owned by the evidence order with the covenant marker', edits: [rx(A_RD, /require\(OpTxInputScriptSigSubstr\(tk, end, end \+ 32 \+ mark\.length\) == byte\[\]\(OpInputCovenantId\(ev\)\) \+ mark\);/, '')] },
  // ---- the stop-entry evidence: evLeg() (the B leg in mode 0)
  { id: 'IF92', file: F, test: T_EVB, expect: ['NE04a0', 'NE04b0'], note: 'evLeg: the B evidence sigscript starts with the 8-byte fill push (a cancel read as a fill refused)', edits: [rx(A_EVLEG, /require\(OpTxInputScriptSigSubstr\(ev, 0, 1\) == byte\[\]\(0x08\)\);/, '')] },
  { id: 'IF93', file: F, test: T_EVB, expect: ['NE02a0', 'NE02b0'], inputOnly: ['NE02a0', 'NE02b0'], note: 'evLeg: the B evidence is filled (n > 0; minTouch 0 makes the B threshold 0; the unfilled order itself refuses its n = 0 push here, so only the entry input flips)', edits: [rx(A_EVLEG, /require\(n > 0\);/, '')] },
  { id: 'IF94', file: F, test: T_EVB, expect: ['NE08a0', 'NE08b0'], note: 'evLeg: the B evidence fills at least ceil(minTouch * entryStop / scale(A))', edits: [rx(A_EVLEG, /require\(n >= minN\);/, '')] },
  { id: 'IF95', file: F, test: T_EVB, expect: ['NE12a0', 'NE12b0'], note: 'evLeg: the B evidence is of token B', edits: [rx(A_EVLEG, /require\(byte\[32\]\(o\.slice\(34, 66\)\) == tok\);/, '')] },
  { id: 'IF96', file: F, test: T_EVB, expect: ['NE10a0', 'NE10b0'], note: "evLeg: the B evidence quotes per B's scale", edits: [rx(A_EVLEG, /require\(int\(byte\[8\]\(q\.slice\(0, 8\)\)\) == sc\);/, '')] },
  { id: 'IF97', file: F, test: T_EVB, expect: ['NE06a0', 'NE06b0'], note: 'evLeg: the B evidence is not decaying', edits: [rx(A_EVLEG, /require\(int\(byte\[8\]\(r\.slice\(18, 26\)\)\) == 0\);/, '')] },
  { id: 'IF98', file: F, test: T_EVB, expect: ['NE18b0'], note: "evLeg: the B ask evidence's custody tk carries token B", edits: [rx(A_EVLEG, /require\(OpInputCovenantId\(tk\) == tok\);/, '')] },
  { id: 'IF99', file: F, test: T_EVB, expect: ['NE19b0'], note: 'evLeg: tk is owned by the B ask evidence with the covenant marker', edits: [rx(A_EVLEG, /require\(OpTxInputScriptSigSubstr\(tk, end, end \+ 32 \+ mark\.length\) == byte\[\]\(OpInputCovenantId\(ev\)\) \+ mark\);/, '')] },
  // ---- the stop-entry evidence: stopTouch()
  { id: 'IF100', file: F, test: T_EVB, expect: ['NE21a1', 'NE21b1'], note: 'stopTouch: evMode is 0 or 1', edits: [rx(A_STOP, /require\(evMode == 1\);/, '')] },
  { id: 'IF101', file: F, test: T_EVB, expect: ['NE22b0'], note: 'stopTouch: the B quote is positive (an ask of B at price 0 would arm any buy stop: ceil(stop * 0 / scale(B)) = 0)', edits: [rx(A_STOP, /require\(b > 0\);/, '')] },
  { id: 'IF102', file: F, test: T_EVB, expect: ['NE17a0', 'NE17a1'], note: 'stopTouch: a sell stop arms only at a rate <= entryStop (one unit beyond refused; PE17 exactly at the stop accepted)', edits: [rx(A_STOP, /require\(a <= q\);/, '')] },
  { id: 'IF103', file: F, test: T_EVB, expect: ['NE17b0', 'NE17b1'], note: 'stopTouch: a buy stop arms only at a rate >= entryStop', edits: [rx(A_STOP, /require\(a >= q\);/, '')] },
  // ---- merge (ifd_merge_rules, ifd_merge_refund)
  { id: 'IF104', file: F, test: T_MRF, expect: ['NIM10'], hold: { [T_MR]: ['NIM9'] }, note: 'merge: m > 0 (a merge never reads the exit refund / update push n = 0 as an amount). NIM9 (m = 0 next to a take-profit of n) stays rejected by the push check IF110', edits: [del('                require(m > 0);\n')] },
  { id: 'IF104c', file: F, test: T_MR, expect: ['NIM9'], note: 'm > 0 AND the entry / exit push cross-checks removed: a merge of 0 next to a take-profit of n leaves the n bought A and back(n) to the matcher', edits: [del('                require(m > 0);\n'), del(PUSH_M)], also: [{ file: 'KobCondPair', edits: [del(PUSH_X)] }] },
  { id: 'IF105', file: F, test: T_MR, expect: ['NIX1'], note: 'merge: the exit at k carries the committed exit state (outside the mutable payloads): a look-alike with another tpPrice refused', edits: [del('                require(x.slice(0, 415) == st.slice(0, 415));\n')] },
  { id: 'IF106', file: F, test: T_MR, expect: ['NIX2'], note: 'merge: the exit is booked by this entry (push opcodes, parent, rptPrice = price, rptPre): another rptPrice refused', edits: [rx('                require(x.slice(423, 424)', /require\(x\.slice\(423, 424\)[\s\S]*?\);/, '')] },
  { id: 'IF107', file: F, test: T_MR, expect: ['NIX3'], note: "merge: the committed exit is the entry's opposite side (a buy-first entry whose committed exit is a BID refused)", edits: [del('                require(int(byte[8](st.slice(34, 42))) == xSide);\n')] },
  { id: 'IF108', file: F, test: T_MR, expect: ['NIX4'], note: "merge: the committed exit quotes at the entry's scale of A (an exit at scale 999 refused: the two would round the merge differently)", edits: [del('                require(int(byte[8](st.slice(xsOff, xsOff + 8))) == aScale);\n')] },
  { id: 'IF109', file: F, test: T_MR, expect: ['NIX5'], inputOnly: ['NIX5'], hold: { [T_MR]: ['NIX6'] }, note: "merge: the exit's sigscript starts with an 8-byte push (a 9-byte push whose bytes [1..9) are m refused; the exit's own byte[8] argument refuses it too). NIX6 (a minimal push) stays rejected by the amount check", edits: [del('                require(OpTxInputScriptSigSubstr(k, 0, 1) == byte[](0x08));\n')] },
  { id: 'IF110', file: F, hold: { [T_MR]: ['NIX7'] }, note: "merge: the exit's pushed n is m. DEFENCE IN DEPTH with the exit's own check of the entry's push (KobCondPair, cond-pair-rpt CR7): an entry merging m != n stays rejected by the exit (NIX7, judged at the exit); IF110c removes both", edits: [del(PUSH_M)] },
  { id: 'IF110c', file: F, test: T_MR, expect: ['NIX7', 'NIX7e'], note: "the entry's check of the exit's push AND the exit's check of the entry's push removed: an entry merging m = n - 1 leaves budget(1 whole A) and the exit's KAS to the matcher", edits: [del(PUSH_M)], also: [{ file: 'KobCondPair', edits: [del(PUSH_X)] }] },
  { id: 'IF111', file: F, test: T_MR, expect: ['NIM8'], note: 'merge: the buy-first budget(m) is positive (a price-0 entry re-armed with no escrow growth refused)', edits: [del('                    require(budget > 0);\n')] },
  { id: 'IF112', file: F, test: T_MR, expect: ['NIM6'], note: 'merge into an existing A custody: it keeps its carrier', edits: [rep('                    if (aHeld > 0) {\n                        require(tx.outputs[ac].value >= aCar);\n', '                    if (aHeld > 0) {\n')] },
  { id: 'IF113', file: F, test: T_MR, expect: ['NIM7'], note: 'merge into an existing B custody: it keeps its carrier', edits: [rep('                    if (custody > 0) {\n                        require(tx.outputs[bc].value >= bCar);\n', '                    if (custody > 0) {\n')] },
  { id: 'IF114', file: F, test: T_MR, expect: ['NIX11'], note: "merge sell-out: the exit custody input xc is authenticated (heldGx): xc named at a P2PK input would shrink the entry's claim of the exit's KAS", edits: [del("                    heldGx(xc, xTok, xTh, xPr, xLn, xSf, xFm, OpInputCovenantId(k), int(byte[8](x.slice(442, 450))), zg, xsExt);\n")] },
  // ---- limits
  { id: 'IF115', file: F, test: T_LIM, expect: ['NIF12k'], note: 'a booking needs n < 2^53 (the merge argument): a KCC-20 A booked for 2^53 refused (PIF12k books 2^53 - 1)', edits: [del('                    require(n < MERGE_K);\n')] },
];
