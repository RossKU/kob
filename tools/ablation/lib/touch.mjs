// Touch-trigger (v2.6) mutations shared by the four order catalogs. Every conditional order / stop entry
// reads its trigger evidence in one of three functions: touchAsk(ev, tk), touchBid(ev) or touch(ev, tk)
// (KobCondAsk / KobCondBid update, either side). The same checks appear in each (the KRON twins differ only
// in byte offsets and the custody owner encoding), so one generator emits one mutation per check, scoped to
// the function by its header (rx: from the header to the next entry / function declaration).
//
//   touchMutations({ prefix, file, fn, runs, notes })
//     prefix  id prefix, e.g. 'TA' -> TA-push08, TA-npos, ...
//     file    contract (.sil name)
//     fn      the function header line, e.g. 'function touchAsk(int ev, int tk) : int {'
//     runs    { check: { testFn: [scenario ids] } }; only the checks listed are emitted. A value may also be
//             { run: {...}, hold: {...}, inputOnly: [...] } for defence-in-depth entries.
//
// Checks: push08 (the fill argument is a fixed 8-byte push), npos (n > 0), tpl (template hash of the evidence),
// p2sh (the evidence UTXO is the P2SH of that redeem script), token, scale (the evidence quotes per the same scale),
// mintouch (n >= minTouch base units),
// slope (== 0), cltv (tx.daa >= exposed + minRestDaa), interval / active / tokdaa (the three exposure terms),
// tkcov (the custody carries the token's covenant id), owner (the custody is owned by the evidence ask, scheme
// covenant id).
import { rep, rx, rxDel } from './edits.mjs';

const TPL_END = '        return rs.slice(pre, pre + stLen);\n    }\n';

// tplState without the template-hash check (P2SH only) and without the P2SH check (template only)
const NO_HASH = `
    function tplStateNoHash(int idx, int pre, int stLen, int suf) : byte[] {
        int size = pre + stLen + suf;
        int base = OpTxInputScriptSigLen(idx) - size;
        byte[] rs = OpTxInputScriptSigSubstr(idx, base, base + size);
        require(tx.inputs[idx].scriptPubKey == byte[](new ScriptPubKeyP2SHFromRedeemScript(rs)));
        return rs.slice(pre, pre + stLen);
    }
`;
const NO_P2SH = `
    function tplStateNoP2sh(int idx, int pre, int stLen, int suf, byte[32] tpl) : byte[] {
        int size = pre + stLen + suf;
        int base = OpTxInputScriptSigLen(idx) - size;
        byte[] rs = OpTxInputScriptSigSubstr(idx, base, base + size);
        require(blake3(byte[](pre as byte[8]) + rs.slice(0, pre) + byte[](suf as byte[8]) + rs.slice(pre + stLen, size)) == tpl);
        return rs.slice(pre, pre + stLen);
    }
`;

const CHECKS = {
  push08: { note: 'the evidence sigscript starts with the fixed 8-byte push of its fill argument (0x08)', edits: f => [rxDel(f, /^\s*require\(OpTxInputScriptSigSubstr\(ev, 0, 1\) == byte\[\]\(0x08\)\);$/m)] },
  npos: { note: 'the evidence fill argument n > 0', edits: f => [rxDel(f, /^\s*require\(n > 0\);$/m)] },
  tpl: {
    note: 'the evidence is an instance of the KobAsk / KobBid template (template hash; P2SH kept)',
    edits: f => [rep(TPL_END, TPL_END + NO_HASH), rx(f, /tplState\(ev, ([^,]+), ([^,]+), ([^,]+), [^)]+\)/, m => `tplStateNoHash(ev, ${m[1]}, ${m[2]}, ${m[3]})`)],
  },
  p2sh: {
    note: 'the evidence UTXO is the P2SH of the redeem script read (template hash kept)',
    edits: f => [rep(TPL_END, TPL_END + NO_P2SH), rx(f, /tplState\(ev, /, 'tplStateNoP2sh(ev, ')],
  },
  token: { note: 'the evidence trades this token (tokenCovId)', edits: f => [rxDel(f, /^\s*require\(byte\[32\]\(o\.slice\(34, 66\)\) == tokenCovId\);$/m)] },
  scale: { note: 'the evidence quotes per the same scale (prices comparable)', edits: f => [rxDel(f, /^\s*require\(int\(byte\[8\]\([oq]\.slice\([^)]*\)\)\) == scale\);$/m)] },
  mintouch: { note: 'the evidence fill is >= minTouch base units', edits: f => [rxDel(f, /^\s*require\(n >= minTouch\);$/m)] },
  slope: { note: 'the evidence does not decay (slope 0)', edits: f => [rxDel(f, /^\s*require\(int\(byte\[8\]\([or]\.slice\([^)]*\)\)\) == 0\);$/m)] },
  cltv: { note: 'exposure: tx.daa >= exposed + minRestDaa (CLTV)', edits: f => [rxDel(f, /^\s*require\(tx\.daa >= exposed \+ minRestDaa\);$/m)] },
  interval: {
    note: 'exposure counts the TWAP / DCA interval (UTXO DAA + interval)',
    edits: f => [rx(f, /int exposed = OpTxInputDaaScore\(ev\) \+ int\(byte\[8\]\([or]\.slice\([^)]*\)\)\);/, 'int exposed = OpTxInputDaaScore(ev);')],
  },
  active: { note: 'exposure counts the evidence activeFrom', edits: f => [rxDel(f, /^\s*if \(act > exposed\) \{ exposed = act; \}$/m)] },
  tokdaa: { note: 'exposure counts the evidence ask custody DAA', edits: f => [rxDel(f, /^\s*if \(td > exposed\) \{ exposed = td; \}$/m)] },
  tkcov: { note: 'the custody input tk carries the token covenant id', edits: f => [rxDel(f, /^\s*require\(OpInputCovenantId\(tk\) == tokenCovId\);$/m)] },
  owner: {
    note: 'the custody input tk is owned by the evidence ask (owner = its covenant id, scheme covenant id)',
    edits: f => [rxDel(f, /^\s*require\(OpTxInputScriptSigSubstr\(tk, end, end \+ \d+\) == [^\n]*\);$/m)],
  },
};

export function touchMutations({ prefix, file, fn, runs, what = '' }) {
  const out = [];
  for (const [check, r] of Object.entries(runs)) {
    const c = CHECKS[check];
    if (!c) throw new Error(`touch mutation: unknown check ${check}`);
    const spec = r.run || r.hold ? r : { run: r };
    const m = { id: `${prefix}-${check}`, file, note: `${what}${what ? ': ' : ''}${c.note}`, edits: c.edits(fn) };
    if (spec.run) m.run = spec.run;
    if (spec.hold) m.hold = spec.hold;
    if (spec.inputOnly) m.inputOnly = spec.inputOnly;
    out.push(m);
  }
  return out;
}
