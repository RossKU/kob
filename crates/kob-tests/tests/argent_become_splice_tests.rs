//! `become` as a splice (docs/argent-feedback.md, item 10): the hand-written order, the Argent ports and a prototype of
//! a splice lowering of `become`, run side by side in the rusty-kaspa v2.1.0 script engine.
//!
//! Variants of `KobAsk`, all compiled with the same constructor arguments (`contracts/v2/KobAsk.ctor.json`):
//!   A  hand-written SilverScript (`contracts/v2/KobAsk.sil`);
//!   B  the 1:1 Argent port (`contracts/argent/port/kob_ask_port.ag` -> argentc -> `port-out/KobAsk.port.sil`);
//!   C  the idiomatic port, B without the body checks the generated ones repeat (`kob_ask_port_idiomatic.ag` ->
//!      `port-out/KobAsk.idiomatic.sil`; it must equal B with those lines removed);
//!   D  a prototype of a splice lowering of `become`: C's generated SilverScript with the continuation state literal and
//!      the generated `validateOutputState` replaced, by a text transform ([`become_as_splice`]), with the changed state
//!      fields collected per continuation and their bytes spliced into this script (the way `KobAsk.sil`'s `contSpk`
//!      does). Everything else argentc generates (count bounds, `cont.length == count`, cancel's `OpAuthOutputCount ==
//!      0`) is kept. D1 is the same with the continuation count bounded to one slot (`emits ...[0..=1]`).
//!
//! Measured: redeem script bytes; script units per order input, its signature script, transaction size, compute mass
//! and relay floor (100 sompi/gram) of a partial fill that rests, a full fill, a refund and a cancel (the kob-protocol
//! fixtures, compute budgets tightened per variant). Behaviour: every KCC-20 fixture and branch shape that spends a
//! `KobAsk`, as built (positives), and edits of them (negatives: forged continuation states, a dropped, duplicated or
//! foreign continuation, a continuation where none may be, a cancel that continues the covenant), must get the same
//! verdict at every input under B, C, D and D1 as under A. Ablations (one check removed) show which check carries which
//! verdict. Run:
//!
//! ```text
//! cargo test --release -p kob-tests --test argent_become_splice_tests -- --nocapture
//! ```

mod common;
#[path = "../../kob-protocol/tests/common/mod.rs"]
mod fx;
#[path = "common/pair_harness.rs"]
mod ph;

use std::collections::BTreeMap;

use fx::*;
use kaspa_consensus_core::mass::ComputeBudget;
use kaspa_consensus_core::tx::{CovenantBinding, TransactionOutput};
use kaspa_consensus_core::Hash;
use kob_protocol::artifacts::{template, TemplateId};
use kob_protocol::budget::budget_for_units;
use kob_protocol::build::{Action, Leg};
use kob_protocol::script::p2sh_spk;
use kob_protocol::state::{AnyState, AskState};
use kob_protocol::tx::{masses, min_fee, SigPlan, MIN_FEE_RATE};
use ph::*;
use silverscript_abi::{ArtifactValue, SilAbiArtifact};
use silverscript_lang::compiler::CompileOptions;

fn read(rel: &str) -> String {
    std::fs::read_to_string(common::repo_root().join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}")).replace("\r\n", "\n")
}

fn compile(src: &str, what: &str) -> SilAbiArtifact {
    let ctor: Vec<ArtifactValue> = serde_json::from_str(&read("contracts/v2/KobAsk.ctor.json")).expect("ctor json");
    common::compile_contract(src, &ctor, CompileOptions::default()).unwrap_or_else(|e| panic!("compile {what}: {e:?}"))
}

/// `src` with every `(needle, replacement)` applied; each needle must occur exactly `n` times.
fn edit(src: &str, edits: &[(&str, &str, usize)]) -> String {
    let mut s = src.to_string();
    for (needle, with, n) in edits {
        assert_eq!(s.matches(needle).count(), *n, "the source changed: `{needle}` must occur {n} time(s)");
        s = s.replace(needle, with);
    }
    s
}

// ---------------------------------------------------------------- the checks (as in argent_port_tests.rs)

const GEN_BOUNDS: (&str, &str, usize) =
    ("        require(gen__next_output_count >= 0);\n        require(gen__next_output_count <= 1);\n", "", 1);
const GEN_BECOME: &str = "        require(cont.length == gen__next_output_count);\n        for (gen__next_output_position, 0, gen__next_output_count, 1) {\n            State gen__source_next_kob_ask_state = cont[gen__next_output_position];\n            // :: become KobAsk\n            validateOutputState(\n                OpAuthOutputIdx(this.activeInputIndex, gen__next_output_position),\n                gen__source_next_kob_ask_state\n            );\n        }\n";
const GEN_CANCEL: (&str, &str, usize) = ("        require(OpAuthOutputCount(this.activeInputIndex) == 0);\n\n", "", 1);
/// The body checks of the hand-written order that the generated ones repeat (removed from B: C).
const BODY_DUPS: [(&str, &str, usize); 4] = [
    ("                require(OpCovOutputCount(selfId) == 1);\n", "", 1),
    ("                require(OpCovOutputCount(selfId) == 0);\n", "", 1),
    ("            require(OpCovOutputCount(selfId) == 0);\n", "", 1),
    ("                require(tx.outputs[selfOut].scriptPubKey == contSpk(amountLeft - n));\n", "", 1),
];

// ---------------------------------------------------------------- the splice lowering (prototype)

/// Bytes of a fixed-width state field's value (`None`: variable length, no compile-time offset).
fn field_width(ty: &str) -> Option<usize> {
    match ty {
        "int" => Some(8),
        "pubkey" => Some(32),
        "byte" => Some(1),
        t if t.starts_with("byte[") && t.ends_with(']') => t[5..t.len() - 1].parse().ok(),
        _ => None,
    }
}

/// The state fields of the generated actor (type, name), in layout order.
fn state_fields(src: &str, actor: &str) -> Vec<(String, String)> {
    let head = format!("    // :: state fields: {actor}\n");
    let at = src.find(&head).unwrap_or_else(|| panic!("no `{}` block", head.trim()));
    src[at + head.len()..]
        .lines()
        .take_while(|l| !l.trim().is_empty())
        .map(|l| {
            let mut w = l.split_whitespace();
            let (ty, name) = (w.next().unwrap(), w.next().unwrap());
            assert_eq!(w.next(), Some("="), "state field line `{l}`");
            (ty.to_string(), name.to_string())
        })
        .collect()
}

/// (offset of the value bytes in the redeem script, width) of every state field: each field is one data push
/// (opcode + value) and the state span starts at `state_start`.
fn field_offsets(fields: &[(String, String)], state_start: usize, state_len: usize) -> BTreeMap<String, (usize, usize)> {
    let mut at = state_start;
    let mut m = BTreeMap::new();
    for (ty, name) in fields {
        let w = field_width(ty).unwrap_or_else(|| panic!("state field `{name}: {ty}` has no fixed width: no splice offset"));
        assert!(w <= 75, "single-byte push opcode");
        m.insert(name.clone(), (at + 1, w));
        at += 1 + w;
    }
    assert_eq!(at, state_start + state_len, "the computed state layout must match the compiled state span");
    m
}

fn encode_expr(ty: &str, v: &str) -> String {
    match ty {
        "int" => format!("byte[]({v} as byte[8])"),
        _ => format!("byte[]({v})"),
    }
}

/// How the continuation count is carried.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Slots {
    /// One array per changed field (any count bound).
    Arrays,
    /// One scalar per changed field and a counter (count bound 1).
    One,
}

/// The splice lowering of `become next <- KobAsk[](cont)` on argentc's generated SilverScript `src` (state span
/// `[state_start, state_start + state_len)` of its compiled script): the `State` literal appended to `cont` is reduced to
/// the fields whose value is not the field itself (compile-time: `field: field` is "unchanged"), and the generated
/// `validateOutputState` of each continuation becomes an SPK equality with this script's redeem script with those fields'
/// bytes replaced. The count checks stay.
fn become_as_splice(src: &str, state_start: usize, state_len: usize, slots: Slots) -> String {
    let fields = state_fields(src, "KobAsk");
    let offs = field_offsets(&fields, state_start, state_len);
    let ty_of: BTreeMap<&str, &str> = fields.iter().map(|(t, n)| (n.as_str(), t.as_str())).collect();

    // the one State literal appended to `cont`
    let open = "cont = cont.append(State {\n";
    assert_eq!(src.matches(open).count(), 1, "one continuation literal");
    let at = src.find(open).unwrap();
    let line_start = src[..at].rfind('\n').unwrap() + 1;
    let indent = &src[line_start..at];
    let close = format!("{indent}}});\n");
    let end = at + src[at..].find(&close).expect("literal end") + close.len();
    let body = &src[at + open.len()..end - close.len()];
    let mut seen = vec![];
    let mut changed: Vec<(String, String)> = vec![];
    for l in body.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with("//")) {
        let (name, expr) = l.trim_end_matches(',').split_once(": ").unwrap_or_else(|| panic!("literal line `{l}`"));
        seen.push(name.to_string());
        if expr.trim() != name {
            changed.push((name.to_string(), expr.trim().to_string()));
        }
    }
    assert_eq!(seen, fields.iter().map(|(_, n)| n.clone()).collect::<Vec<_>>(), "the literal names every field in order");
    assert!(!changed.is_empty(), "a continuation equal to self");
    let mut changed_sorted = changed.clone();
    changed_sorted.sort_by_key(|(n, _)| offs[n].0);

    // the splice helper: this script's redeem script with the changed fields' bytes replaced
    let params: Vec<String> = changed_sorted.iter().map(|(n, _)| format!("{} gen__v_{n}", ty_of[n.as_str()])).collect();
    let mut pieces = vec![];
    let mut from = "0".to_string();
    for (n, _) in &changed_sorted {
        let (o, w) = offs[n];
        pieces.push(format!("gen__rs.slice({from}, {o})"));
        pieces.push(encode_expr(ty_of[n.as_str()], &format!("gen__v_{n}")));
        from = (o + w).to_string();
    }
    pieces.push(format!("gen__rs.slice({from}, this.bytecodeSize)"));
    let what: Vec<String> = changed_sorted.iter().map(|(n, _)| format!("{n} [{}..{})", offs[n].0, offs[n].0 + offs[n].1)).collect();
    let helper = format!(
        "    // :: become splice: KobAsk = this script with {}\n    function gen__become_splice_KobAsk({}) : byte[] {{\n        int gen__me = this.activeInputIndex;\n        int gen__len = OpTxInputScriptSigLen(gen__me);\n        byte[] gen__rs = OpTxInputScriptSigSubstr(gen__me, gen__len - this.bytecodeSize, gen__len);\n        return byte[](new ScriptPubKeyP2SHFromRedeemScript({}));\n    }}\n\n",
        what.join(", "),
        params.join(", "),
        pieces.join(" + ")
    );

    let (decl, append, check) = match slots {
        Slots::Arrays => {
            let decl: String = changed.iter().map(|(n, _)| format!("        {}[] gen__next_{n};\n", ty_of[n.as_str()])).collect();
            let append: String =
                changed.iter().map(|(n, e)| format!("{indent}gen__next_{n} = gen__next_{n}.append({e});\n")).collect();
            let args: Vec<String> = changed_sorted.iter().map(|(n, _)| format!("gen__next_{n}[gen__next_output_position]")).collect();
            let check = format!(
                "        require(gen__next_{}.length == gen__next_output_count);\n        for (gen__next_output_position, 0, gen__next_output_count, 1) {{\n            // :: become KobAsk (splice)\n            require(tx.outputs[OpAuthOutputIdx(this.activeInputIndex, gen__next_output_position)].scriptPubKey == gen__become_splice_KobAsk({}));\n        }}\n",
                changed[0].0,
                args.join(", ")
            );
            (decl, append, check)
        }
        Slots::One => {
            assert_eq!(
                src.matches("        require(gen__next_output_count <= 1);\n").count(),
                1,
                "one-slot lowering needs a count bound of 1"
            );
            let mut decl = "        int gen__next_len = 0;\n".to_string();
            for (n, _) in &changed {
                let ty = ty_of[n.as_str()];
                assert_eq!(ty, "int", "prototype: int slots only");
                decl.push_str(&format!("        {ty} gen__next0_{n} = 0;\n"));
            }
            let mut append: String = changed.iter().map(|(n, e)| format!("{indent}gen__next0_{n} = {e};\n")).collect();
            append.push_str(&format!("{indent}gen__next_len = gen__next_len + 1;\n"));
            let args: Vec<String> = changed_sorted.iter().map(|(n, _)| format!("gen__next0_{n}")).collect();
            let check = format!(
                "        require(gen__next_len == gen__next_output_count);\n        if (gen__next_output_count == 1) {{\n            // :: become KobAsk (splice)\n            require(tx.outputs[OpAuthOutputIdx(this.activeInputIndex, 0)].scriptPubKey == gen__become_splice_KobAsk({}));\n        }}\n",
                args.join(", ")
            );
            (decl, append, check)
        }
    };
    let mut out = String::with_capacity(src.len());
    out.push_str(&src[..line_start]);
    out.push_str(&append);
    out.push_str(&src[end..]);
    let out = edit(&out, &[("        State[] cont;\n", &decl, 1), (GEN_BECOME, &check, 1)]);
    edit(&out, &[("    // :: range helpers\n", &format!("{helper}    // :: range helpers\n"), 1)])
}

// ---------------------------------------------------------------- variants under test

struct Variant {
    name: String,
    bytes: usize,
    subs: Subs,
}

fn variant(name: &str, src: &str) -> Variant {
    let art = compile(src, name);
    let bytes = common::bytecode(&art).len();
    let (prefix, suffix, hash) = common::compiled_template_parts_and_hash(&art);
    let pinned = template(TemplateId::KobAsk);
    assert_eq!(prefix, pinned.prefix, "{name}: the state span must not move");
    let sub = Sub { id: TemplateId::KobAsk, prefix, suffix, hash: hash.try_into().expect("32-byte hash") };
    Variant { name: name.into(), bytes, subs: Subs { subs: BTreeMap::from([(TemplateId::KobAsk, sub)]) } }
}

// ---------------------------------------------------------------- transactions

struct Case {
    name: String,
    ed: Ed,
    /// Inputs whose verdict is compared: every input except entries of other templates (readers that pin the pinned
    /// KobAsk template hash, not the variant's).
    inputs: Vec<usize>,
    positive: bool,
}

fn ask_state(ed: &Ed, i: usize) -> AskState {
    match AnyState::decode(TemplateId::KobAsk, &ed.entry_state(i).1).expect("KobAsk state") {
        AnyState::KobAsk(s) => s,
        _ => unreachable!(),
    }
}

fn enc(s: &AskState) -> Vec<u8> {
    AnyState::KobAsk(s.clone()).encode()
}

fn entry_name(p: &SigPlan) -> Option<&str> {
    match p {
        SigPlan::Entry { entry, .. } => Some(entry.as_str()),
        _ => None,
    }
}

fn compared_inputs(ed: &Ed) -> Vec<usize> {
    (0..ed.plans.len())
        .filter(|&i| !matches!(&ed.plans[i], SigPlan::Entry { template, .. } if *template != TemplateId::KobAsk))
        .collect()
}

/// A GTC ask filled to the last base unit (the FOK fixture with tif GTC).
fn take_full(p: TemplateId) -> Action {
    let (_, a) = scenarios_on(p).into_iter().find(|(n, _)| n == "take.ask.fok").expect("take.ask.fok");
    let Action::Batch(mut b) = a else { panic!("take.ask.fok is a batch") };
    let Leg::Ask { order, amount, .. } = &mut b.legs[0] else { panic!("take.ask.fok leg 0 is an ask") };
    order.state.tif = 0;
    assert_eq!(*amount, order.state.amount_left, "a full fill");
    Action::Batch(b)
}

/// Every KCC-20 fixture and branch shape that spends a KobAsk (on the 3/3 and the 8/8 reference programs).
fn positives() -> Vec<(String, Ed)> {
    let mut v = vec![];
    for p in [TemplateId::Kcc20Ref, TemplateId::Kcc20Ref8x8] {
        let tag = p.name();
        let mut all = scenarios_on(p);
        all.extend(branches::branch_shapes(p));
        all.push(("take.ask.full".into(), take_full(p)));
        for (n, a) in all {
            let ed = Ed::new(&format!("{tag}.{n}"), &built(a));
            if !ed.inputs_of(TemplateId::KobAsk).is_empty() {
                v.push((ed.name.clone(), ed));
            }
        }
    }
    v
}

fn bind(ed: &mut Ed, auth: usize, c: [u8; 32], spk: kaspa_consensus_core::tx::ScriptPublicKey, value: u64) -> usize {
    ed.tx.outputs.push(TransactionOutput {
        value,
        script_public_key: spk,
        covenant: Some(CovenantBinding { authorizing_input: auth as u16, covenant_id: Hash::from_bytes(c) }),
    });
    ed.tx.outputs.len() - 1
}

/// The edits of one KobAsk input `j` of a positive.
fn negatives_of(ed: &Ed, j: usize) -> Vec<(String, Ed)> {
    let mut v = vec![];
    let c = cov_of(&ed.entries[j]).expect("order covenant");
    let st = ask_state(ed, j);
    let carrier = ed.entries[j].amount;
    let mut add = |what: &str, f: &dyn Fn(&mut Ed)| {
        let mut e = ed.clone().named(&format!("{} / in[{j}] {what}", ed.name));
        f(&mut e);
        v.push((e.name.clone(), e));
    };
    match entry_name(&ed.plans[j]) {
        Some("cancel") => {
            add("cancel + a covenant output (KobAsk, same state)", &|e| {
                bind(e, j, c, template(TemplateId::KobAsk).spk(&enc(&st)), carrier);
            });
        }
        Some("settle") => {
            let n = fill_n(&ed.plans[j]).expect("settle n");
            match ed.cont_out(c) {
                Some(k) => {
                    let left = st.amount_left - n;
                    let forged: Vec<(&str, AskState)> = vec![
                        ("cont amountLeft unchanged", AskState { amount_left: st.amount_left, ..st.clone() }),
                        ("cont amountLeft + 1", AskState { amount_left: left + 1, ..st.clone() }),
                        ("cont amountLeft - 1", AskState { amount_left: left - 1, ..st.clone() }),
                        ("cont maker (first field)", AskState { maker: pk(TAKER), amount_left: left, ..st.clone() }),
                        (
                            "cont decayStep (field before amountLeft)",
                            AskState { decay_step: st.decay_step + 1, amount_left: left, ..st.clone() },
                        ),
                        (
                            "cont extensionCommitment (last field)",
                            AskState { extension_commitment: [0xdd; 32], amount_left: left, ..st.clone() },
                        ),
                        ("cont price - 1", AskState { price: st.price - 1, amount_left: left, ..st.clone() }),
                        ("cont tif IOC", AskState { tif: 1, amount_left: left, ..st.clone() }),
                    ];
                    for (what, s) in forged {
                        add(what, &|e| e.set_out_spk_state(k, TemplateId::KobAsk, &enc(&s)));
                    }
                    let honest = AskState { amount_left: left, ..st.clone() };
                    add("cont foreign code (honest state, one byte appended)", &|e| {
                        let t = template(TemplateId::KobAsk);
                        e.tx.outputs[k].script_public_key =
                            p2sh_spk(&[t.prefix.as_slice(), &enc(&honest), &t.suffix, &[0x61]].concat());
                    });
                    add("cont dropped (plain output)", &|e| e.unbind(k, MAKER_A));
                    add("cont duplicated", &|e| {
                        let o = e.tx.outputs[k].clone();
                        e.tx.outputs.push(o);
                    });
                    add("cont value - 1", &|e| {
                        let x = e.value(k);
                        e.set_value(k, x - 1);
                    });
                }
                None => {
                    let next = AskState { amount_left: st.amount_left - n, ..st.clone() };
                    add("a continuation where none may be", &|e| {
                        bind(e, j, c, template(TemplateId::KobAsk).spk(&enc(&next)), carrier);
                    });
                    add("a continuation (same state) where none may be", &|e| {
                        bind(e, j, c, template(TemplateId::KobAsk).spk(&enc(&st)), carrier);
                    });
                }
            }
        }
        other => panic!("{}: KobAsk entry {other:?}", ed.name),
    }
    v
}

fn cases() -> Vec<Case> {
    let mut v = vec![];
    for (name, ed) in positives() {
        let inputs = compared_inputs(&ed);
        let asks = ed.inputs_of(TemplateId::KobAsk);
        v.push(Case { name, ed: ed.clone(), inputs: inputs.clone(), positive: true });
        for j in asks {
            for (n, e) in negatives_of(&ed, j) {
                v.push(Case { name: n, ed: e, inputs: inputs.clone(), positive: false });
            }
        }
    }
    v
}

/// Verdict (accepted) of every compared input.
fn verdicts(var: &Variant, case: &Case) -> Vec<(usize, bool)> {
    let (tx, entries) = case.ed.finish(&var.subs);
    let res = kob_protocol::verify::execute(&tx, &entries, false).unwrap_or_else(|e| panic!("{} {}: {e}", var.name, case.name));
    case.inputs.iter().map(|&i| (i, res[i].is_ok())).collect()
}

// ---------------------------------------------------------------- costs

#[derive(Debug, Clone, Copy)]
struct Cost {
    units: u64,
    sigscript: usize,
    size: u64,
    compute: u64,
    fee: u64,
}

fn cost(var: &Variant, ed: &Ed) -> Cost {
    let j = ed.inputs_of(TemplateId::KobAsk)[0];
    let (tx, entries) = ed.finish(&var.subs);
    let units = kob_protocol::verify::measure_units(&tx, &entries).unwrap_or_else(|e| panic!("{} {}: {e}", var.name, ed.name));
    let mut e2 = ed.clone();
    for (i, u) in units.iter().enumerate() {
        e2.tx.inputs[i].compute_commit = ComputeBudget::from(budget_for_units(*u)).into();
    }
    let (tx, entries) = e2.finish(&var.subs);
    let res = kob_protocol::verify::execute(&tx, &entries, true).unwrap();
    assert!(res.iter().all(|r| r.is_ok()), "{} {}: tightened budgets: {res:?}", var.name, ed.name);
    let m = masses(&tx, &entries);
    Cost {
        units: units[j],
        sigscript: tx.inputs[j].signature_script.len(),
        size: m.size,
        compute: m.compute,
        fee: min_fee(&m, MIN_FEE_RATE),
    }
}

// ---------------------------------------------------------------- the test

#[test]
fn become_as_splice_vs_hand_written() {
    let a_src = read("contracts/v2/KobAsk.sil");
    let b_src = read("contracts/argent/port-out/KobAsk.port.sil");
    let c_src = read("contracts/argent/port-out/KobAsk.idiomatic.sil");
    assert_eq!(c_src, edit(&b_src, &BODY_DUPS), "KobAsk.idiomatic.sil is KobAsk.port.sil without the repeated body checks");
    let c_art = compile(&c_src, "C");
    let layout = common::state_layout(&c_art);
    let la = common::state_layout(&compile(&a_src, "A"));
    assert_eq!((layout.start, layout.len), (la.start, la.len), "A and C share the state layout");
    let d_src = become_as_splice(&c_src, layout.start, layout.len, Slots::Arrays);
    let d1_src = become_as_splice(&c_src, layout.start, layout.len, Slots::One);
    // the prototype as committed (for reading; the test always transforms C itself)
    for (f, s) in [("contracts/argent/port/KobAsk.splice.sil", &d_src), ("contracts/argent/port/KobAsk.splice1.sil", &d1_src)] {
        if std::env::var_os("KOB_WRITE_SPLICE").is_some() {
            std::fs::write(common::repo_root().join(f), s).unwrap();
        }
        assert_eq!(&read(f), s, "{f} is stale: rerun with KOB_WRITE_SPLICE=1");
    }
    // the hand-written splice offset is the one the layout gives
    assert!(a_src.contains("rs.slice(0, 236) + byte[](newLeft as byte[8]) + rs.slice(244, this.bytecodeSize)"));
    assert!(d_src.contains("gen__rs.slice(0, 236) + byte[](gen__v_amountLeft as byte[8]) + gen__rs.slice(244, this.bytecodeSize)"));

    let main = [variant("A", &a_src), variant("B", &b_src), variant("C", &c_src), variant("D", &d_src), variant("D1", &d1_src)];
    let a = &main[0];
    assert_eq!(a.subs.get(TemplateId::KobAsk).suffix, template(TemplateId::KobAsk).suffix, "A is the pinned KobAsk");

    // ---- sizes
    let size = |s: &str, what: &str| common::bytecode(&compile(s, what)).len();
    println!("\nredeem script (same constructor arguments)");
    for v in &main {
        println!("  {:<3} {:>5} B  ({:+} vs A)", v.name, v.bytes, v.bytes as i64 - a.bytes as i64);
    }
    let d = &main[3];
    let a_dups: Vec<(&str, &str, usize)> = vec![
        ("require(OpCovOutputCount(selfId) == 1);", "", 1),
        ("require(OpCovOutputCount(selfId) == 0);", "", 2),
        ("                require(tx.outputs[selfOut].scriptPubKey == contSpk(amountLeft - n));\n", "", 1),
    ];
    let a_count = a.bytes - size(&edit(&a_src, &a_dups[..2]), "A - counts");
    let a_spk = a.bytes - size(&edit(&a_src, &a_dups[2..]), "A - contSpk check");
    let d_bounds = d.bytes - size(&edit(&d_src, &[GEN_BOUNDS]), "D - bounds");
    let d_cancel = d.bytes - size(&edit(&d_src, &[GEN_CANCEL]), "D - cancel");
    let d_check_txt =
        d_src[d_src.find("        require(gen__next_amountLeft.length").unwrap()..].split_inclusive('\n').take(5).collect::<String>();
    let d_splice = d.bytes - size(&edit(&d_src, &[(&d_check_txt, "", 1)]), "D - splice check");
    let d_len_eq = d.bytes
        - size(
            &edit(&d_src, &[("        require(gen__next_amountLeft.length == gen__next_output_count);\n", "", 1)]),
            "D - len == count",
        );
    let d_slots = size(&edit(&d_src, &[(&d_check_txt, "", 1)]), "D - check")
        - size(
            &edit(
                &d_src,
                &[
                    (&d_check_txt, "", 1),
                    ("        int[] gen__next_amountLeft;\n", "", 1),
                    ("                gen__next_amountLeft = gen__next_amountLeft.append(amountLeft - n);\n", "", 1),
                ],
            ),
            "D - check - slots",
        );
    println!("  A: own output-count checks {a_count} B, continuation SPK check (contSpk) {a_spk} B");
    println!(
        "  D: count bounds {d_bounds} B, cancel OpAuthOutputCount == 0 {d_cancel} B, splice block {d_splice} B (of it `len == count` {d_len_eq} B), changed-field array {d_slots} B"
    );
    let d1 = &main[4];
    let d1_check_txt = d1_src[d1_src.find("        require(gen__next_len == gen__next_output_count);").unwrap()..]
        .split_inclusive('\n')
        .take(5)
        .collect::<String>();
    let d1_no_check = edit(&d1_src, &[(&d1_check_txt, "", 1)]);
    let d1_splice = d1.bytes - size(&d1_no_check, "D1 - splice check");
    let d1_len_eq = d1.bytes
        - size(
            &edit(
                &d1_src,
                &[(
                    "        require(gen__next_len == gen__next_output_count);
",
                    "",
                    1,
                )],
            ),
            "D1 - len == count",
        );
    let d1_slots = size(&d1_no_check, "D1 - check")
        - size(
            &edit(
                &d1_no_check,
                &[
                    (
                        "        int gen__next_len = 0;
        int gen__next0_amountLeft = 0;
",
                        "",
                        1,
                    ),
                    (
                        "                gen__next0_amountLeft = amountLeft - n;
                gen__next_len = gen__next_len + 1;
",
                        "",
                        1,
                    ),
                ],
            ),
            "D1 - check - slots",
        );
    let d1_min = size(&edit(&d1_src, &[GEN_BOUNDS, GEN_CANCEL]), "D1 - bounds - cancel");
    println!(
        "  D1: splice block {d1_splice} B (of it `len == count` {d1_len_eq} B), counter and slot {d1_slots} B; D1 without the count bounds and cancel's check {d1_min} B ({:+} vs A)",
        d1_min as i64 - a.bytes as i64
    );

    // ---- costs
    let tag = TemplateId::Kcc20Ref.name();
    let pos: BTreeMap<String, Ed> = positives().into_iter().collect();
    let shapes = [
        ("settle, partial fill (rests)", format!("{tag}.take.ask.partial")),
        ("settle, full fill (GTC)", format!("{tag}.take.ask.full")),
        ("settle, refund (n = 0)", format!("{tag}.refund.ask.expiry")),
        ("cancel", format!("{tag}.cancel.ask")),
    ];
    println!("\ncosts (order input: script units, sigscript; tx: size, compute mass, relay floor at 100 sompi/gram)");
    for (what, name) in &shapes {
        let ed = &pos[name];
        println!("  {what} ({name})");
        let ca = cost(a, ed);
        for v in &main {
            let c = cost(v, ed);
            println!(
                "    {:<3} units {:>8} ({:+7})  sigscript {:>5} B  tx {:>5} B  compute {:>7}  min fee {:>7} sompi ({:+})",
                v.name,
                c.units,
                c.units as i64 - ca.units as i64,
                c.sigscript,
                c.size,
                c.compute,
                c.fee,
                c.fee as i64 - ca.fee as i64
            );
        }
    }

    // ---- behaviour
    let cases = cases();
    let npos = cases.iter().filter(|c| c.positive).count();
    println!("\nbehaviour: {} cases ({npos} positives, {} negatives)", cases.len(), cases.len() - npos);
    let base: Vec<Vec<(usize, bool)>> = cases.iter().map(|c| verdicts(a, c)).collect();
    for (c, r) in cases.iter().zip(&base) {
        if c.positive {
            assert!(r.iter().all(|(_, ok)| *ok), "A rejects the positive {}: {r:?}", c.name);
        }
    }
    let a_refuses = |k: usize| base[k].iter().any(|(i, ok)| !ok && cases[k].ed.inputs_of(TemplateId::KobAsk).contains(i));
    let refused: BTreeMap<String, usize> = {
        let mut m = BTreeMap::new();
        for (k, c) in cases.iter().enumerate().filter(|(_, c)| !c.positive) {
            let what = c.name.split("] ").nth(1).unwrap_or(&c.name).to_string();
            *m.entry(format!("{what}: {}", if a_refuses(k) { "refused by A" } else { "ACCEPTED by A" })).or_insert(0) += 1;
        }
        m
    };
    for (k, n) in &refused {
        println!("  {k}  x{n}");
    }
    // a maker's cancel that continues the covenant (an output bound to the order's id: an amend keeps the order's id
    // and its custody, `build_amend_order`)
    let cancel_continues = |c: &Case| {
        c.ed.inputs_of(TemplateId::KobAsk)
            .into_iter()
            .any(|j| entry_name(&c.ed.plans[j]) == Some("cancel") && !c.ed.bound_to(cov_of(&c.ed.entries[j]).unwrap()).is_empty())
    };
    let kind = |c: &Case| -> String {
        if c.positive {
            format!("positive {}", c.name.split_once('.').map(|x| x.1).unwrap_or(&c.name))
        } else {
            c.name.split("] ").nth(1).unwrap_or(&c.name).to_string()
        }
    };
    // (case, flips) of every case whose verdicts differ from A's
    let compare = |v: &Variant| -> Vec<(usize, String)> {
        let mut diffs = vec![];
        for (k, c) in cases.iter().enumerate() {
            let r = verdicts(v, c);
            if r != base[k] {
                let flips: Vec<String> = r
                    .iter()
                    .zip(&base[k])
                    .filter(|(x, y)| x.1 != y.1)
                    .map(|(x, _)| format!("in[{}] {}", x.0, if x.1 { "accepts" } else { "refuses" }))
                    .collect();
                diffs.push((k, flips.join(", ")));
            }
        }
        diffs
    };
    let summary = |diffs: &[(usize, String)]| {
        let mut m: BTreeMap<String, usize> = BTreeMap::new();
        for (k, f) in diffs {
            let tag = if cancel_continues(&cases[*k]) { "  [cancel continuing the covenant]" } else { "" };
            *m.entry(format!("{} -> {f}{tag}", kind(&cases[*k]))).or_insert(0) += 1;
        }
        for (k, n) in m {
            println!("      {k}  x{n}");
        }
    };
    let mut differ = BTreeMap::new();
    for v in &main[1..] {
        let diffs = compare(v);
        let other = diffs.iter().filter(|(k, _)| !cancel_continues(&cases[*k])).count();
        println!("  {} vs A: {} differing case(s), {other} of them not a cancel continuing the covenant", v.name, diffs.len());
        summary(&diffs);
        differ.insert(v.name.clone(), diffs);
    }

    // ---- ablations: which check carries which verdict
    let d_once = d_check_txt.clone();
    let abl: Vec<(String, String)> = vec![
        ("A - contSpk check".into(), edit(&a_src, &a_dups[2..])),
        ("A - own count checks".into(), edit(&a_src, &a_dups[..2])),
        ("B - generated become".into(), edit(&b_src, &[(GEN_BECOME, "", 1)])),
        ("B - generated count bounds".into(), edit(&b_src, &[GEN_BOUNDS])),
        ("C - generated become".into(), edit(&c_src, &[(GEN_BECOME, "", 1)])),
        ("C - generated count bounds".into(), edit(&c_src, &[GEN_BOUNDS])),
        ("D - splice block".into(), edit(&d_src, &[(&d_once, "", 1)])),
        (
            "D - len == count".into(),
            edit(&d_src, &[("        require(gen__next_amountLeft.length == gen__next_output_count);\n", "", 1)]),
        ),
        ("D - count bounds".into(), edit(&d_src, &[GEN_BOUNDS])),
        ("D1 - splice block".into(), edit(&d1_src, &[(&d1_check_txt, "", 1)])),
    ];
    println!("\nablations (cases whose verdict differs from A)");
    for (name, src) in &abl {
        let v = variant(name, src);
        let diffs = compare(&v);
        let other = diffs.iter().filter(|(k, _)| !cancel_continues(&cases[*k])).count();
        println!("  {name} ({} B): {} case(s), {other} of them not a cancel continuing the covenant", v.bytes, diffs.len());
        let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
        for (k, _) in diffs.iter().filter(|(k, _)| !cancel_continues(&cases[*k])) {
            *kinds.entry(kind(&cases[*k])).or_insert(0) += 1;
        }
        for (k, n) in kinds {
            println!("      {k}  x{n}");
        }
    }

    // every variant decides every case as A does, except the cancel that continues the covenant (the generated
    // `emits none` check refuses it; the hand-written cancel leaves the outputs to the maker's signature)
    for (name, diffs) in &differ {
        for (k, f) in diffs {
            assert!(cancel_continues(&cases[*k]), "{name} differs from A: {} ({f})", cases[*k].name);
        }
    }
}
