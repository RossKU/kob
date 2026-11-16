//! Bid delivery modes: what the indexer reads of a delivering order's fills, its bought-custody
//! and its claim (`docs/design/bid-delivery-spec.md`).
//!
//! A delivering order (`KobBid`, `KobCondBid`, `KobPair`, `KobCondPair` and the KRON twins) hands
//! the tokens it buys to its maker in one of three modes, fixed at placement:
//!
//! - mode 0 (`wallet`): every fill delivers a new plain token UTXO to the maker;
//! - mode 1 (`order`): the order keeps what it buys in its **bought-custody**, a token UTXO of the
//!   bought token owned by the order's covenant id holding exactly `bought` base units. The maker
//!   gets one plain UTXO when the order ends, is refunded, is cancelled or is claimed;
//! - mode 2 (`topup`): a fill may add the tokens to the maker's receive UTXO (form 2) and keep the
//!   order's `topupTip`, or deliver a new plain (form 0) or receive (form 1) UTXO.
//!
//! An order placed before the delivery block (the legacy state layout) reads as mode 0.
//!
//! The bought-custody is stored as a `token_utxos` row of role [`ROLE_BOUGHT`], never as a stray.

use super::record::{HeldOut, Reveal};
use crate::hex::Hash32;
use kob_protocol::artifacts::{token_template, TemplateId};
use kob_protocol::family::Family;
use kob_protocol::state::{AnyState, Delivery, TokenState, MODE_ACCUMULATE, MODE_NEW, MODE_TOPUP};

/// The `token_utxos` / `token_holdings` role of a mode-1 order's bought-custody.
pub const ROLE_BOUGHT: &str = "bought";

/// The API name of a delivery mode: `wallet` (0), `order` (1), `topup` (2).
pub fn mode_name(mode: i64) -> Option<&'static str> {
    match mode {
        MODE_NEW => Some("wallet"),
        MODE_ACCUMULATE => Some("order"),
        MODE_TOPUP => Some("topup"),
        _ => None,
    }
}

/// The delivery block that describes an order: its own (a delivering order), else the one an
/// if-done entry writes into its exits (with `bought = 0`).
pub fn block_of(s: &AnyState) -> Option<Delivery> {
    s.delivery().or_else(|| s.exit_delivery())
}

/// The token a delivering order buys.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BoughtToken {
    /// Covenant id of the token.
    pub token: [u8; 32],
    /// Template hash of its pinned program.
    pub tpl_hash: [u8; 32],
    /// Its family, when the state's code is valid.
    pub family: Option<Family>,
    /// The extension commitment its deliveries carry (all zero for KRON).
    pub ext: [u8; 32],
}

/// The token `s` buys and delivers to its maker. `None` for the kinds that deliver nothing.
pub fn bought_token(s: &AnyState) -> Option<BoughtToken> {
    let plain = |ext: [u8; 32]| BoughtToken {
        token: s.token_cov_id(),
        tpl_hash: s.token_tpl_hash().unwrap_or([0; 32]),
        family: Some(s.family()),
        ext: if s.family() == Family::Kron { [0; 32] } else { ext },
    };
    let pair = |t: kob_protocol::state::PairToken| BoughtToken {
        token: t.cov_id,
        tpl_hash: t.tpl_hash,
        family: t.family_of(),
        ext: if t.family_of() == Some(Family::Kcc20) { t.ext.unwrap_or([0; 32]) } else { [0; 32] },
    };
    match s {
        AnyState::KobBid(b) | AnyState::KobBidKron(b) => Some(plain(b.extension_commitment)),
        AnyState::KobCondBid(c) | AnyState::KobCondBidKron(c) => Some(plain(c.extension_commitment)),
        AnyState::KobPair(p) => Some(pair(p.t_token())),
        AnyState::KobCondPair(p) => Some(pair(p.t_token())),
        _ => None,
    }
}

/// Argument index of the time `t` of a fill, where it is not the call's last argument.
///
/// `KobBid.fill(nb, tokIn, t, form)` and `KobCondBid.settle(nb, tokIn, leg, ev, t, form)` end with
/// `form`. The KRON twins have the same positions, without `form`.
pub fn time_arg(template: TemplateId) -> Option<usize> {
    match template.canonical() {
        TemplateId::KobBid | TemplateId::KobBidKron => Some(2),
        TemplateId::KobCondBid | TemplateId::KobCondBidKron => Some(4),
        _ => None,
    }
}

/// Argument indices `(tokIn, form)` of a delivering fill (bid-delivery-spec §3).
///
/// `None` for a kind without `form`: the KRON twins, which deliver in mode 0.
fn fill_args(template: TemplateId) -> Option<(usize, usize)> {
    match template {
        TemplateId::KobBid => Some((1, 3)),
        TemplateId::KobCondBid => Some((1, 5)),
        TemplateId::KobPair => Some((2, 6)),
        TemplateId::KobCondPair => Some((2, 13)),
        _ => None,
    }
}

/// How one fill delivered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Form {
    /// A new plain token UTXO for the maker (mode 0, mode 2 form 0, and a mode-1 order that ends).
    New,
    /// Into the order's bought-custody (mode 1).
    Custody,
    /// A new receive UTXO for the maker (mode 2 form 1).
    Receive,
    /// A top-up of the maker's receive UTXO (mode 2 form 2).
    Topup,
}

impl Form {
    /// The `detail.delivery` of a fill event.
    pub fn name(self) -> &'static str {
        match self {
            Form::New => "new",
            Form::Custody => "custody",
            Form::Receive => "receive",
            Form::Topup => "topup",
        }
    }
}

/// What a delivering order's fill did with the bought tokens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FillDelivery {
    /// The order's delivery block before the fill.
    pub block: Delivery,
    /// How the tokens went (a mode-1 order's ending fill is [`Form::Custody`]: its release).
    pub form: Form,
    /// The token input the delivery is built from, when it takes one: the bought-custody (mode 1,
    /// `bought > 0`) or the receive UTXO (a top-up).
    pub tok_in: Option<usize>,
    /// The order's carrier per delivery (`deliveryCarrier`).
    pub carrier: i64,
}

impl FillDelivery {
    /// The KAS the fill releases from the escrow beyond the price (bid-delivery-spec §4,
    /// `released`): the delivery carrier, except for a mode-1 fill into an existing custody (0) and
    /// a top-up (`topupTip`).
    pub fn released(&self) -> i64 {
        match (self.block.mode, self.form) {
            (MODE_ACCUMULATE, _) if self.block.bought > 0 => 0,
            (MODE_TOPUP, Form::Topup) => self.block.topup_tip,
            _ => self.carrier,
        }
    }
}

/// The delivery carrier of a delivering order.
pub fn carrier_of(s: &AnyState) -> Option<i64> {
    match s {
        AnyState::KobBid(b) | AnyState::KobBidKron(b) => Some(b.delivery_carrier),
        AnyState::KobCondBid(c) | AnyState::KobCondBidKron(c) => Some(c.delivery_carrier),
        AnyState::KobPair(p) => Some(p.delivery_carrier),
        AnyState::KobCondPair(p) => Some(p.delivery_carrier),
        _ => None,
    }
}

/// How the fill of `before` revealed by `r` delivers. `None` for a kind that delivers nothing to
/// its maker, or a mode-2 fill whose `form` the covenant refuses.
pub fn fill_delivery(before: &AnyState, r: &Reveal) -> Option<FillDelivery> {
    let block = before.delivery()?;
    let carrier = carrier_of(before)?;
    let args = fill_args(r.template);
    let tok_in = |want: bool| if want { args.and_then(|(k, _)| r.int(k)).and_then(|v| usize::try_from(v).ok()) } else { None };
    let (form, tok_in) = match block.mode {
        MODE_ACCUMULATE => (Form::Custody, tok_in(block.bought > 0)),
        MODE_TOPUP => match args.and_then(|(_, f)| r.int(f))? {
            0 => (Form::New, None),
            1 => (Form::Receive, None),
            2 => (Form::Topup, tok_in(true)),
            _ => return None,
        },
        _ => (Form::New, None),
    };
    Some(FillDelivery { block, form, tok_in, carrier })
}

/// Whether a token output owned by order `cov` is exactly the bought-custody that the order's
/// state after the transaction requires (bid-delivery-spec §1, §5).
///
/// It is the order's own bought token on its pinned program, owned by the order id (scheme `0x04`),
/// with borrowing disabled, the order's extension commitment, and exactly `bought` base units. A
/// look-alike of any other amount, token, program or commitment is a stray.
pub fn is_bought_custody(after: &AnyState, cov: &Hash32, token: &Hash32, amount: i64, held: Option<&HeldOut>) -> bool {
    let (Some(bought), Some(want)) = (after.bought_custody(), bought_token(after)) else { return false };
    let Some(held) = held else { return false };
    let s: &TokenState = &held.state;
    amount == bought
        && s.amount() == bought
        && token.0 == want.token
        && token_template(held.program).hash == want.tpl_hash
        && s.is_covenant_owned()
        && s.is_plain()
        && s.owner() == cov.0
        && Some(s.family()) == want.family
        && s.extension() == want.ext
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::{bid, cond_bid, MAKER_A};
    use kob_protocol::state::BidState;

    fn reveal(template: TemplateId, args: &[i64]) -> Reveal {
        Reveal { template, state: vec![], tag: [0; 4], args: args.iter().map(|a| a.to_le_bytes().to_vec()).collect() }
    }

    #[test]
    fn modes_and_released_kas() {
        let b = bid(MAKER_A, 250_000_000);
        let c = b.delivery_carrier;
        let at = |s: BidState, args: &[i64]| fill_delivery(&AnyState::KobBid(s), &reveal(TemplateId::KobBid, args));
        // mode 0: a new UTXO and one carrier
        let d = at(b.clone(), &[1, 0, 0, 0]).unwrap();
        assert_eq!((d.form, d.released(), d.tok_in), (Form::New, c, None));
        // mode 1, first fill: one carrier into the new custody; later: none, from the custody input
        let m1 = BidState { mode: 1, ..b.clone() };
        assert_eq!(at(m1.clone(), &[1, 0, 0, 0]).map(|d| (d.form, d.released(), d.tok_in)), Some((Form::Custody, c, None)));
        let m1b = BidState { bought: 5, ..m1 };
        assert_eq!(at(m1b, &[1, 3, 0, 0]).map(|d| (d.released(), d.tok_in)), Some((0, Some(3))));
        // mode 2: form 2 keeps topupTip and names the receive UTXO; forms 0 and 1 a carrier; others refused
        let m2 = BidState { mode: 2, recv_threshold: 7, topup_tip: 1234, ..b };
        assert_eq!(at(m2.clone(), &[1, 2, 0, 2]).map(|d| (d.form, d.released(), d.tok_in)), Some((Form::Topup, 1234, Some(2))));
        assert_eq!(at(m2.clone(), &[1, 2, 0, 1]).map(|d| (d.form, d.released())), Some((Form::Receive, c)));
        assert_eq!(at(m2.clone(), &[1, 2, 0, 0]).map(|d| d.form), Some(Form::New));
        assert_eq!(at(m2, &[1, 2, 0, 3]), None);
        // the time argument is not the last one any more
        assert_eq!(time_arg(TemplateId::KobBid), Some(2));
        assert_eq!(time_arg(TemplateId::KobCondBid), Some(4));
        assert_eq!(bought_token(&AnyState::KobCondBid(cond_bid(MAKER_A))).map(|t| t.token), Some(cond_bid(MAKER_A).token_cov_id));
        assert_eq!(mode_name(1), Some("order"));
    }
}
