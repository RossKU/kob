//! The listing gate (`sanity::check`) for states no order should be listed with: a stop entry whose trigger is beyond its
//! limit (armable, never fillable: C6-6), and a state built in memory with a number the 8-byte state pushes cannot carry
//! (`i64::MIN` in a field its mode does not read: every builder would fail to encode it, C6-3).

use kob_executor::sanity::check;
use kob_executor::testkit::*;
use kob_protocol::state::*;

#[test]
fn a_stop_entry_whose_trigger_is_beyond_its_limit_is_not_listed() {
    let b = ifd_bid(1, 10);
    assert!(check(&AnyState::KobIfdBid(IfdBidState { entry_stop: b.price, ..b.clone() })).is_ok(), "a trigger at the limit is fine");
    let e = check(&AnyState::KobIfdBid(IfdBidState { entry_stop: b.price + 1, ..b.clone() })).unwrap_err();
    assert_eq!(e, "bad_state:entryStop:beyond_limit");
    let a = ifd_ask(1);
    assert!(check(&AnyState::KobIfdAsk(IfdAskState { entry_stop: a.price, ..a.clone() })).is_ok());
    let e = check(&AnyState::KobIfdAsk(IfdAskState { entry_stop: a.price - 1, ..a.clone() })).unwrap_err();
    assert_eq!(e, "bad_state:entryStop:beyond_limit");
    // a limit entry (entryStop 0) is not a stop entry
    assert!(check(&AnyState::KobIfdAsk(IfdAskState { entry_stop: 0, ..a })).is_ok());
}

#[test]
fn an_unencodable_state_is_not_listed() {
    let s = AskState { price_end: i64::MIN, ..ask(1, 250_000_000) };
    assert!(AnyState::KobAsk(s.clone()).try_encode().is_err(), "i64::MIN has no canonical 8-byte push");
    assert_eq!(check(&AnyState::KobAsk(s)).unwrap_err(), "bad_state:encoding:unencodable");
}

/// Live TN10 shapes (2026-10-06, protocol v3, prices per whole token): market orders of TETH and TBTC are IOC auctions over
/// 300 DAA from the touch (rise for a buy, decay for a sell; a pair market order decays in B per whole A). Their slopes (1e9 to
/// 3e10 per DAA) passed the protocol's builder gate but the listing gate bounded `slope x (2^34 / decayStep)` by 2^60, as if
/// the path were evaluated for 545 years, and left every one unlisted (`bad_state:slope:overflow`): never matched, never
/// killed, the escrow locked until the maker cancelled. The matcher evaluates a path only before the order's refund time
/// (`t < refund_due <= expiryDaa`, from `origin >= activeFrom`), so the gate bounds the product over that window.
const LIVE_TETH_RISE_BID: &str = r#"{"kind":"KobBid","state":{"activeFrom":"589237500","decayStep":"1","deliveryCarrier":"1000000000","expiryDaa":"589237800","extensionCommitment":"0000000000000000000000000000000000000000000000000000000000000000","interval":"0","maker":"19d7e0528509a5b9b523b8fefb1617833e09f82b32af4d31ca67a3f65ffe22e8","maxFill":"0","minFill":"1","price":"6323507075544","priceEnd":"6513212287810","refundTip":"5000000","reserve":"0","scale":"100000000","slope":"948526062","tif":"1","tip":"0","tokenCovId":"827f13e92508094b97d367b4dd9ff95377255c4b163e39a3feebc431baea0926","tokenTplHash":"40fef59a59bd76991f4d4e2101d1e3e34860997b89fe7714532637cec482a9d7","tplPrefixLen":"1","tplSuffixLen":"6707"}}"#;
const LIVE_TBTC_DECAY_ASK: &str = r#"{"kind":"KobAsk","state":{"activeFrom":"589237229","amountLeft":"844","decayStep":"1","expiryDaa":"589237529","interval":"0","maker":"3d457cd98b931431d5998084909cd17d616a2a4e1d87545f45d97fed08b925a3","maxFill":"0","minFill":"1","price":"200132824074995","priceEnd":"194128839352746","refundTip":"5000000","scale":"100000000","slope":"30019923612","tif":"1","tip":"0","tokenCovId":"7832859429d5a9ef5778a31af8371daaa94fd6f49069d90d1c85a8e8fc257754","tokenTplHash":"40fef59a59bd76991f4d4e2101d1e3e34860997b89fe7714532637cec482a9d7","tplPrefixLen":"1","tplSuffixLen":"6707"}}"#;
const LIVE_TBTC_RISE_BID: &str = r#"{"kind":"KobBid","state":{"activeFrom":"589237216","decayStep":"1","deliveryCarrier":"1000000000","expiryDaa":"589237516","extensionCommitment":"0000000000000000000000000000000000000000000000000000000000000000","interval":"0","maker":"6b7b25bc5d08851771192060682aa7322dd9bd35f1e21b5895251dad18d5cd4e","maxFill":"0","minFill":"1","price":"200092544019457","priceEnd":"206095320340040","refundTip":"5000000","reserve":"0","scale":"100000000","slope":"30013881603","tif":"1","tip":"0","tokenCovId":"7832859429d5a9ef5778a31af8371daaa94fd6f49069d90d1c85a8e8fc257754","tokenTplHash":"40fef59a59bd76991f4d4e2101d1e3e34860997b89fe7714532637cec482a9d7","tplPrefixLen":"1","tplSuffixLen":"6707"}}"#;
const LIVE_TBTC_PAIR_DECAY: &str = r#"{"kind":"KobPair","state":{"activeFrom":"589236184","amountLeft":"9445","custody":"9445","decayStep":"1","deliveryCarrier":"1000000000","expiryDaa":"589236484","interval":"0","maker":"5e7fdf3506c07c9e8ce467d2c244fecb0b17b707b0aedaf5abdfc40a36e3e87b","maxFill":"0","minFill":"1","price":"8566990408090","priceEnd":"8309980695848","refundTip":"9400000","sCovId":"7832859429d5a9ef5778a31af8371daaa94fd6f49069d90d1c85a8e8fc257754","sFamily":"1","sPre":"1","sScale":"100000000","sSuf":"6707","sTplHash":"40fef59a59bd76991f4d4e2101d1e3e34860997b89fe7714532637cec482a9d7","side":"1","slope":"1285048562","tCovId":"7cfe8aa05f45fbabdd115bcdf2e369e43f0ab59d429c40d17f3f0fa4bbe75652","tExt":"0000000000000000000000000000000000000000000000000000000000000000","tFamily":"1","tPre":"1","tScale":"100000000","tSuf":"6707","tTplHash":"40fef59a59bd76991f4d4e2101d1e3e34860997b89fe7714532637cec482a9d7","tif":"1","tip":"0"}}"#;

fn live(json: &str) -> AnyState {
    serde_json::from_str(json).expect("a live order state")
}

#[test]
fn auctions_of_high_priced_tokens_are_listed_over_their_own_window() {
    for (name, json) in [
        ("TETH rise bid", LIVE_TETH_RISE_BID),
        ("TBTC decay ask", LIVE_TBTC_DECAY_ASK),
        ("TBTC rise bid", LIVE_TBTC_RISE_BID),
        ("TBTC/TUSD pair decay", LIVE_TBTC_PAIR_DECAY),
    ] {
        assert_eq!(check(&live(json)), Ok(()), "{name}");
    }
}

#[test]
fn a_decay_path_is_still_bounded_over_a_long_window() {
    // the same TBTC decay as a GTC Dutch order of about 90 days (7.8e7 DAA): 3e10 x 7.8e7 = 2.3e18 > 2^60, refused
    let AnyState::KobAsk(a) = live(LIVE_TBTC_DECAY_ASK) else { panic!("an ask") };
    let gtc = AskState { tif: 0, expiry_daa: a.active_from + 78_000_000, ..a.clone() };
    assert_eq!(check(&AnyState::KobAsk(gtc)).unwrap_err(), "bad_state:slope:overflow");
    // a Dutch over 90 days that moves 100 times slower fits
    let slow = AskState { tif: 0, expiry_daa: a.active_from + 78_000_000, slope: a.slope / 100, ..a.clone() };
    assert_eq!(check(&AnyState::KobAsk(slow)), Ok(()));
    // without an activation the covenant's origin is 0 and t - origin is the whole chain's DAA: refused as before
    let no_origin = AskState { active_from: 0, ..a.clone() };
    assert_eq!(check(&AnyState::KobAsk(no_origin)).unwrap_err(), "bad_state:slope:overflow");
    // a pair decay likewise
    let AnyState::KobPair(p) = live(LIVE_TBTC_PAIR_DECAY) else { panic!("a pair order") };
    let p_gtc = PairState { tif: 0, expiry_daa: p.active_from + (1 << 40), slope: i64::MAX / 1_000, ..p.clone() };
    assert_eq!(check(&AnyState::KobPair(p_gtc)).unwrap_err(), "bad_state:slope:overflow");
}
