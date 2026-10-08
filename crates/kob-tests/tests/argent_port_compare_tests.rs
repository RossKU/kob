//! Opcode-level comparison of the hand-written `KobAsk` and its idiomatic Argent port (docs/argent-port-compare.md,
//! docs/argent-feedback.md item 10).
//!
//! Three variants, compiled with the same constructor arguments (`contracts/v2/KobAsk.ctor.json`):
//!   hand       `contracts/v2/KobAsk.sil` (hand-written SilverScript);
//!   idiomatic  `contracts/argent/port-out/KobAsk.idiomatic.sil` (argentc output of `port/kob_ask_port_idiomatic.ag`);
//!   splice1    `contracts/argent/port/KobAsk.splice1.sil` (the idiomatic port with `become` lowered to a splice of the
//!              changed field, written by `argent_become_splice_tests.rs`).
//!
//! For each variant this writes an annotated disassembly to `contracts/argent/port/compare/KobAsk.<variant>.opcodes.txt`
//! and a per-section byte table to `contracts/argent/port/compare/KobAsk.sections.txt`:
//!   - every opcode with its offset in the redeem script and its push data;
//!   - the source statement it was compiled from (silverscript-lang `record_debug_infos`; the bytecode is asserted to be
//!     identical to a compile without debug info);
//!   - the entry it belongs to (`settle`, `cancel`) and the state field of each push in the state span;
//!   - a tag for every opcode that disappears when one construct is removed from the source and the script is compiled
//!     again (constructs removed one after the other; the listings before and after are matched by a longest common
//!     subsequence of (opcode, push data)). The byte ranges in the files are the offsets of the tagged opcodes.
//!
//! The committed files must be what this test writes. Regenerate with:
//!
//! ```text
//! KOB_WRITE_COMPARE=1 cargo test --release -p kob-tests --test argent_port_compare_tests
//! ```

mod common;

use kaspa_consensus_core::hashing::sighash::SigHashReusedValuesUnsync;
use kaspa_consensus_core::tx::PopulatedTransaction;
use silverscript_abi::ArtifactValue;
use silverscript_lang::ast::parse_contract_ast;
use silverscript_lang::compiler::{artifact_value_to_expr, compile_contract, CompileOptions};
use silverscript_lang::debug_info::{DebugStep, StepKind};

const CTOR: &str = "contracts/v2/KobAsk.ctor.json";
const OUT_DIR: &str = "contracts/argent/port/compare";
const THIS: &str = "crates/kob-tests/tests/argent_port_compare_tests.rs";

fn read(rel: &str) -> String {
    std::fs::read_to_string(common::repo_root().join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}")).replace("\r\n", "\n")
}

/// `src` with every `(needle, replacement)` applied; each needle must occur exactly `n` times.
fn edit(src: &str, edits: &[(String, String, usize)]) -> String {
    let mut s = src.to_string();
    for (needle, with, n) in edits {
        assert_eq!(s.matches(needle.as_str()).count(), *n, "the source changed: `{needle}` must occur {n} time(s)");
        s = s.replace(needle.as_str(), with);
    }
    s
}

fn cut(needle: &str, n: usize) -> (String, String, usize) {
    (needle.to_string(), String::new(), n)
}

// ---------------------------------------------------------------- compile with debug info

struct Step {
    start: usize,
    end: usize,
    line: usize,
    seq: u32,
}

struct Compiled {
    bytecode: Vec<u8>,
    /// (entry, start, end)
    entries: Vec<(String, usize, usize)>,
    steps: Vec<Step>,
    state: (usize, usize),
    fields: Vec<String>,
}

fn ctor() -> Vec<ArtifactValue> {
    serde_json::from_str(&read(CTOR)).expect("ctor json")
}

fn bytecode(src: &str, what: &str) -> Vec<u8> {
    common::bytecode(
        &common::compile_contract(src, &ctor(), CompileOptions::default()).unwrap_or_else(|e| panic!("compile {what}: {e:?}")),
    )
}

fn compile_debug(src: &str, what: &str) -> Compiled {
    let ast = parse_contract_ast(src).unwrap_or_else(|e| panic!("parse {what}: {e:?}"));
    let args: Vec<_> = ctor()
        .iter()
        .zip(&ast.params)
        .map(|(v, p)| artifact_value_to_expr(v, &p.type_ref, &ast).expect("constructor argument"))
        .collect();
    assert_eq!(args.len(), ast.params.len(), "{what}: constructor argument count");
    let c = compile_contract(src, &args, CompileOptions { record_debug_infos: true, ..Default::default() })
        .unwrap_or_else(|e| panic!("compile {what}: {e:?}"));
    assert_eq!(c.bytecode, bytecode(src, what), "{what}: debug info must not change the bytecode");
    let d = c.debug_info.as_ref().expect("debug info");
    let steps = d
        .steps
        .iter()
        .filter(|s: &&DebugStep| matches!(s.kind, StepKind::Source {}) && s.bytecode_end > s.bytecode_start)
        .map(|s| Step { start: s.bytecode_start, end: s.bytecode_end, line: s.span.line as usize, seq: s.sequence })
        .collect();
    let mut entries: Vec<_> = d.functions.iter().map(|f| (f.name.clone(), f.bytecode_start, f.bytecode_end)).collect();
    entries.sort_by_key(|e| e.1);
    Compiled {
        bytecode: c.bytecode.clone(),
        entries,
        steps,
        state: (c.state_layout.start, c.state_layout.len),
        fields: ast.fields.iter().map(|f| f.name.clone()).collect(),
    }
}

// ---------------------------------------------------------------- disassembly

#[derive(Clone)]
struct Op {
    off: usize,
    len: usize,
    code: u8,
    name: String,
    data: Vec<u8>,
}

fn disasm(bc: &[u8]) -> Vec<Op> {
    let mut ops = vec![];
    let mut off = 0;
    for op in kaspa_txscript::parse_script::<PopulatedTransaction<'static>, SigHashReusedValuesUnsync>(bc) {
        let op = op.expect("parse opcode");
        let code = op.value();
        let data = op.get_data().to_vec();
        let len = match code {
            0x01..=0x4b => 1 + data.len(),
            0x4c => 2 + data.len(),
            0x4d => 3 + data.len(),
            0x4e => 5 + data.len(),
            _ => 1,
        };
        let name = op.to_string().split(' ').next().unwrap().to_string();
        ops.push(Op { off, len, code, name, data });
        off += len;
    }
    assert_eq!(off, bc.len(), "the disassembly covers the script");
    ops
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn show_data(d: &[u8]) -> String {
    if d.is_empty() {
        String::new()
    } else if d.len() <= 16 {
        format!("0x{}", hex(d))
    } else {
        format!("0x{}..{} ({} B)", hex(&d[..8]), hex(&d[d.len() - 4..]), d.len())
    }
}

/// A push of a number (stack depths, offsets, counts): `Op0`, `Op1Negate`, `Op1`..`Op16`, or a push of at most 8 bytes.
fn is_number(op: &Op) -> bool {
    (op.code <= 0x4e && op.data.len() <= 8) || op.code == 0x4f || (0x51..=0x60).contains(&op.code)
}

/// Pairs (i, j) of a common subsequence of `a` and `b`: equal opcodes with equal push data (weight 3), or two number
/// pushes whose value differs (weight 2: an operand the removal changed, e.g. a stack depth). Opcodes of `a` marked in
/// `gone` (compiled from the removed source text) are never paired.
fn align(a: &[Op], gone: &[bool], b: &[Op]) -> Vec<(usize, usize)> {
    let (n, m) = (a.len(), b.len());
    let w = |i: usize, j: usize| -> u32 {
        if gone[i] {
            0
        } else if a[i].code == b[j].code && a[i].data == b[j].data {
            3
        } else if is_number(&a[i]) && is_number(&b[j]) {
            2
        } else {
            0
        }
    };
    let at = |i: usize, j: usize| i * (m + 1) + j;
    let mut t = vec![0u32; (n + 1) * (m + 1)];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            let x = w(i, j);
            let diag = if x > 0 { t[at(i + 1, j + 1)] + x } else { 0 };
            t[at(i, j)] = diag.max(t[at(i + 1, j)]).max(t[at(i, j + 1)]);
        }
    }
    let (mut i, mut j, mut v) = (0, 0, vec![]);
    while i < n && j < m {
        let x = w(i, j);
        if x > 0 && t[at(i, j)] == t[at(i + 1, j + 1)] + x {
            v.push((i, j));
            i += 1;
            j += 1;
        } else if t[at(i + 1, j)] >= t[at(i, j + 1)] {
            i += 1;
        } else {
            j += 1;
        }
    }
    v
}

// ---------------------------------------------------------------- constructs

struct Construct {
    tag: &'static str,
    what: &'static str,
    edits: Vec<(String, String, usize)>,
}

struct Variant {
    file: &'static str,
    title: &'static str,
    src_path: &'static str,
    src: String,
    constructs: Vec<Construct>,
}

/// One removed construct, measured on the variant's listing.
struct Removed {
    tag: &'static str,
    what: &'static str,
    before: usize,
    after: usize,
    /// Bytes of the full script's opcodes that disappear (tagged in the listing).
    tagged: usize,
    /// Bytes that disappear but were added by an earlier removal (not in the full script).
    untracked: usize,
    /// Bytes in the result that the script before did not have.
    inserted: usize,
    /// Number pushes kept with another value (stack depths, sizes): count, and bytes before minus bytes after.
    changed: (usize, i64),
    /// Per entry (or "-" outside the entries): tagged bytes.
    per_entry: Vec<(String, usize)>,
}

struct Analysis {
    c: Compiled,
    ops: Vec<Op>,
    tags: Vec<Option<&'static str>>,
    removed: Vec<Removed>,
    rest: usize,
    /// The script with every construct removed, and the index of each of its opcodes in `ops` (`None`: not in the
    /// full script).
    rest_ops: Vec<Op>,
    rest_map: Vec<Option<usize>>,
    rest_bytecode: Vec<u8>,
}

fn entry_of(c: &Compiled, off: usize) -> String {
    c.entries.iter().find(|e| e.1 <= off && off < e.2).map(|e| e.0.clone()).unwrap_or_else(|| "-".into())
}

/// The opcodes of `src` (disassembled: `ops`) compiled from the text the `edits` remove: for each removed occurrence,
/// every opcode from the first to the last byte of the debug steps of its source lines (inlined helpers included).
fn removed_text_ops(src: &str, ops: &[Op], edits: &[(String, String, usize)], what: &str) -> Vec<bool> {
    let c = compile_debug(src, what);
    assert_eq!(disasm(&c.bytecode).len(), ops.len());
    let mut gone = vec![false; ops.len()];
    for (needle, _, _) in edits {
        for (p, _) in src.match_indices(needle.as_str()) {
            let first = src[..p].matches('\n').count() + 1;
            let last = first + needle.trim_end_matches('\n').matches('\n').count();
            let in_text: Vec<&Step> = c.steps.iter().filter(|s| (first..=last).contains(&s.line)).collect();
            assert!(!in_text.is_empty(), "{what}: no bytecode for lines {first}..={last}");
            let (b, e) = (in_text.iter().map(|s| s.start).min().unwrap(), in_text.iter().map(|s| s.end).max().unwrap());
            for (i, op) in ops.iter().enumerate() {
                if op.off >= b && op.off < e {
                    gone[i] = true;
                }
            }
        }
    }
    gone
}

fn analyse(v: &Variant) -> Analysis {
    let c = compile_debug(&v.src, v.file);
    let ops = disasm(&c.bytecode);
    let mut tags: Vec<Option<&'static str>> = vec![None; ops.len()];
    let mut cur_src = v.src.clone();
    let mut cur_ops = ops.clone();
    let mut cur_bc = c.bytecode.clone();
    let mut map: Vec<Option<usize>> = (0..ops.len()).map(Some).collect();
    let mut removed = vec![];
    for k in &v.constructs {
        let gone = removed_text_ops(&cur_src, &cur_ops, &k.edits, &format!("{} before {}", v.file, k.tag));
        let next_src = edit(&cur_src, &k.edits);
        let next_bc = bytecode(&next_src, &format!("{} - {}", v.file, k.what));
        let next_ops = disasm(&next_bc);
        let pairs = align(&cur_ops, &gone, &next_ops);
        let mut kept = vec![false; cur_ops.len()];
        let mut next_map = vec![None; next_ops.len()];
        let mut matched_next = vec![false; next_ops.len()];
        let mut changed = (0, 0i64);
        for &(i, j) in &pairs {
            kept[i] = true;
            matched_next[j] = true;
            next_map[j] = map[i];
            if cur_ops[i].code != next_ops[j].code || cur_ops[i].data != next_ops[j].data {
                changed.0 += 1;
                changed.1 += cur_ops[i].len as i64 - next_ops[j].len as i64;
            }
        }
        let (mut tagged, mut untracked) = (0, 0);
        let mut per_entry: Vec<(String, usize)> = vec![];
        for (i, op) in cur_ops.iter().enumerate().filter(|(i, _)| !kept[*i]) {
            match map[i] {
                Some(o) => {
                    assert!(tags[o].is_none(), "{}: opcode at {} removed twice", v.file, ops[o].off);
                    tags[o] = Some(k.tag);
                    tagged += op.len;
                    let e = entry_of(&c, ops[o].off);
                    match per_entry.iter_mut().find(|x| x.0 == e) {
                        Some(x) => x.1 += op.len,
                        None => per_entry.push((e, op.len)),
                    }
                }
                None => untracked += op.len,
            }
        }
        let inserted: usize = next_ops.iter().zip(&matched_next).filter(|(_, m)| !**m).map(|(o, _)| o.len).sum();
        let before: usize = cur_ops.iter().map(|o| o.len).sum();
        let after: usize = next_ops.iter().map(|o| o.len).sum();
        assert_eq!(
            (before - after) as i64,
            (tagged + untracked) as i64 - inserted as i64 + changed.1,
            "{} - {}: bytes add up",
            v.file,
            k.what
        );
        removed.push(Removed { tag: k.tag, what: k.what, before, after, tagged, untracked, inserted, changed, per_entry });
        cur_src = next_src;
        cur_ops = next_ops;
        cur_bc = next_bc;
        map = next_map;
    }
    let rest = cur_ops.iter().map(|o| o.len).sum();
    Analysis { c, ops, tags, removed, rest, rest_ops: cur_ops, rest_map: map, rest_bytecode: cur_bc }
}

/// Contiguous byte ranges of the opcodes tagged `tag`.
fn ranges(a: &Analysis, tag: &str) -> Vec<(usize, usize)> {
    let mut v: Vec<(usize, usize)> = vec![];
    for (op, t) in a.ops.iter().zip(&a.tags) {
        if *t == Some(tag) {
            match v.last_mut() {
                Some(r) if r.1 == op.off => r.1 = op.off + op.len,
                _ => v.push((op.off, op.off + op.len)),
            }
        }
    }
    v
}

fn show_ranges(r: &[(usize, usize)]) -> String {
    r.iter().map(|(s, e)| format!("[{s}, {e}) {} B", e - s)).collect::<Vec<_>>().join(", ")
}

// ---------------------------------------------------------------- the variants

const GEN_BOUNDS: &str = "        require(gen__next_output_count >= 0);\n        require(gen__next_output_count <= 1);\n";
const GEN_COUNT: &str = "        int gen__next_output_count = OpAuthOutputCount(this.activeInputIndex);\n";
const GEN_CANCEL: &str = "        require(OpAuthOutputCount(this.activeInputIndex) == 0);\n\n";
const GEN_BECOME: &str = "        require(cont.length == gen__next_output_count);\n        for (gen__next_output_position, 0, gen__next_output_count, 1) {\n            State gen__source_next_kob_ask_state = cont[gen__next_output_position];\n            // :: become KobAsk\n            validateOutputState(\n                OpAuthOutputIdx(this.activeInputIndex, gen__next_output_position),\n                gen__source_next_kob_ask_state\n            );\n        }\n";

fn state_literal(src: &str) -> String {
    let open = "                cont = cont.append(State {\n";
    let close = "                });\n";
    let at = src.find(open).expect("continuation literal");
    let end = at + src[at..].find(close).expect("literal end") + close.len();
    src[at..end].to_string()
}

fn variants() -> Vec<Variant> {
    let hand = read("contracts/v2/KobAsk.sil");
    let idio = read("contracts/argent/port-out/KobAsk.idiomatic.sil");
    let spl1 = read("contracts/argent/port/KobAsk.splice1.sil");
    let lit = state_literal(&idio);
    let d1_check = {
        let at = spl1.find("        require(gen__next_len == gen__next_output_count);\n").expect("D1 splice block");
        spl1[at..].split_inclusive('\n').take(5).collect::<String>()
    };
    vec![
        Variant {
            file: "hand",
            title: "KobAsk, hand-written SilverScript",
            src_path: "contracts/v2/KobAsk.sil",
            src: hand,
            constructs: vec![
                Construct {
                    tag: "spk",
                    what: "continuation check: scriptPubKey == contSpk(amountLeft - n) (splice of amountLeft)",
                    edits: vec![cut("                require(tx.outputs[selfOut].scriptPubKey == contSpk(amountLeft - n));\n", 1)],
                },
                Construct {
                    tag: "cnt",
                    what: "own output-count checks: OpCovOutputCount(selfId) == 1 (rest), == 0 (fill, refund)",
                    edits: vec![
                        cut("require(OpCovOutputCount(selfId) == 1);", 1),
                        cut("require(OpCovOutputCount(selfId) == 0);", 2),
                    ],
                },
            ],
        },
        Variant {
            file: "idiomatic",
            title: "KobAsk, idiomatic Argent port (argentc output)",
            src_path: "contracts/argent/port-out/KobAsk.idiomatic.sil",
            src: idio,
            constructs: vec![
                Construct {
                    tag: "bec",
                    what: "generated become: cont.length == count, validateOutputState per continuation",
                    edits: vec![cut(GEN_BECOME, 1)],
                },
                Construct {
                    tag: "lit",
                    what: "become state literal: State[] cont, cont.append(State { 20 fields })",
                    edits: vec![cut("        State[] cont;\n", 1), cut(&lit, 1)],
                },
                Construct {
                    tag: "bnd",
                    what: "generated count bounds: 0 <= OpAuthOutputCount <= 1 (emits next: KobAsk[0..=1])",
                    edits: vec![cut(GEN_BOUNDS, 1)],
                },
                Construct {
                    tag: "ocnt",
                    what: "generated local: gen__next_output_count = OpAuthOutputCount(this input)",
                    edits: vec![cut(GEN_COUNT, 1)],
                },
                Construct {
                    tag: "cxl",
                    what: "generated cancel check: OpAuthOutputCount == 0 (cancel ... emits none)",
                    edits: vec![cut(GEN_CANCEL, 1)],
                },
            ],
        },
        Variant {
            file: "splice1",
            title: "KobAsk, idiomatic port with become lowered to a splice (prototype D1)",
            src_path: "contracts/argent/port/KobAsk.splice1.sil",
            src: spl1,
            constructs: vec![
                Construct {
                    tag: "spl",
                    what: "become as a splice: len == count, scriptPubKey == this script with amountLeft replaced",
                    edits: vec![cut(&d1_check, 1)],
                },
                Construct {
                    tag: "slot",
                    what: "become slot: counter and the changed field (amountLeft - n)",
                    edits: vec![
                        cut("        int gen__next_len = 0;\n        int gen__next0_amountLeft = 0;\n", 1),
                        cut("                gen__next0_amountLeft = amountLeft - n;\n                gen__next_len = gen__next_len + 1;\n", 1),
                    ],
                },
                Construct {
                    tag: "bnd",
                    what: "generated count bounds: 0 <= OpAuthOutputCount <= 1",
                    edits: vec![cut(GEN_BOUNDS, 1)],
                },
                Construct {
                    tag: "ocnt",
                    what: "generated local: gen__next_output_count = OpAuthOutputCount(this input)",
                    edits: vec![cut(GEN_COUNT, 1)],
                },
                Construct {
                    tag: "cxl",
                    what: "generated cancel check: OpAuthOutputCount == 0",
                    edits: vec![cut(GEN_CANCEL, 1)],
                },
            ],
        },
    ]
}

// ---------------------------------------------------------------- rendering

/// What else the removal changed besides the tagged opcodes (empty: nothing).
fn side_effects(r: &Removed) -> String {
    let mut v = vec![];
    if r.changed.0 > 0 {
        v.push(format!("{} number push(es) kept with another value ({:+} B)", r.changed.0, -r.changed.1));
    }
    if r.inserted > 0 {
        v.push(format!("{} B of new opcodes", r.inserted));
    }
    if r.untracked > 0 {
        v.push(format!("{} B removed that an earlier removal had added", r.untracked));
    }
    v.join("; ")
}

fn header(v: &Variant, a: &Analysis) -> String {
    let mut s = String::new();
    s.push_str(&format!("# {}: {} ({} B redeem script)\n", v.title, v.src_path, a.c.bytecode.len()));
    s.push_str(&format!("# constructor arguments: {CTOR}; written by {THIS}\n"));
    s.push_str("# regenerate: KOB_WRITE_COMPARE=1 cargo test --release -p kob-tests --test argent_port_compare_tests\n#\n");
    s.push_str(
        "# Columns: offset in the redeem script (decimal), size, tag, opcode, push data (hex; longer than 16 B abbreviated).\n",
    );
    s.push_str("# `-- Lnnn` the source line of the statement the following opcodes were compiled from (compiler debug info);\n");
    s.push_str("# `-- (no statement)` opcodes the compiler emits outside any statement (branch ends, stack cleanup).\n");
    s.push_str("# Tags: opcodes that disappear when the construct is removed from the source and the script compiled again\n");
    s.push_str("# (removed in this order, each from the previous result). The opcodes of the removed statements always count as\n");
    s.push_str("# removed; the rest is matched against the new listing by a longest common subsequence of (opcode, push data),\n");
    s.push_str("# where a number push may match one with another value (a stack depth the removal shifted). So a tagged run can\n");
    s.push_str("# also hold stack cleanup at a branch end, or a neighbour's operand that became another opcode (`Op5 OpPick` ->\n");
    s.push_str("# `OpDup`). `>>>` lines mark each tagged run with its byte range.\n#\n");
    for r in &a.removed {
        s.push_str(&format!("#   {:<4} {}\n", r.tag, r.what));
        s.push_str(&format!(
            "#        {} -> {} B (-{} B); removed opcodes {} B at {}\n",
            r.before,
            r.after,
            r.before - r.after,
            r.tagged,
            show_ranges(&ranges(a, r.tag)),
        ));
        let x = side_effects(r);
        if !x.is_empty() {
            s.push_str(&format!("#        also: {x}\n"));
        }
    }
    s.push_str(&format!("#   rest {} B (the script with all of the above removed)\n", a.rest));
    s
}

fn listing(v: &Variant, a: &Analysis) -> String {
    let lines: Vec<&str> = v.src.lines().collect();
    let mut s = header(v, a);
    let (st, sl) = a.c.state;
    let mut region = String::new();
    let mut last_line: Option<Option<usize>> = None;
    let mut last_tag: Option<&str> = None;
    let mut field = 0;
    let region_of = |off: usize| -> String {
        if off < st {
            "prefix".into()
        } else if off < st + sl {
            "state fields".into()
        } else {
            match a.c.entries.iter().find(|e| e.1 <= off && off < e.2) {
                Some(e) => format!("entry {}", e.0),
                None if a.c.entries.first().is_some_and(|e| off < e.1) => "entry dispatch".into(),
                None => "between entries / trailer".into(),
            }
        }
    };
    for (i, op) in a.ops.iter().enumerate() {
        let r = region_of(op.off);
        if r != region {
            let last = a.ops[i..].iter().take_while(|o| region_of(o.off) == r).last().map(|o| o.off + o.len).unwrap();
            let label = &r;
            s.push_str(&format!("\n== {label} [{}, {last}) {} B\n", op.off, last - op.off));
            region = r.clone();
            last_line = None;
            last_tag = None;
        }
        if region == "state fields" {
            let name = a.c.fields.get(field).cloned().unwrap_or_else(|| "?".into());
            field += 1;
            let value = format!("{name}, value [{}, {})", op.off + op.len - op.data.len(), op.off + op.len);
            s.push_str(&format!("{:>6} {:>4}  {:<4} {:<22} {:<40} ; {value}\n", op.off, op.len, "", op.name, show_data(&op.data)));
            continue;
        }
        let step = a.c.steps.iter().filter(|x| x.start <= op.off && op.off < x.end).max_by_key(|x| x.seq);
        let line = Some(step.map(|x| x.line));
        if line != last_line {
            match step {
                Some(x) => {
                    let t = lines.get(x.line - 1).map(|l| l.trim()).unwrap_or("");
                    let t = if t.len() > 110 { format!("{}..", &t[..108]) } else { t.to_string() };
                    s.push_str(&format!("  -- L{} {t}\n", x.line));
                }
                None => s.push_str("  -- (no statement)\n"),
            }
            last_line = line;
        }
        let tag = a.tags[i];
        if let Some(t) = tag.filter(|_| tag != last_tag) {
            let run_end =
                a.ops[i..].iter().zip(&a.tags[i..]).take_while(|(_, x)| **x == tag).last().map(|(o, _)| o.off + o.len).unwrap();
            s.push_str(&format!("  >>> {t} [{}, {run_end}) {} B\n", op.off, run_end - op.off));
        }
        last_tag = tag;
        let row = format!("{:>6} {:>4}  {:<4} {:<22} {}", op.off, op.len, tag.unwrap_or(""), op.name, show_data(&op.data));
        s.push_str(row.trim_end());
        s.push('\n');
    }
    s
}

fn sections(vs: &[Variant], an: &[Analysis]) -> String {
    let mut s = String::new();
    s.push_str(&format!("# KobAsk: bytes per section (constructor arguments {CTOR}); written by {THIS}\n"));
    s.push_str("# Each construct is removed from the source after the ones above it and the script compiled again. The\n");
    s.push_str("# first number is the size difference; \"removed opcodes\" are the offsets, in the full script, of the opcodes\n");
    s.push_str("# that disappear; \"also\" are the other changes (see the header of KobAsk.<variant>.opcodes.txt).\n");
    for (v, a) in vs.iter().zip(an) {
        s.push_str(&format!("\n{} ({}): {} B\n", v.title, v.src_path, a.c.bytecode.len()));
        for (name, b, e) in &a.c.entries {
            s.push_str(&format!("  entry {name:<8} [{b}, {e}) {} B\n", e - b));
        }
        for r in &a.removed {
            let per: Vec<String> = r.per_entry.iter().map(|(e, n)| format!("{e} {n} B")).collect();
            s.push_str(&format!("  {:<4} -{:>4} B  {}\n", r.tag, r.before - r.after, r.what));
            s.push_str(&format!(
                "         removed opcodes {} B at {} ({})\n",
                r.tagged,
                show_ranges(&ranges(a, r.tag)),
                per.join(", ")
            ));
            let x = side_effects(r);
            if !x.is_empty() {
                s.push_str(&format!("         also: {x}\n"));
            }
        }
        s.push_str(&format!("  rest  {:>4} B  everything else\n", a.rest));
    }
    s
}

/// The opcodes in which the rests of `x` and `y` differ, at their offsets in the full scripts.
fn rest_diff(xn: &str, x: &Analysis, yn: &str, y: &Analysis) -> String {
    let pairs = align(&x.rest_ops, &vec![false; x.rest_ops.len()], &y.rest_ops);
    let at = |a: &Analysis, i: usize| a.rest_map[i].map(|o| a.ops[o].off.to_string()).unwrap_or_else(|| "-".into());
    let show = |a: &Analysis, i: usize| {
        let o = &a.rest_ops[i];
        format!("{} {}", o.name, show_data(&o.data)).trim_end().to_string()
    };
    let mut lines = vec![];
    let (mut i, mut j) = (0, 0);
    for &(pi, pj) in pairs.iter().chain(std::iter::once(&(x.rest_ops.len(), y.rest_ops.len()))) {
        for k in i..pi {
            lines.push(format!("    {xn} only  at {:>5}: {}", at(x, k), show(x, k)));
        }
        for k in j..pj {
            lines.push(format!("    {yn} only  at {:>5}: {}", at(y, k), show(y, k)));
        }
        i = pi + 1;
        j = pj + 1;
    }
    let changed = pairs
        .iter()
        .filter(|(i, j)| (x.rest_ops[*i].code, &x.rest_ops[*i].data) != (y.rest_ops[*j].code, &y.rest_ops[*j].data))
        .count();
    let mut s = format!(
        "  rest of {xn} ({} B) vs rest of {yn} ({} B): {} opcode(s) only in one of them, {changed} number push(es) with another value\n",
        x.rest,
        y.rest,
        lines.len()
    );
    for l in lines {
        s.push_str(&l);
        s.push('\n');
    }
    s
}

#[test]
fn kob_ask_opcode_comparison() {
    let vs = variants();
    let an: Vec<Analysis> = vs.iter().map(analyse).collect();
    let mut files: Vec<(String, String)> =
        vs.iter().zip(&an).map(|(v, a)| (format!("{OUT_DIR}/KobAsk.{}.opcodes.txt", v.file), listing(v, a))).collect();
    let mut sec = sections(&vs, &an);
    sec.push_str("\nthe rests compared (opcodes matched as above; offsets in the full scripts)\n");
    sec.push_str(&rest_diff("hand", &an[0], "idiomatic", &an[1]));
    sec.push_str(&rest_diff("idiomatic", &an[1], "splice1", &an[2]));
    files.push((format!("{OUT_DIR}/KobAsk.sections.txt"), sec));

    // the totals the documents quote
    let (h, c, d1) = (&an[0], &an[1], &an[2]);
    assert_eq!(h.c.bytecode.len(), 1684, "hand-written KobAsk size");
    assert_eq!(c.c.bytecode.len(), 2705, "idiomatic port size");
    assert_eq!(d1.c.bytecode.len(), 1713, "splice prototype size");
    for a in &an {
        let total: usize = a.removed.iter().map(|r| r.before - r.after).sum::<usize>() + a.rest;
        assert_eq!(total, a.c.bytecode.len());
    }
    // with those constructs removed, the three are the same script
    assert_eq!(h.rest_bytecode, c.rest_bytecode, "hand-written and idiomatic rests differ");
    assert_eq!(c.rest_bytecode, d1.rest_bytecode, "idiomatic and splice1 rests differ");

    let write = std::env::var_os("KOB_WRITE_COMPARE").is_some();
    if write {
        std::fs::create_dir_all(common::repo_root().join(OUT_DIR)).unwrap();
    }
    let mut stale = vec![];
    for (f, text) in &files {
        let p = common::repo_root().join(f);
        if write {
            std::fs::write(&p, text).unwrap();
        } else if std::fs::read_to_string(&p).map(|x| x.replace("\r\n", "\n")).ok().as_deref() != Some(text.as_str()) {
            stale.push(f.clone());
        }
    }
    print!("{}", files.last().unwrap().1);
    assert!(stale.is_empty(), "stale (rerun with KOB_WRITE_COMPARE=1): {stale:?}");
}
