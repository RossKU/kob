//! The KOB protocol model as the indexer sees it.
//!
//! The module classifies which templates are orders, which entry a dispatch tag selects, how an
//! entry's arguments decode, and which fields of an order are mutable. Everything
//! protocol-specific comes from `kob-protocol` (pinned templates, typed states). This module only
//! adds the classification the indexer needs. Both token families are covered. A KRON kind
//! (`KobAskKron`, ...) is classified like its KCC-20 counterpart (`TemplateId::base`).

use kob_protocol::artifacts::{template, token_template, TemplateId};
use kob_protocol::family::Family;
use kob_protocol::state::AnyState;
use std::collections::HashMap;
use std::sync::OnceLock;

/// Token family byte of the KOB1 placement record for KCC-20 (`Family::code`; KRON is `0x02`).
pub const FAMILY_KCC20: u8 = 1;

/// The order kinds of both families (KRON: both delivery builds).
pub const ORDER_KINDS: [TemplateId; 21] = [
    TemplateId::KobAsk,
    TemplateId::KobBid,
    TemplateId::KobCondAsk,
    TemplateId::KobCondBid,
    TemplateId::KobIfdBid,
    TemplateId::KobIfdAsk,
    TemplateId::KobAskKron,
    TemplateId::KobBidKron,
    TemplateId::KobCondAskKron,
    TemplateId::KobCondBidKron,
    TemplateId::KobIfdBidKron,
    TemplateId::KobIfdAskKron,
    TemplateId::KobAskKronD0,
    TemplateId::KobBidKronD0,
    TemplateId::KobCondAskKronD0,
    TemplateId::KobCondBidKronD0,
    TemplateId::KobIfdBidKronD0,
    TemplateId::KobIfdAskKronD0,
    TemplateId::KobPair,
    TemplateId::KobCondPair,
    TemplateId::KobIfdPair,
];

/// True for the order kinds of both families.
pub fn is_order(id: TemplateId) -> bool {
    ORDER_KINDS.contains(&id)
}

/// The side of a KAS-book kind: 1 if the order sells the token (holds tokens in custody), 2 if it
/// buys the token (holds KAS), 0 for other kinds.
///
/// The pair kinds are one template for both sides. [`side_of`] reads their side from the state.
pub fn side(id: TemplateId) -> u8 {
    match id.base() {
        TemplateId::KobAsk | TemplateId::KobCondAsk | TemplateId::KobIfdAsk => 1,
        TemplateId::KobBid | TemplateId::KobCondBid | TemplateId::KobIfdBid => 2,
        _ => 0,
    }
}

/// The side of an order: 1 sells its base token, 2 buys it.
///
/// It is the kind's side for the KAS kinds and the state's `side` for a pair order. A pair `ASK`
/// (1) sells A for B and a pair `BID` (2) buys A with B. A sell-first `KobIfdPair` is 1 and a
/// buy-first one is 2.
pub fn side_of(s: &AnyState) -> u8 {
    match s {
        AnyState::KobPair(p) => side_code(p.side),
        AnyState::KobCondPair(p) => side_code(p.side),
        AnyState::KobIfdPair(p) => side_code(p.side),
        other => side(other.template_id()),
    }
}

fn side_code(side: i64) -> u8 {
    match side {
        kob_protocol::state::SIDE_ASK => 1,
        kob_protocol::state::SIDE_BID => 2,
        _ => 0,
    }
}

/// A resting limit order with a plain `price`, which appears in the books.
///
/// A pair order is in no KAS book, because it has no KAS price. It appears in the pair books
/// (`/v1/pairs`).
pub fn in_book(id: TemplateId) -> bool {
    matches!(id.base(), TemplateId::KobAsk | TemplateId::KobBid | TemplateId::KobIfdAsk | TemplateId::KobIfdBid)
}

/// The order an if-done entry spawns on each fill (of the entry's family and delivery build).
pub fn exit_template(id: TemplateId) -> Option<TemplateId> {
    let same = |k: TemplateId| if id.is_delivery0() { k.in_family(id.family()).delivery0() } else { k.in_family(id.family()) };
    match id.base() {
        TemplateId::KobIfdBid => Some(same(TemplateId::KobCondAsk)),
        TemplateId::KobIfdAsk => Some(same(TemplateId::KobCondBid)),
        TemplateId::KobIfdPair => Some(TemplateId::KobCondPair),
        _ => None,
    }
}

type EntryTable = HashMap<(TemplateId, [u8; 4]), String>;

fn entry_table() -> &'static EntryTable {
    static T: OnceLock<EntryTable> = OnceLock::new();
    T.get_or_init(|| {
        let mut m = HashMap::new();
        // Artifact-backed templates only. The raw KRON token programs have no ABI.
        for id in TemplateId::ALL.into_iter().filter(|t| t.is_artifact()) {
            for (name, e) in &template(id).contract().entries {
                m.insert((id, *e.dispatch_tag.as_bytes()), name.clone());
            }
        }
        m
    })
}

/// Name of the entry a dispatch tag selects in a template.
pub fn entry_name(id: TemplateId, tag: [u8; 4]) -> Option<&'static str> {
    entry_table().get(&(id, tag)).map(|s| s.as_str())
}

/// Identifies a revealed redeem script as an instance of one of the pinned artifact-backed
/// templates and returns its state span.
///
/// The redeem script is the last push of a covenant input's signature script. The templates are the
/// order kinds and the KCC-20 programs of both families' orders. KRON token programs are
/// [`identify_kron_token`].
pub fn identify_redeem(redeem: &[u8]) -> Option<(TemplateId, &[u8])> {
    TemplateId::ALL.into_iter().filter(|t| t.is_artifact()).find_map(|id| template(id).state_of(redeem).map(|s| (id, s)))
}

/// Identifies a revealed redeem script as one of the token programs pinned as raw redeem scripts.
///
/// These are the KRON programs (profile source `raw-redeem`), which [`identify_redeem`] does not
/// cover.
pub fn identify_kron_token(redeem: &[u8]) -> Option<(TemplateId, &[u8])> {
    TemplateId::TOKEN_PROGRAMS
        .into_iter()
        .filter(|t| !t.is_artifact())
        .find_map(|id| token_template(id).state_of(redeem).map(|s| (id, s)))
}

/// One-byte code of an order template in the record log.
///
/// It is the KOB1 kind code. The high bit (`0x80`) is set for the KRON kinds, which share the codes
/// of their KCC-20 twins. Bit `0x40` is set for the delivery id_type 0 builds of the KRON kinds
/// (`*KronD0`). So a code below `0x80` is a KCC-20 kind, and a code without `0x40` is a plain
/// build.
pub fn wire_code(t: TemplateId) -> u8 {
    t.kind_code().expect("order template")
        | if t.family() == Family::Kron { 0x80 } else { 0 }
        | if t.is_delivery0() { 0x40 } else { 0 }
}

/// Inverse of [`wire_code`].
pub fn from_wire_code(code: u8) -> Option<TemplateId> {
    let family = if code & 0x80 != 0 { Family::Kron } else { Family::Kcc20 };
    let id = TemplateId::from_kind_code(family, code & 0x3f)?;
    match code & 0x40 != 0 {
        false => Some(id),
        true if family == Family::Kron && id.delivery0() != id => Some(id.delivery0()),
        true => None,
    }
}

/// The family of a token program template hash, or `None` when it is not a supported program.
pub fn token_family(tpl_hash: &[u8; 32]) -> Option<Family> {
    kob_protocol::artifacts::token_template_by_hash(tpl_hash).map(|t| t.family)
}

/// A decoded `nb` argument (the fixed 8-byte quantity of `settle` / `fill`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Nb {
    /// `0`: a refund (`settle(0)`).
    Zero,
    /// `n > 0` base units.
    Fill(i64),
    /// Repeat merge argument `-(k * 2^53 + m)`: the booked exit at input `k` sold `m` base units.
    Merge { k: usize, m: i64 },
}

/// Decodes an 8-byte sign-magnitude little-endian `nb` push.
pub fn nb(push: &[u8]) -> Option<Nb> {
    let b: [u8; 8] = push.try_into().ok()?;
    let neg = b[7] & 0x80 != 0;
    let mag = u64::from_le_bytes(b) & !(1u64 << 63);
    let mag = i64::try_from(mag).ok()?;
    Some(match (neg, mag) {
        (_, 0) => Nb::Zero,
        (false, n) => Nb::Fill(n),
        (true, v) => {
            let shift = kob_protocol::state::MERGE_SHIFT;
            Nb::Merge { k: usize::try_from(v / shift).ok()?, m: v % shift }
        }
    })
}

/// The mutable fields of an order, the splice windows of `docs/spec/matcher.md` section 1.1.
///
/// A field an order kind does not have is ignored.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Mutable {
    /// `amountLeft` (base units).
    pub amount: Option<i64>,
    /// `armed`.
    pub armed: Option<i64>,
    /// `stopPrice`.
    pub stop: Option<i64>,
    /// `rptAmount`.
    pub rpt: Option<i64>,
    /// A pair order's exact custody (`custody`): the S it holds, or an entry's B escrow or prefund.
    pub custody: Option<i64>,
}

/// Current values of the mutable fields.
pub fn mutable_of(s: &AnyState) -> Mutable {
    match s {
        AnyState::KobAsk(a) | AnyState::KobAskKron(a) => Mutable { amount: Some(a.amount_left), ..Default::default() },
        AnyState::KobPair(a) => Mutable { amount: Some(a.amount_left), custody: Some(a.custody), ..Default::default() },
        AnyState::KobCondPair(a) => Mutable {
            amount: Some(a.amount_left),
            armed: Some(a.armed),
            stop: Some(a.stop_price),
            rpt: None,
            custody: Some(a.custody),
        },
        AnyState::KobIfdPair(a) => Mutable {
            amount: Some(a.amount_left),
            armed: Some(a.armed),
            stop: None,
            rpt: Some(a.rpt_amount),
            custody: Some(a.custody),
        },
        AnyState::KobCondAsk(a) | AnyState::KobCondAskKron(a) => {
            Mutable { amount: Some(a.amount_left), armed: Some(a.armed), stop: Some(a.stop_price), rpt: None, custody: None }
        }
        AnyState::KobCondBid(a) | AnyState::KobCondBidKron(a) => {
            Mutable { amount: Some(a.amount_left), armed: Some(a.armed), stop: Some(a.stop_price), rpt: None, custody: None }
        }
        AnyState::KobIfdBid(a) | AnyState::KobIfdBidKron(a) => {
            Mutable { amount: Some(a.amount_left), armed: Some(a.armed), stop: None, rpt: Some(a.rpt_amount), custody: None }
        }
        AnyState::KobIfdAsk(a) | AnyState::KobIfdAskKron(a) => {
            Mutable { amount: Some(a.amount_left), armed: Some(a.armed), stop: None, rpt: Some(a.rpt_amount), custody: None }
        }
        AnyState::KobBid(_) | AnyState::KobBidKron(_) => Mutable::default(),
    }
}

/// `s` with the given mutable fields replaced (fields the kind does not have are ignored).
pub fn with_mutable(s: &AnyState, m: Mutable) -> AnyState {
    let mut s = s.clone();
    match &mut s {
        AnyState::KobAsk(a) | AnyState::KobAskKron(a) => {
            if let Some(v) = m.amount {
                a.amount_left = v;
            }
        }
        AnyState::KobPair(a) => {
            if let Some(v) = m.amount {
                a.amount_left = v;
            }
            if let Some(v) = m.custody {
                a.custody = v;
            }
        }
        AnyState::KobCondPair(a) => {
            if let Some(v) = m.amount {
                a.amount_left = v;
            }
            if let Some(v) = m.armed {
                a.armed = v;
            }
            if let Some(v) = m.stop {
                a.stop_price = v;
            }
            if let Some(v) = m.custody {
                a.custody = v;
            }
        }
        AnyState::KobIfdPair(a) => {
            if let Some(v) = m.amount {
                a.amount_left = v;
            }
            if let Some(v) = m.armed {
                a.armed = v;
            }
            if let Some(v) = m.rpt {
                a.rpt_amount = v;
            }
            if let Some(v) = m.custody {
                a.custody = v;
            }
        }
        AnyState::KobCondAsk(a) | AnyState::KobCondAskKron(a) => {
            if let Some(v) = m.amount {
                a.amount_left = v;
            }
            if let Some(v) = m.armed {
                a.armed = v;
            }
            if let Some(v) = m.stop {
                a.stop_price = v;
            }
        }
        AnyState::KobCondBid(a) | AnyState::KobCondBidKron(a) => {
            if let Some(v) = m.amount {
                a.amount_left = v;
            }
            if let Some(v) = m.armed {
                a.armed = v;
            }
            if let Some(v) = m.stop {
                a.stop_price = v;
            }
        }
        AnyState::KobIfdBid(a) | AnyState::KobIfdBidKron(a) => {
            if let Some(v) = m.amount {
                a.amount_left = v;
            }
            if let Some(v) = m.armed {
                a.armed = v;
            }
            if let Some(v) = m.rpt {
                a.rpt_amount = v;
            }
        }
        AnyState::KobIfdAsk(a) | AnyState::KobIfdAskKron(a) => {
            if let Some(v) = m.amount {
                a.amount_left = v;
            }
            if let Some(v) = m.armed {
                a.armed = v;
            }
            if let Some(v) = m.rpt {
                a.rpt_amount = v;
            }
        }
        AnyState::KobBid(_) | AnyState::KobBidKron(_) => {}
    }
    s
}

/// Scale, minimum fill, price (the limit or start price), tip and time gates, whichever the kind
/// has.
#[derive(Debug, Clone, Copy, Default)]
pub struct Terms {
    /// Base units per whole token (the price denominator).
    pub scale: i64,
    /// Smallest fill in base units (unless the fill takes everything left).
    pub min_fill: i64,
    /// The quote of a resting order, sompi per whole token.
    ///
    /// It is `None` for the conditional kinds, which have leg prices instead, and for the pair
    /// kinds, whose prices are in token B ([`pair_price`]).
    pub price: Option<i64>,
    /// Priority tip, sompi per whole token (a pair order: per whole A, released to the filler).
    pub tip: i64,
    /// Time in force, for the kinds that have one.
    pub tif: Option<i64>,
    /// `activeFrom`.
    pub active_from: i64,
    /// `expiryDaa`.
    pub expiry_daa: i64,
    /// `refundTip`.
    pub refund_tip: i64,
}

/// The terms of an order state, whichever fields its kind has.
pub fn terms_of(s: &AnyState) -> Terms {
    match s {
        AnyState::KobAsk(a) | AnyState::KobAskKron(a) => Terms {
            scale: a.scale,
            min_fill: a.min_fill,
            price: Some(a.price),
            tip: a.tip,
            tif: Some(a.tif),
            active_from: a.active_from,
            expiry_daa: a.expiry_daa,
            refund_tip: a.refund_tip,
        },
        AnyState::KobBid(a) | AnyState::KobBidKron(a) => Terms {
            scale: a.scale,
            min_fill: a.min_fill,
            price: Some(a.price),
            tip: a.tip,
            tif: Some(a.tif),
            active_from: a.active_from,
            expiry_daa: a.expiry_daa,
            refund_tip: a.refund_tip,
        },
        AnyState::KobCondAsk(a) | AnyState::KobCondAskKron(a) => Terms {
            scale: a.scale,
            min_fill: a.min_fill,
            price: None,
            tip: a.tip,
            tif: None,
            active_from: a.active_from,
            expiry_daa: a.expiry_daa,
            refund_tip: a.refund_tip,
        },
        AnyState::KobCondBid(a) | AnyState::KobCondBidKron(a) => Terms {
            scale: a.scale,
            min_fill: a.min_fill,
            price: None,
            tip: a.tip,
            tif: None,
            active_from: a.active_from,
            expiry_daa: a.expiry_daa,
            refund_tip: a.refund_tip,
        },
        AnyState::KobIfdBid(a) | AnyState::KobIfdBidKron(a) => Terms {
            scale: a.scale,
            min_fill: a.min_fill,
            price: Some(a.price),
            tip: a.tip,
            tif: None,
            active_from: a.active_from,
            expiry_daa: a.expiry_daa,
            refund_tip: a.refund_tip,
        },
        AnyState::KobIfdAsk(a) | AnyState::KobIfdAskKron(a) => Terms {
            scale: a.scale,
            min_fill: a.min_fill,
            price: Some(a.price),
            tip: a.tip,
            tif: None,
            active_from: a.active_from,
            expiry_daa: a.expiry_daa,
            refund_tip: a.refund_tip,
        },
        // A pair order has no KAS quote. Its price is B base units per whole A, and its tip is KAS
        // per whole A.
        AnyState::KobPair(a) => Terms {
            scale: a.a_scale(),
            min_fill: a.min_fill,
            price: None,
            tip: a.tip,
            tif: Some(a.tif),
            active_from: a.active_from,
            expiry_daa: a.expiry_daa,
            refund_tip: a.refund_tip,
        },
        AnyState::KobCondPair(a) => Terms {
            scale: a.a_scale(),
            min_fill: a.min_fill,
            price: None,
            tip: a.tip,
            tif: None,
            active_from: a.active_from,
            expiry_daa: a.expiry_daa,
            refund_tip: a.refund_tip,
        },
        AnyState::KobIfdPair(a) => Terms {
            scale: a.a_scale,
            min_fill: a.min_fill,
            price: None,
            tip: a.tip,
            tif: None,
            active_from: a.active_from,
            expiry_daa: a.expiry_daa,
            refund_tip: a.refund_tip,
        },
    }
}

/// Extension commitment recorded in the state.
///
/// Every KAS-book kind names it. An ask and a conditional ask name the one of its custody, a
/// sell-first entry names it in its committed exit, and a bid-side kind names the one it delivers.
pub fn extension_of(s: &AnyState) -> Option<[u8; 32]> {
    match s {
        AnyState::KobAsk(a) | AnyState::KobAskKron(a) => Some(a.extension_commitment),
        AnyState::KobCondAsk(a) | AnyState::KobCondAskKron(a) => Some(a.extension_commitment),
        AnyState::KobIfdAsk(a) | AnyState::KobIfdAskKron(a) => a.exit().ok().map(|x| x.extension_commitment),
        AnyState::KobBid(a) | AnyState::KobBidKron(a) => Some(a.extension_commitment),
        AnyState::KobCondBid(a) | AnyState::KobCondBidKron(a) => Some(a.extension_commitment),
        AnyState::KobIfdBid(a) | AnyState::KobIfdBidKron(a) => Some(a.extension_commitment),
        _ => None,
    }
}

/// The limit price of a pair order in token B (B base units per whole A).
///
/// It is a `KobPair`'s `price` (the start price when decaying) or a `KobIfdPair`'s `price`. It is
/// `None` for a `KobCondPair` (leg prices) and the KAS kinds.
pub fn pair_price(s: &AnyState) -> Option<i64> {
    match s {
        AnyState::KobPair(p) => Some(p.price),
        AnyState::KobIfdPair(p) => Some(p.price),
        _ => None,
    }
}

/// True for the pair kinds (`KobPair`, `KobCondPair`, `KobIfdPair`).
pub fn is_pair(id: TemplateId) -> bool {
    id.is_pair()
}

/// Covenant id of the parent entry of a booked repeat exit.
pub fn parent_of(s: &AnyState) -> Option<[u8; 32]> {
    match s {
        AnyState::KobCondAsk(a) | AnyState::KobCondAskKron(a) if a.parent != [0; 32] => Some(a.parent),
        AnyState::KobCondBid(a) | AnyState::KobCondBidKron(a) if a.parent != [0; 32] => Some(a.parent),
        AnyState::KobCondPair(a) if a.parent != [0; 32] => Some(a.parent),
        _ => None,
    }
}

/// Idle lifetime bound of an order (90 days at 10 DAA/s).
pub use kob_protocol::state::MAX_IDLE;

#[cfg(test)]
mod tests {
    use super::*;
    use kob_protocol::build::merge_arg;

    #[test]
    fn nb_decoding() {
        assert_eq!(nb(&0i64.to_le_bytes()), Some(Nb::Zero));
        assert_eq!(nb(&7i64.to_le_bytes()), Some(Nb::Fill(7)));
        assert_eq!(nb(&merge_arg(3, 5).unwrap()), Some(Nb::Merge { k: 3, m: 5 }));
        assert_eq!(nb(&[1, 2, 3]), None);
    }

    #[test]
    fn every_entry_of_every_order_template_resolves() {
        for id in ORDER_KINDS {
            let t = template(id);
            for (name, tag) in t.entries() {
                let tag = kob_protocol::json::from_hex(&tag).unwrap();
                assert_eq!(entry_name(id, tag.try_into().unwrap()), Some(name.as_str()), "{}", id.name());
            }
        }
        let ids: Vec<_> = TemplateId::ALL.into_iter().filter(|t| t.is_token() && t.is_artifact()).collect();
        assert!(!ids.is_empty());
        for id in ids {
            let t = template(id);
            assert!(t.entries().contains_key("transfer") && t.entries().contains_key("transfer_delegator"), "{}", id.name());
        }
    }

    #[test]
    fn mutable_fields_are_the_spliced_windows() {
        // A template's compiled state. An if-done entry's committed exit placeholder (its last
        // field, a PUSHDATA2) is replaced by its exit kind's compiled state prefix at the entry's
        // scales, because the entries decode their committed exit.
        fn compiled(id: TemplateId) -> AnyState {
            let t = template(id);
            let mut b = t.contract().compiled.bytecode[1..1 + t.state_len].to_vec();
            let Some(xid) = exit_template(id) else { return AnyState::decode(id, &b).expect("compiled state") };
            let n = b.len();
            let j = (0..n.saturating_sub(3))
                .find(|&j| b[j] == 0x4d && u16::from_le_bytes([b[j + 1], b[j + 2]]) as usize == n - j - 3)
                .expect("the committed exit is the last field");
            let x = compiled(xid);
            let scales: Vec<i64> = (0..10).map(|k| 10i64.pow(k)).collect();
            for &sa in &scales {
                for &sb in &scales {
                    let mut y = x.clone();
                    match &mut y {
                        AnyState::KobCondAsk(c) | AnyState::KobCondAskKron(c) => c.scale = sa,
                        AnyState::KobCondBid(c) | AnyState::KobCondBidKron(c) => c.scale = sa,
                        AnyState::KobCondPair(c) => (c.s_scale, c.t_scale) = (sa, sb),
                        _ => unreachable!("an exit kind"),
                    }
                    let e = y.encode();
                    b[j + 3..].copy_from_slice(&e[..n - j - 3]);
                    if let Ok(s) = AnyState::decode(id, &b) {
                        return s;
                    }
                }
            }
            panic!("{}: no committed exit decodes", id.name())
        }
        for id in ORDER_KINDS {
            let s = compiled(id);
            let m = mutable_of(&s);
            let names: Vec<&str> = kob_protocol::state::mutable_windows(id).iter().map(|w| w.0).collect();
            assert_eq!(m.amount.is_some(), names.contains(&"amountLeft"), "{}", id.name());
            assert_eq!(m.armed.is_some(), names.contains(&"armed"), "{}", id.name());
            assert_eq!(m.stop.is_some(), names.contains(&"stopPrice"), "{}", id.name());
            assert_eq!(m.rpt.is_some(), names.contains(&"rptAmount"), "{}", id.name());
            assert_eq!(with_mutable(&s, m), s);
        }
    }

    /// With `deploy-tn10` or `deploy-mainnet` the build only records its network, and no template
    /// depends on a genesis. The indexer identifies exactly the templates the deployment record
    /// lists, by redeem script and by hash.
    #[cfg(any(feature = "deploy-tn10", feature = "deploy-mainnet"))]
    #[test]
    fn deployment_templates_are_the_ones_identified() {
        use kob_protocol::artifacts::{deployment, deployment_network, template_by_hash};
        assert_eq!(deployment_network(), Some(deployment::NETWORK));
        let m: serde_json::Value = serde_json::from_str(deployment::MANIFEST).unwrap();
        let listed = m["templates"].as_array().unwrap();
        assert!(!listed.is_empty());
        for e in listed {
            let id = TemplateId::from_name(e["name"].as_str().unwrap()).unwrap();
            let t = template(id);
            assert_eq!(e["hash"].as_str().unwrap(), t.hash_hex(), "{}", id.name());
            assert_eq!(template_by_hash(&t.hash).map(|t| t.id), Some(id));
            let state = vec![0u8; t.state_len];
            assert_eq!(identify_redeem(&t.redeem(&state)).map(|(i, _)| i), Some(id), "{}", id.name());
        }
    }
}
