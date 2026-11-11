//! The update notice (`kobd::indexer::release`): every placement carries the release of the build that wrote it (the
//! RELEASE record, `docs/spec/kob1-payload.md`); the indexer reports a release newer than its own once it has seen it on 3
//! accepted placements of at least 2 makers, in `/v1/health` (`newer_release_seen`), the status line and the metrics. One
//! maker, or this build's own release, never raises it; the placements are listed exactly as any other.

mod common;

use common::*;
use kob_protocol::build::*;
use kob_protocol::payload::{self, Record, Release};
use kob_protocol::state::*;
use kob_protocol::tx::SignedTx;
use kobd::testkit::*;

/// A release newer than this build.
fn newer() -> Release {
    let c = Release::build_core();
    Release::new(c.major, c.minor + 1, 0)
}

/// A plain ask of `maker` whose payload announces `release` (`None`: the builders' own, this build's release).
fn ask_with(c: &Ctx, maker: u8, price: i64, release: Option<Release>) -> SignedTx {
    let mut req = c.w.create(AnyState::KobAsk(ask(maker, price)), CARRIER, maker, 10 * WHOLE);
    if let Some(version) = release {
        req.records = vec![Record::Release { version }];
    }
    c.w.sign(&Action::CreateOrder(req))
}

fn notice(c: &Ctx) -> Option<kobd::indexer::release::NewerRelease> {
    c.hs.health.snapshot().newer_release_seen
}

#[tokio::test]
async fn a_newer_release_on_three_placements_of_two_makers_raises_the_notice() {
    let c = Ctx::new();
    // this build's own placements carry its release (the builders append it): never a notice
    let own = c.w.create_tx(AnyState::KobAsk(ask(MAKER_A, P250)), CARRIER, MAKER_A, 10 * WHOLE);
    let p = payload::decode(&own.tx.payload).unwrap().unwrap();
    assert_eq!(payload::release_of(&p), Release::of_build());
    let own_b = ask_with(&c, MAKER_B, P250, None);
    c.push(&[&own, &own_b]).await;
    assert_eq!(notice(&c), None);

    // a newer release, three placements of ONE maker: not enough
    let first = ask_with(&c, MAKER_A, P260, Some(newer()));
    c.push(&[&first]).await;
    let first_daa = c.daa();
    let more: Vec<SignedTx> = (1..=2).map(|k| ask_with(&c, MAKER_A, P260 + k, Some(newer()))).collect();
    c.push(&more.iter().collect::<Vec<_>>()).await;
    assert_eq!(notice(&c), None, "one maker cannot raise it");
    assert!(!c.hs.health.snapshot().status_line().contains("newer KOB release"));

    // a second maker: the notice, with the DAA of the first placement that announced it
    let other = ask_with(&c, MAKER_B, P260, Some(newer()));
    c.push(&[&other]).await;
    let n = notice(&c).expect("the notice");
    assert_eq!(n.version, newer());
    assert_eq!(n.first_seen_daa, first_daa);
    let h = c.hs.health.snapshot();
    assert!(
        h.status_line().ends_with(&format!(" | a newer KOB release is in use on chain ({}): update", newer())),
        "{}",
        h.status_line()
    );
    assert!(h.metrics_text().contains("\nkob_newer_release_seen 1\n"));
    // kept in the store: a restart shows it again
    assert_eq!(c.hs.ingest.lock().unwrap().newer_release_seen().unwrap(), Some(n));

    // informational only: every placement is an order like any other
    for t in [&own, &own_b, &first, &more[0], &more[1], &other] {
        assert_eq!(c.hs.status(&c.w.cov(t, 0)), "open");
    }
}

#[tokio::test]
async fn a_placement_the_indexer_refuses_does_not_count() {
    let c = Ctx::new();
    // three makers announce the newer release, but one placement's record does not match its output (refused): two remain
    let a = ask_with(&c, MAKER_A, P250, Some(newer()));
    let b = ask_with(&c, MAKER_B, P250, Some(newer()));
    let mut built = build(&Action::CreateOrder({
        let mut r = c.w.create(AnyState::KobAsk(ask(TAKER, P250)), CARRIER, TAKER, 10 * WHOLE);
        r.records = vec![Record::Release { version: newer() }];
        r
    }))
    .unwrap();
    // the record announces another price than the output's script commits to
    let mut p = payload::decode(&built.tx.payload).unwrap().unwrap();
    let Record::Order { state, .. } = &mut p.records[0] else { panic!("order record first") };
    let AnyState::KobAsk(mut s) = AnyState::decode(kob_protocol::artifacts::TemplateId::KobAsk, state).unwrap() else { panic!() };
    s.price += 1;
    *state = AnyState::KobAsk(s).encode();
    built.tx.payload = payload::encode(&p.records).unwrap();
    let forged = c.w.resign(built, false);
    c.push(&[&a, &b, &forged]).await;
    assert_eq!(c.hs.status(&c.w.cov(&a, 0)), "open");
    assert_eq!(notice(&c), None, "only accepted placements count");
    // a third accepted one passes the bar
    let third = ask_with(&c, MAKER_A, P260, Some(newer()));
    c.push(&[&third]).await;
    assert_eq!(notice(&c).map(|n| n.version), Some(newer()));
}
