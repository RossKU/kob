//! The kaspa-redteam library attacks (OUT-01..04, FEE-01, IN-01..02, SIG-01..02, FAM-10, BOR-01) and the token-amount
//! attack TOK-01 on every honest shape of `shapes.rs`, from the taker, matcher and keeper seats (see `rt.rs`).
//!
//! FAM-10 and BOR-01 run on every token build here (kaspa-redteam reads only the KCC-20 reference program): a stray of
//! every token of the transaction owned by each covenant input, and every custody with borrowing on.
//!
//! Each test prints its coverage matrix (`RT|attack|template|program|judged|refused|notes|findings|dropped|by_target`) and fails
//! on any finding, after writing its replay file (`KOB_RT_FINDINGS`, default `<tmp>/kob-rt-findings`).
//!
//! The existing tests per attack id (hand-built scenarios, ablation, pre-sign) are mapped in docs/security/redteam-coverage.md.
//!
//! Run: cargo test -p kob-tests --test redteam_library_tests -- --nocapture --test-threads=2 (the pair tests: add --ignored)

#[path = "../../../kob-protocol/tests/common/mod.rs"]
mod fx;
mod rt;
mod shapes;

use kob_protocol::artifacts::TemplateId;
use rt::{honest, library_attempts, Run};

fn run(title: &str, shapes: Vec<shapes::Shape>) -> Run {
    let mut r = Run::default();
    for (name, b) in shapes {
        let h = honest(&name, &b);
        let a = library_attempts(&h);
        r.attack(&name, &h, a);
    }
    r.report(title);
    r
}

macro_rules! program_test {
    ($name:ident, $p:expr) => {
        #[test]
        fn $name() {
            let p: TemplateId = $p;
            let r = run(&format!("library/{}", p.name()), shapes::program_shapes(p));
            r.assert_clean();
        }
    };
}

program_test!(library_kcc20ref, TemplateId::Kcc20Ref);
program_test!(library_kcc20ref_4x5, TemplateId::Kcc20Ref4x5);
program_test!(library_kcc20ref_8x8, TemplateId::Kcc20Ref8x8);
program_test!(library_kcc20ref_16x16, TemplateId::Kcc20Ref16x16);
program_test!(library_kcc20p2, TemplateId::Kcc20P2);
program_test!(library_kcc20kaspacom, TemplateId::Kcc20KaspaCom025);
program_test!(library_kcc20publicmint, TemplateId::Kcc20PublicMint);
program_test!(library_kforge7214, TemplateId::KForgeToken7214);
program_test!(library_kaspa_rocket, TemplateId::KaspaRocketToken11458);
program_test!(library_kron2433, TemplateId::KronToken2433);
program_test!(library_kron2732, TemplateId::KronToken2732);
program_test!(library_kforge1785, TemplateId::KForgeToken1785);

macro_rules! pair_test {
    ($name:ident, $i:expr) => {
        #[test]
        #[ignore = "heavy (minutes per program pair): cargo test -p kob-tests --test redteam_library_tests -- --ignored"]
        fn $name() {
            let (pa, pb) = shapes::PAIRS[$i];
            let r = run(&format!("library/pair/{}", fx::pair::pair_name(pa, pb)), shapes::pair_shapes(pa, pb));
            r.assert_clean();
        }
    };
}

pair_test!(library_pair_0, 0);
pair_test!(library_pair_1, 1);
pair_test!(library_pair_2, 2);
pair_test!(library_pair_3, 3);
pair_test!(library_pair_4, 4);
pair_test!(library_pair_5, 5);
pair_test!(library_pair_6, 6);
pair_test!(library_pair_7, 7);

#[test]
fn library_claims() {
    let r = run("library/claims", shapes::claim_shapes());
    r.assert_clean();
}

#[test]
fn library_router() {
    let r = run("library/router", shapes::router_shapes());
    r.assert_clean();
}

/// Prints the shapes whose name contains `RT_DESCRIBE` (inputs and outputs), for writing attacks.
#[test]
#[ignore = "debug: RT_DESCRIBE=<name part> cargo test --test redteam_library_tests describe -- --ignored --nocapture"]
fn describe() {
    let pat = std::env::var("RT_DESCRIBE").unwrap_or_default();
    let mut all = vec![];
    for p in [TemplateId::Kcc20Ref, TemplateId::KronToken2433] {
        all.extend(shapes::program_shapes(p));
    }
    all.extend(shapes::pair_shapes(TemplateId::Kcc20Ref, TemplateId::Kcc20Ref));
    all.extend(shapes::claim_shapes());
    all.extend(shapes::router_shapes());
    for (n, b) in all.iter().filter(|(n, _)| n.contains(&pat)) {
        let h = honest(n, b);
        println!("{n}\n{}", rt::describe(&h));
    }
}
