//! Every pair order shape the `kob-protocol` builders produce (`pair_scenarios`: creation, cancels with strays of both
//! tokens, refunds and kills, routes through the KAS books, netting of opposite pair orders, conditional legs armed in
//! both evidence modes, updates (arm, trail), if-done fills of both sides, bookings and repeat merges), on the four
//! family mixes of a pair A/B, executed in the script engine against the pair templates compiled from source
//! (`common/pair_harness.rs`). Every input of every shape must pass. Under an ablation run a rejected shape prints
//! `ABLATION-POS-FAIL` (a weakened contract that refuses an honest shape).
//! Run: cargo test -p kob-tests --test kob_pair_shapes_tests -- --nocapture --test-threads=1

mod common;
#[path = "../../kob-protocol/tests/common/mod.rs"]
mod fx;
#[path = "common/pair_harness.rs"]
mod ph;

use fx::pair::{pair_name, pair_scenarios};
use ph::{built, family_mixes, Ed, Run, Subs};

#[test]
fn pair_shapes_pass_under_the_compiled_templates() {
    let subs = Subs::compile();
    let mut count = 0;
    for (pa, pb) in family_mixes() {
        let r = Run { subs: &subs, pair: pair_name(pa, pb) };
        for (name, a) in pair_scenarios(pa, pb) {
            r.ok(&Ed::new(&name, &built(a)));
            count += 1;
        }
    }
    println!("pair shapes: {count}");
    assert!(count >= 4 * 60, "pair shapes: {count}");
}
