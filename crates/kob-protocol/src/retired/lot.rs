//! The lot state layouts of the retired templates (protocol v2.3 to v2.6): every template retired so far is a lot
//! template. An order was `lotsLeft` lots of `lotUnits x unit` base units at a price in sompi per `unit`; the custody of
//! a token-holding order held exactly `lotsLeft x lotUnits x unit` base units.
//!
//! These types only decode and re-encode the state spans of retired orders (spend-only support: the maker's cancel,
//! [`crate::build::build_cancel_retired`]) and read the payload records of versions 2 and 3, which describe them. JSON
//! keys are the v2.6 contract field names (the receipt era's `minRcptUnits` reads as `minTouchUnits`, the cross
//! limits before the pair-market auction read with `bLotEnd = bLot`, `auctionDaa = 0`; see [`crate::retired`]).

use serde::{Deserialize, Serialize};

use crate::family::Family;
use crate::state::fields_struct;

fields_struct!(
    /// `KobAsk` (limit sell incl. IOC/FOK, timed activation, TWAP, Dutch decay and market auctions).
    AskState, "KobAsk (v2.6 lot layout)", {
        /// Payout, token refund and cancel key (x-only).
        maker: [u8; 32] => "maker",
        token_cov_id: [u8; 32] => "tokenCovId",
        token_tpl_hash: [u8; 32] => "tokenTplHash",
        tpl_prefix_len: i64 => "tplPrefixLen",
        tpl_suffix_len: i64 => "tplSuffixLen",
        /// Token base units per price unit.
        unit: i64 => "unit",
        /// Price units per lot.
        lot_units: i64 => "lotUnits",
        /// Quote, sompi per price unit (start price when decaying).
        price: i64 => "price",
        /// Optional priority tip, sompi per lot.
        tip_lot: i64 => "tipLot",
        /// 0 GTC/GTD, 1 IOC, 2 FOK.
        tif: i64 => "tif",
        active_from: i64 => "activeFrom",
        expiry_daa: i64 => "expiryDaa",
        refund_tip: i64 => "refundTip",
        interval: i64 => "interval",
        max_lots: i64 => "maxLots",
        slope: i64 => "slope",
        price_end: i64 => "priceEnd",
        /// DAA per decay step (> 0 when decaying; 1 for a market auction).
        decay_step: i64 => "decayStep",
        /// Lots in custody (mutable): the custody holds exactly `lotsLeft × lotUnits × unit`.
        lots_left: i64 => "lotsLeft",
    }
);

fields_struct!(
    /// `KobBid` (limit buy incl. IOC/FOK, DCA, rising bids and market auctions).
    BidState, "KobBid (v2.6 lot layout)", {
        maker: [u8; 32] => "maker",
        token_cov_id: [u8; 32] => "tokenCovId",
        token_tpl_hash: [u8; 32] => "tokenTplHash",
        tpl_prefix_len: i64 => "tplPrefixLen",
        tpl_suffix_len: i64 => "tplSuffixLen",
        extension_commitment: [u8; 32] => "extensionCommitment",
        unit: i64 => "unit",
        lot_units: i64 => "lotUnits",
        price: i64 => "price",
        tip_lot: i64 => "tipLot",
        tif: i64 => "tif",
        active_from: i64 => "activeFrom",
        expiry_daa: i64 => "expiryDaa",
        refund_tip: i64 => "refundTip",
        /// KAS never spent on fills.
        reserve: i64 => "reserve",
        /// KAS placed on each delivered token UTXO.
        delivery_carrier: i64 => "deliveryCarrier",
        interval: i64 => "interval",
        max_lots: i64 => "maxLots",
        slope: i64 => "slope",
        price_end: i64 => "priceEnd",
        /// DAA per rise step (> 0 when rising).
        decay_step: i64 => "decayStep",
    }
);

fields_struct!(
    /// `KobCondAsk`: stop, stop-limit, stop-market, trailing, take-profit and OCO sell; every
    /// buy-first if-done exit (with the repeat fields written by its entry).
    CondAskState, "KobCondAsk (v2.6 lot layout)", {
        maker: [u8; 32] => "maker",
        token_cov_id: [u8; 32] => "tokenCovId",
        token_tpl_hash: [u8; 32] => "tokenTplHash",
        tpl_prefix_len: i64 => "tplPrefixLen",
        tpl_suffix_len: i64 => "tplSuffixLen",
        unit: i64 => "unit",
        lot_units: i64 => "lotUnits",
        tip_lot: i64 => "tipLot",
        active_from: i64 => "activeFrom",
        expiry_daa: i64 => "expiryDaa",
        refund_tip: i64 => "refundTip",
        tp_price: i64 => "tpPrice",
        /// Mutable (trailing).
        stop_price: i64 => "stopPrice",
        slip_bps: i64 => "slipBps",
        trail_step: i64 => "trailStep",
        trail_gap: i64 => "trailGap",
        trail_wait: i64 => "trailWait",
        /// Smallest trigger evidence (units of a plain order filled in the same transaction), user-set.
        min_touch_units: i64 => "minTouchUnits",
        min_rest_daa: i64 => "minRestDaa",
        /// Mutable: 0 not armed, 1 armed (auction origin = this UTXO's DAA), >= 2 the origin.
        armed: i64 => "armed",
        /// Stop-leg auction length in DAA (0 = the whole band at once).
        band_daa: i64 => "bandDaa",
        /// Most an arming / trailing keeper takes from the carrier.
        keeper_tip: i64 => "keeperTip",
        /// Mutable: the custody holds exactly this many lots.
        lots_left: i64 => "lotsLeft",
        /// Repeat IFD: covenant id of the entry this exit re-arms (0 = none).
        parent: [u8; 32] => "parent",
        /// Repeat IFD: sompi per take-profit lot returned to the entry.
        rpt_lot: i64 => "rptLot",
        /// Repeat IFD: from this DAA a take-profit may skip the entry.
        rpt_until: i64 => "rptUntil",
    }
);

fields_struct!(
    /// `KobCondBid`: buy-stop, stop-limit, trailing, limit leg and OCO buy; every sell-first
    /// if-done exit (with the repeat fields written by its entry).
    CondBidState, "KobCondBid (v2.6 lot layout)", {
        maker: [u8; 32] => "maker",
        token_cov_id: [u8; 32] => "tokenCovId",
        token_tpl_hash: [u8; 32] => "tokenTplHash",
        tpl_prefix_len: i64 => "tplPrefixLen",
        tpl_suffix_len: i64 => "tplSuffixLen",
        extension_commitment: [u8; 32] => "extensionCommitment",
        unit: i64 => "unit",
        lot_units: i64 => "lotUnits",
        tip_lot: i64 => "tipLot",
        active_from: i64 => "activeFrom",
        expiry_daa: i64 => "expiryDaa",
        refund_tip: i64 => "refundTip",
        delivery_carrier: i64 => "deliveryCarrier",
        tp_price: i64 => "tpPrice",
        stop_price: i64 => "stopPrice",
        slip_bps: i64 => "slipBps",
        trail_step: i64 => "trailStep",
        trail_gap: i64 => "trailGap",
        trail_wait: i64 => "trailWait",
        /// Smallest trigger evidence (units of a plain order filled in the same transaction), user-set.
        min_touch_units: i64 => "minTouchUnits",
        min_rest_daa: i64 => "minRestDaa",
        lots_left: i64 => "lotsLeft",
        armed: i64 => "armed",
        band_daa: i64 => "bandDaa",
        keeper_tip: i64 => "keeperTip",
        parent: [u8; 32] => "parent",
        /// Repeat IFD: the entry's all-in proceeds per lot.
        rpt_lot: i64 => "rptLot",
        /// Repeat IFD: the entry's prefund per lot.
        rpt_pre: i64 => "rptPre",
        rpt_until: i64 => "rptUntil",
    }
);

fields_struct!(
    /// `KobIfdBid`: buy-first IFD / IFO entry (limit or stop entry, optionally repeating); every
    /// fill creates a fresh committed `KobCondAsk` exit.
    IfdBidState, "KobIfdBid (v2.6 lot layout)", {
        maker: [u8; 32] => "maker",
        token_cov_id: [u8; 32] => "tokenCovId",
        token_tpl_hash: [u8; 32] => "tokenTplHash",
        tpl_prefix_len: i64 => "tplPrefixLen",
        tpl_suffix_len: i64 => "tplSuffixLen",
        extension_commitment: [u8; 32] => "extensionCommitment",
        unit: i64 => "unit",
        lot_units: i64 => "lotUnits",
        lots_left: i64 => "lotsLeft",
        price: i64 => "price",
        tip_lot: i64 => "tipLot",
        active_from: i64 => "activeFrom",
        expiry_daa: i64 => "expiryDaa",
        refund_tip: i64 => "refundTip",
        delivery_carrier: i64 => "deliveryCarrier",
        exit_carrier: i64 => "exitCarrier",
        /// Smallest fill except the one that takes the rest.
        min_lots: i64 => "minLots",
        /// 0 = limit entry; else the buy-stop trigger price.
        entry_stop: i64 => "entryStop",
        band_daa: i64 => "bandDaa",
        /// Smallest trigger evidence (units of a plain order filled in the same transaction), user-set.
        min_touch_units: i64 => "minTouchUnits",
        min_rest_daa: i64 => "minRestDaa",
        keeper_tip: i64 => "keeperTip",
        armed: i64 => "armed",
        /// Repeat: 0 = off, else 1 + lot re-arms left.
        rpt_lots: i64 => "rptLots",
        /// The committed exit: the first 279 bytes of its `KobCondAsk` state (up to `lotsLeft`,
        /// which each fill replaces; the repeat fields are written by the entry).
        exit_state: Vec<u8> => "exitState",
    }
);

fields_struct!(
    /// `KobIfdAsk`: sell-first IFD / IFO entry (limit or stop entry, optionally repeating); every
    /// fill creates a fresh `KobCondBid` exit with `lotsLeft = n`.
    IfdAskState, "KobIfdAsk (v2.6 lot layout)", {
        maker: [u8; 32] => "maker",
        token_cov_id: [u8; 32] => "tokenCovId",
        token_tpl_hash: [u8; 32] => "tokenTplHash",
        tpl_prefix_len: i64 => "tplPrefixLen",
        tpl_suffix_len: i64 => "tplSuffixLen",
        unit: i64 => "unit",
        lot_units: i64 => "lotUnits",
        price: i64 => "price",
        tip_lot: i64 => "tipLot",
        active_from: i64 => "activeFrom",
        expiry_daa: i64 => "expiryDaa",
        refund_tip: i64 => "refundTip",
        prefund_lot: i64 => "prefundLot",
        exit_carrier: i64 => "exitCarrier",
        min_lots: i64 => "minLots",
        /// 0 = limit entry; else the sell-stop trigger price.
        entry_stop: i64 => "entryStop",
        band_daa: i64 => "bandDaa",
        /// Smallest trigger evidence (units of a plain order filled in the same transaction), user-set.
        min_touch_units: i64 => "minTouchUnits",
        min_rest_daa: i64 => "minRestDaa",
        keeper_tip: i64 => "keeperTip",
        armed: i64 => "armed",
        /// Mutable: the custody holds exactly this many lots (0 = a repeating entry waiting).
        lots_left: i64 => "lotsLeft",
        rpt_lots: i64 => "rptLots",
        /// The committed exit: the first 321 bytes of its `KobCondBid` state (its `lotsLeft` is
        /// replaced by each fill's n; the repeat fields are written by the entry).
        exit_state: Vec<u8> => "exitState",
    }
);

fields_struct!(
    /// `KobCross` (`KobCrossKron`): cross limit selling token A for token B at a guaranteed all-in rate,
    /// filled only through the two KAS books (A -> KAS -> B) in one transaction. The custody of A is of
    /// the template's family; B (`bFamily`) may be of either family.
    CrossState, "KobCross (v2.6 lot layout)", {
        /// B deliveries, A refunds and returns, cancel key (x-only).
        maker: [u8; 32] => "maker",
        /// Token A (sold, escrowed).
        token_cov_id: [u8; 32] => "tokenCovId",
        token_tpl_hash: [u8; 32] => "tokenTplHash",
        tpl_prefix_len: i64 => "tplPrefixLen",
        tpl_suffix_len: i64 => "tplSuffixLen",
        /// Token A base units per unit.
        unit: i64 => "unit",
        /// Units per lot (one lot = `lotUnits × unit` of A).
        lot_units: i64 => "lotUnits",
        /// Family code of token B: 1 KCC-20, 2 KRON.
        b_family: i64 => "bFamily",
        /// Token B (bought).
        b_cov_id: [u8; 32] => "bCovId",
        b_tpl_hash: [u8; 32] => "bTplHash",
        b_prefix_len: i64 => "bPrefixLen",
        b_suffix_len: i64 => "bSuffixLen",
        /// KCC-20 extension commitment of the maker's B deliveries (zero for a KRON B).
        b_ext: [u8; 32] => "bExt",
        /// The rate: B base units per lot of A the maker receives at least (an auction's start).
        b_lot: i64 => "bLot",
        /// Optional priority tip, sompi per lot of A, prefunded in the order UTXO and released to the filler (KAS, like
        /// every other KOB tip: it creates no token output).
        tip_lot: i64 => "tipLot",
        /// 0 GTC/GTD, 1 IOC, 2 FOK.
        tif: i64 => "tif",
        active_from: i64 => "activeFrom",
        expiry_daa: i64 => "expiryDaa",
        refund_tip: i64 => "refundTip",
        /// Sompi the order moves onto each B delivery (prefunded in the order UTXO).
        delivery_carrier: i64 => "deliveryCarrier",
        /// Auction (pair market order): the worst rate the rate relaxes to (`bLotEnd ≤ bLot`; 0 when not auctioning).
        b_lot_end: i64 => "bLotEnd",
        /// Auction length in DAA from `activeFrom` (0 = a fixed rate `bLot`).
        auction_daa: i64 => "auctionDaa",
        /// Lots in custody (mutable): the custody holds exactly `lotsLeft × lotUnits × unit` of A.
        lots_left: i64 => "lotsLeft",
    }
);

/// The state of an order of a retired (lot) template, tagged by its kind (`KobCross` for both cross limit templates;
/// the family is the retired template's, [`crate::retired::Retired::family`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "state")]
pub enum LotState {
    KobAsk(AskState),
    KobBid(BidState),
    KobCondAsk(CondAskState),
    KobCondBid(CondBidState),
    KobIfdBid(IfdBidState),
    KobIfdAsk(IfdAskState),
    KobCross(CrossState),
}

/// `lots x lotUnits x unit` (`None` on an overflow or a negative factor: such a custody cannot exist).
fn lot_amount(lots: i64, lot_units: i64, unit: i64) -> Option<i64> {
    if lots < 0 || lot_units < 0 || unit < 0 {
        return None;
    }
    lots.checked_mul(lot_units)?.checked_mul(unit)
}

impl LotState {
    /// Name of the kind (`KobAsk`, ...; the KCC-20 name in both families).
    pub fn kind_name(&self) -> &'static str {
        match self {
            LotState::KobAsk(_) => "KobAsk",
            LotState::KobBid(_) => "KobBid",
            LotState::KobCondAsk(_) => "KobCondAsk",
            LotState::KobCondBid(_) => "KobCondBid",
            LotState::KobIfdBid(_) => "KobIfdBid",
            LotState::KobIfdAsk(_) => "KobIfdAsk",
            LotState::KobCross(_) => "KobCross",
        }
    }
    /// Maker key (payouts, refunds, the cancel signature).
    pub fn maker(&self) -> [u8; 32] {
        match self {
            LotState::KobAsk(s) => s.maker,
            LotState::KobBid(s) => s.maker,
            LotState::KobCondAsk(s) => s.maker,
            LotState::KobCondBid(s) => s.maker,
            LotState::KobIfdBid(s) => s.maker,
            LotState::KobIfdAsk(s) => s.maker,
            LotState::KobCross(s) => s.maker,
        }
    }
    /// The order's token (a cross limit: token A): covenant id, program template hash, prefix and suffix lengths.
    pub fn token(&self) -> ([u8; 32], [u8; 32], i64, i64) {
        match self {
            LotState::KobAsk(s) => (s.token_cov_id, s.token_tpl_hash, s.tpl_prefix_len, s.tpl_suffix_len),
            LotState::KobBid(s) => (s.token_cov_id, s.token_tpl_hash, s.tpl_prefix_len, s.tpl_suffix_len),
            LotState::KobCondAsk(s) => (s.token_cov_id, s.token_tpl_hash, s.tpl_prefix_len, s.tpl_suffix_len),
            LotState::KobCondBid(s) => (s.token_cov_id, s.token_tpl_hash, s.tpl_prefix_len, s.tpl_suffix_len),
            LotState::KobIfdBid(s) => (s.token_cov_id, s.token_tpl_hash, s.tpl_prefix_len, s.tpl_suffix_len),
            LotState::KobIfdAsk(s) => (s.token_cov_id, s.token_tpl_hash, s.tpl_prefix_len, s.tpl_suffix_len),
            LotState::KobCross(s) => (s.token_cov_id, s.token_tpl_hash, s.tpl_prefix_len, s.tpl_suffix_len),
        }
    }
    /// True for the kinds that hold tokens in covenant-id custody.
    pub fn holds_tokens(&self) -> bool {
        matches!(self, LotState::KobAsk(_) | LotState::KobCondAsk(_) | LotState::KobIfdAsk(_) | LotState::KobCross(_))
    }
    /// The exact custody amount of a token-holding order, `lotsLeft x lotUnits x unit` (`None` for the other kinds and
    /// for a product that does not fit, which no custody can hold).
    pub fn custody_amount(&self) -> Option<i64> {
        match self {
            LotState::KobAsk(s) => lot_amount(s.lots_left, s.lot_units, s.unit),
            LotState::KobCondAsk(s) => lot_amount(s.lots_left, s.lot_units, s.unit),
            LotState::KobIfdAsk(s) => lot_amount(s.lots_left, s.lot_units, s.unit),
            LotState::KobCross(s) => lot_amount(s.lots_left, s.lot_units, s.unit),
            _ => None,
        }
    }
    /// The cross limit's state (its token B side: `bFamily`, `bCovId`, `bTplHash`, lengths, `bExt`).
    pub fn as_cross(&self) -> Option<&CrossState> {
        match self {
            LotState::KobCross(s) => Some(s),
            _ => None,
        }
    }
    /// A token-B family code of a cross limit (`1` KCC-20, `2` KRON).
    pub fn b_family(&self) -> Option<Family> {
        self.as_cross().and_then(|x| crate::state::family_of_code(x.b_family))
    }
}
