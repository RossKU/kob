//! The order carrier and the fee (`docs/spec/order-types.md`, *Defaults*: carriers).
//!
//! Every builder scenario of the golden-vector set on every token program, and the pair scenarios on a set of program
//! pairs, built with every order-side carrier (order UTXOs, custodies, the maker's token UTXOs, `deliveryCarrier`,
//! `exitCarrier`) at one value and the matcher's taker token carrier at its fixture value, signed with the fixture keys,
//! budgets tightened, and validated in the engine. A token output carries a covenant id, so its KIP-9 storage plurality is
//! 2 and its storage mass is `4 × 10^12 / carrier`: the relay fee (`rate × max(compute, normalized transient)`) never
//! prices it, the storage-inclusive priority fee (`rate × max(fee mass, storage)`) does once the storage mass exceeds the
//! transaction's fee mass.
//!
//! `the_default_order_carrier_adds_no_relay_fee` is the gate: at [`DEFAULT_ORDER_CARRIER`] no shape pays more relay fee
//! than at 10 KAS, none comes near the block storage limit, and the opt-in priority fee rises by a bounded amount. The
//! measurement over every candidate carrier (10 / 5 / 3 / 2 / 1.5 / 1 / 0.5 / 0.25 / 0.1 KAS) is `#[ignore]`d:
//!
//! ```text
//! KOB_CARRIER_FEES_OUT=target/carrier_fees.csv cargo test -p kob-protocol --test carrier_fees -- --ignored --nocapture
//! ```

mod common;

use std::fmt::Write as _;

use common::{with_carriers, Carriers, KAS};
use kob_protocol::artifacts::TemplateId;
use kob_protocol::build::{build, Action};
use kob_protocol::defaults::DEFAULT_ORDER_CARRIER;
use kob_protocol::tx::{finalize, min_fee, priority_fee, sign_locally, FinalizeOptions, MassReport, BLOCK_STORAGE_LIMIT};

/// The fee-relevant numbers of one built, signed and validated transaction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Row {
    bytes: u64,
    fee_mass: u64,
    storage: u64,
    relay: u64,
    priority: u64,
}

fn measure(action: &Action) -> Result<Row, String> {
    let built = build(action).map_err(|e| e.to_string())?;
    let sigs = sign_locally(&built, &common::keys()).map_err(|e| e.to_string())?;
    let signed = finalize(&built, &sigs, FinalizeOptions { tighten_budgets: true }).map_err(|e| e.to_string())?;
    kob_protocol::verify::validate_signed(&signed).map_err(|e| e.to_string())?;
    let m: MassReport = signed.fee.mass;
    let rate = signed.fee.fee_rate;
    Ok(Row {
        bytes: m.size,
        fee_mass: m.fee_mass,
        storage: m.storage,
        relay: min_fee(&m, rate),
        priority: priority_fee(&m, rate).max(min_fee(&m, rate)),
    })
}

/// The program pairs of the pair shapes: one per family mix, the 8/8 and P2 programs, and KaspaCom on both sides.
const PAIRS: [(TemplateId, TemplateId); 7] = [
    (TemplateId::Kcc20Ref, TemplateId::Kcc20Ref),
    (TemplateId::Kcc20Ref8x8, TemplateId::Kcc20Ref8x8),
    (TemplateId::Kcc20P2, TemplateId::Kcc20P2),
    (TemplateId::Kcc20KaspaCom025, TemplateId::Kcc20KaspaCom025),
    (TemplateId::Kcc20Ref, TemplateId::KronToken2433),
    (TemplateId::KronToken2433, TemplateId::Kcc20Ref),
    (TemplateId::KronToken2433, TemplateId::KronToken2732),
];

/// Every shape, tagged `<program or pair>/<scenario>`, built under the carriers in force.
fn shapes() -> Vec<(String, Action)> {
    let mut v = vec![];
    for p in common::PROGRAMS {
        v.extend(common::scenarios_on(p).into_iter().map(|(n, a)| (format!("{}/{n}", p.name()), a)));
    }
    for (pa, pb) in PAIRS {
        let tag = common::pair::pair_name(pa, pb);
        v.extend(common::pair::pair_scenarios(pa, pb).into_iter().map(|(n, a)| (format!("{tag}/{n}"), a)));
    }
    v
}

/// The budget-grid shapes measure script units, not carriers (and run thousands of builds).
fn is_grid(name: &str) -> bool {
    name.contains("/grid.")
}

fn measure_all(c: Carriers) -> Vec<(String, Result<Row, String>)> {
    with_carriers(c, || shapes().into_iter().filter(|(n, _)| !is_grid(n)).map(|(n, a)| (n.clone(), measure(&a))).collect())
}

/// The gate: at the default order carrier every shape builds and validates, pays no more relay fee than at 10 KAS (the
/// fee builders, the executor and the wallet pay by default), commits at most 40% of the block storage limit, and pays at
/// most 0.15 KAS more in the opt-in storage-inclusive priority mode (measured: 0.1305 KAS, a KRON / KRON pair stop armed
/// with evidence; a placement's storage mass already exceeds its fee mass at 10 KAS, so no carrier below it is free in that
/// mode, `kob_protocol::defaults::DEFAULT_ORDER_CARRIER`).
#[test]
fn the_default_order_carrier_adds_no_relay_fee() {
    let ten = measure_all(Carriers::order_side(10 * KAS));
    let def = measure_all(Carriers::order_side(DEFAULT_ORDER_CARRIER));
    assert_eq!(ten.len(), def.len());
    let (mut compared, mut same, mut priority_up) = (0, 0, 0);
    for ((n, a), (m, b)) in ten.iter().zip(&def) {
        assert_eq!(n, m);
        let a = a.as_ref().unwrap_or_else(|e| panic!("{n} at 10 KAS: {e}"));
        let b = b.as_ref().unwrap_or_else(|e| panic!("{n} at the default carrier: {e}"));
        assert!(b.bytes <= a.bytes && b.fee_mass <= a.fee_mass && b.relay <= a.relay, "{n}: {b:?} against {a:?} at 10 KAS");
        assert!(b.storage <= BLOCK_STORAGE_LIMIT * 2 / 5, "{n}: storage {}", b.storage);
        assert!(b.priority <= a.priority + 15 * KAS / 100, "{n}: priority fee {} against {}", b.priority, a.priority);
        compared += 1;
        same += usize::from(b.relay == a.relay);
        priority_up += usize::from(b.priority > a.priority);
    }
    // the relay fees that differ are lower (placements and amends of bids, entries and pair orders: their records carry the
    // smaller carrier and value in fewer bytes)
    assert!(compared > 1_000, "{compared} shapes");
    println!("{compared} shapes: {same} at the same relay fee, the rest lower; {priority_up} pay more in priority mode");
}

/// KaspaCom KCC20 0.2.5 at its floor (0.5 KAS): every single-token shape builds and pays no more relay fee than at 10 KAS.
/// (Its pair shapes with evidence or netting exceed the block storage limit at 0.5 KAS, as every program's do.)
#[test]
fn kaspacom_builds_at_its_floor() {
    let kc = TemplateId::Kcc20KaspaCom025;
    let floor = kc.min_token_output().expect("KaspaCom has a token-output floor");
    let only = |rows: Vec<(String, Result<Row, String>)>| -> Vec<(String, Result<Row, String>)> {
        rows.into_iter().filter(|(n, _)| n.starts_with(&format!("{}/", kc.name()))).collect()
    };
    let ten = only(measure_all(Carriers::order_side(10 * KAS)));
    let low = only(measure_all(Carriers::order_side(floor)));
    assert!(ten.len() > 100);
    for ((n, a), (_, b)) in ten.iter().zip(&low) {
        let (a, b) = (a.as_ref().unwrap(), b.as_ref().unwrap_or_else(|e| panic!("{n} at 0.5 KAS: {e}")));
        assert!(b.relay <= a.relay, "{n}");
    }
}

/// The floors: a KaspaCom KCC20 0.2.5 token output below 0.5 KAS and any order carrier below the KIP-9 dust bound are
/// refused by the builders; KaspaCom builds at exactly 0.5 KAS.
#[test]
fn carriers_below_the_floors_are_refused() {
    let kc = TemplateId::Kcc20KaspaCom025;
    let floor = kc.min_token_output().expect("KaspaCom has a token-output floor");
    assert_eq!(floor, KAS / 2);
    let pick = |c: u64, p: TemplateId, name: &str| -> Action {
        with_carriers(Carriers::order_side(c), || {
            common::scenarios_on(p).into_iter().find(|(n, _)| n == name).unwrap_or_else(|| panic!("{name}")).1
        })
    };
    for name in ["create.ask", "create.bid", "take.bid.partial", "create.ifdBid"] {
        measure(&pick(floor, kc, name)).unwrap_or_else(|e| panic!("KaspaCom {name} at its floor: {e}"));
        assert!(measure(&pick(floor - 1, kc, name)).is_err(), "KaspaCom {name} below its floor");
        let dust = kob_protocol::tx::DUST_OUTPUT_MIN;
        assert!(measure(&pick(dust - 1, TemplateId::Kcc20Ref, name)).is_err(), "{name} below the dust bound");
    }
}

/// Measurement (not a gate): every shape at every candidate carrier; the CSV holds one row per shape and carrier, the
/// summary the shapes whose priority fee rises against 10 KAS.
#[test]
#[ignore = "measurement: writes the carrier fee table (KOB_CARRIER_FEES_OUT)"]
fn carrier_fee_table() {
    let candidates: [u64; 9] = [10 * KAS, 5 * KAS, 3 * KAS, 2 * KAS, 3 * KAS / 2, KAS, KAS / 2, KAS / 4, KAS / 10];
    let ten = measure_all(Carriers::order_side(10 * KAS));
    let mut csv = String::from("carrier_sompi,shape,bytes,fee_mass,storage,relay_fee,priority_fee,error\n");
    let mut summary = String::new();
    for c in candidates {
        let rows = if c == 10 * KAS { ten.clone() } else { measure_all(Carriers::order_side(c)) };
        let (mut ok, mut failed, mut relay_up, mut prio_up) = (0, 0, vec![], vec![]);
        let mut tightest: Option<(String, f64)> = None;
        for ((n, base), (m, r)) in ten.iter().zip(&rows) {
            assert_eq!(n, m);
            match r {
                Ok(r) => {
                    ok += 1;
                    let _ = writeln!(csv, "{c},{n},{},{},{},{},{},", r.bytes, r.fee_mass, r.storage, r.relay, r.priority);
                    if let Ok(b) = base {
                        if r.relay > b.relay {
                            relay_up.push(format!("{n} ({} -> {})", b.relay, r.relay));
                        }
                        if r.priority > b.priority {
                            prio_up.push(format!(
                                "{n} ({} -> {}, storage {} fee mass {})",
                                b.priority, r.priority, r.storage, r.fee_mass
                            ));
                        }
                    }
                    let ratio = r.storage as f64 / r.fee_mass as f64;
                    if tightest.as_ref().is_none_or(|t| ratio > t.1) {
                        tightest = Some((format!("{n} storage {} / fee mass {}", r.storage, r.fee_mass), ratio));
                    }
                }
                Err(e) => {
                    failed += 1;
                    let _ = writeln!(csv, "{c},{n},,,,,,{}", e.replace([',', '\n'], ";"));
                }
            }
        }
        let _ = writeln!(
            summary,
            "carrier {:.2} KAS: built {ok}, refused {failed}, relay fee up {}, priority fee up {}; tightest {:?}",
            c as f64 / KAS as f64,
            relay_up.len(),
            prio_up.len(),
            tightest
        );
        for p in prio_up.iter().take(12) {
            let _ = writeln!(summary, "    priority up: {p}");
        }
        for p in relay_up.iter().take(5) {
            let _ = writeln!(summary, "    relay up: {p}");
        }
    }
    println!("{summary}");
    let path = std::env::var("KOB_CARRIER_FEES_OUT").unwrap_or_else(|_| "../../target/carrier_fees.csv".into());
    std::fs::write(&path, &csv).unwrap();
    println!("wrote {path} ({} rows)", csv.lines().count() - 1);
}
