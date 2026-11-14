//! Typed order and token states, and the order arithmetic.
//!
//! Each typed state mirrors the runtime state of its contract field by field, and its JSON keys
//! are the contract's field names. Encoding goes through the ABI description of the artifact
//! (`silverscript_abi::encode_runtime_state_script`), so the bytes are what the compiler places
//! in the state span.
//!
//! # Amounts and prices
//!
//! An order holds an `amount` of its base token, in base units, at a limit `price` per whole
//! base token. The price is in quote units per `scale` base units, where `scale` is
//! `10^decimals` of the base token. Plain orders quote KAS in sompi. A pair order A/B quotes B,
//! see [`pair`]. Tips are always in sompi per whole base token.
//!
//! [`quote_of`] values `n` base units at rate `r` as `n * r / scale`, rounded in the maker's
//! favour:
//! - [`Round::Up`] for what a maker receives, e.g. ask proceeds or the budget a bid consumes.
//! - [`Round::Down`] for what a maker pays, e.g. bid spend or the KAS tips.
//!
//! The helpers of each kind (`proceeds`, `used`, `spend`, `b_out_min`) are the covenant
//! formulas. They return `None` where the covenant would fail: on an overflow, or on a refused
//! input such as a price below its tip.
//!
//! Splitting an order into fills cannot farm the rounding. Each fill moves at most one quote
//! unit, and always to the maker. The proof is in `docs/spec/order-types.md`.
//!
//! # Minimum fill
//!
//! A fill is at least `minFill` base units unless it takes everything left. A `KobBid` also
//! ends on a smaller fill when less than one minimum fill of buying power is left. `fill_ok`
//! of each kind is the covenant's rule.
//!
//! # Numeric gate
//!
//! [`check_scale`] and [`check_quote`] refuse an order that no covenant computation could fill
//! completely. That is a scale that is not a power of ten in `1..=10^9`, or a full fill worth
//! `2^62` quote units or more. [`AnyState::check_numbers`] and the builders apply both.

use std::collections::BTreeMap;

use kaspa_consensus_core::tx::ScriptPublicKey;
use serde::{Deserialize, Serialize};
use silverscript_abi::{decode_runtime_state_script, encode_runtime_state_script, ArtifactValue};

use crate::artifacts::{template, Template, TemplateId, TokenTemplate, KRON_STATE_LEN};
use crate::family::{Family, KRON_TYPE_ADDR, KRON_TYPE_COVID};

/// Errors raised by the state codec.
#[derive(Debug, thiserror::Error)]
pub enum StateError {
    #[error("{0}: missing field `{1}`")]
    Missing(&'static str, String),
    #[error("{0}: field `{1}` has the wrong type ({2})")]
    Type(&'static str, String, String),
    #[error("{0}: codec: {1}")]
    Codec(&'static str, String),
    #[error("{0}: script is not an instance of this template")]
    NotInstance(&'static str),
}

/// Conversion of a Rust field to and from the ABI value model.
pub trait FieldValue: Sized {
    fn to_value(&self) -> ArtifactValue;
    fn from_value(v: &ArtifactValue) -> Result<Self, String>;
}

impl FieldValue for i64 {
    fn to_value(&self) -> ArtifactValue {
        ArtifactValue::Int(*self)
    }
    fn from_value(v: &ArtifactValue) -> Result<Self, String> {
        match v {
            ArtifactValue::Int(i) => Ok(*i),
            other => Err(format!("expected int, got {other:?}")),
        }
    }
}

impl FieldValue for u8 {
    fn to_value(&self) -> ArtifactValue {
        ArtifactValue::Byte(*self)
    }
    fn from_value(v: &ArtifactValue) -> Result<Self, String> {
        match v {
            ArtifactValue::Byte(b) => Ok(*b),
            other => Err(format!("expected byte, got {other:?}")),
        }
    }
}

impl FieldValue for [u8; 32] {
    fn to_value(&self) -> ArtifactValue {
        ArtifactValue::Bytes(self.to_vec())
    }
    fn from_value(v: &ArtifactValue) -> Result<Self, String> {
        match v {
            ArtifactValue::Bytes(b) => b.as_slice().try_into().map_err(|_| format!("expected 32 bytes, got {}", b.len())),
            other => Err(format!("expected bytes, got {other:?}")),
        }
    }
}

impl FieldValue for Vec<u8> {
    fn to_value(&self) -> ArtifactValue {
        ArtifactValue::Bytes(self.clone())
    }
    fn from_value(v: &ArtifactValue) -> Result<Self, String> {
        match v {
            ArtifactValue::Bytes(b) => Ok(b.clone()),
            other => Err(format!("expected bytes, got {other:?}")),
        }
    }
}

/// Contract field of the token extension commitment.
///
/// KRON tokens have none. The KRON adapters have no such state field, and the typed states carry
/// it as all zeros.
pub(crate) const EXT_KEY: &str = "extensionCommitment";

/// A typed state as the ABI's field map (contract field name -> value).
pub trait FieldMap: Sized + Clone {
    /// Short name for errors.
    const NAME: &'static str;
    fn to_values(&self) -> BTreeMap<String, ArtifactValue>;
    fn from_values(m: &BTreeMap<String, ArtifactValue>) -> Result<Self, StateError>;
}

/// Encodes a field map as the state span of the template `t`.
///
/// With `kron` the extension commitment is dropped, and it must be zero, because no KRON template
/// has that field.
pub(crate) fn encode_fields(
    name: &'static str,
    t: &Template,
    kron: bool,
    mut v: BTreeMap<String, ArtifactValue>,
) -> Result<Vec<u8>, StateError> {
    if kron {
        if let Some(ArtifactValue::Bytes(b)) = v.remove(EXT_KEY) {
            if b.iter().any(|x| *x != 0) {
                return Err(StateError::Codec(name, "KRON tokens have no extension commitment (must be zero)".into()));
            }
        }
    }
    encode_runtime_state_script(&t.artifact, &t.contract().runtime_state, &v).map_err(|e| StateError::Codec(name, e.to_string()))
}

/// Decodes the state span of the template `t` into its field map.
///
/// With `kron` an absent extension commitment reads as zero.
pub(crate) fn decode_fields(
    name: &'static str,
    t: &Template,
    kron: bool,
    bytes: &[u8],
) -> Result<BTreeMap<String, ArtifactValue>, StateError> {
    if bytes.len() != t.state_len {
        return Err(StateError::Codec(name, format!("state span is {} bytes, expected {}", bytes.len(), t.state_len)));
    }
    let mut fields = decode_runtime_state_script(&t.artifact, &t.contract().runtime_state, bytes)
        .map_err(|e| StateError::Codec(name, e.to_string()))?;
    if kron {
        fields.entry(EXT_KEY.to_string()).or_insert_with(|| ArtifactValue::Bytes(vec![0; 32]));
    }
    Ok(fields)
}

/// Common interface of every typed state.
///
/// A typed state is the KCC-20 layout of its kind. The KRON adapters have the same fields, except
/// that `KobAsk`, `KobCondAsk` and the bid-side kinds (`KobBid`, `KobCondBid`, `KobIfdBid`) lack
/// the token extension commitment. The typed state keeps the field, as `[0; 32]` for KRON, and the
/// KRON codec drops it.
///
/// The `*_as` codecs take the template of the family, and the plain ones use the KCC-20 templates.
/// The pair kinds are one template for both families, and their state fields name the family of
/// each token.
pub trait StateCodec: FieldMap {
    /// Template whose runtime state this is (for KCC-20, the layout-defining reference program).
    const TEMPLATE: TemplateId;

    /// The encoded state span (KCC-20 template).
    fn encode(&self) -> Vec<u8> {
        self.encode_as(Self::TEMPLATE)
    }

    /// The encoded state span under `id` (the template of this kind in some family).
    fn encode_as(&self, id: TemplateId) -> Vec<u8> {
        self.try_encode_as(id).unwrap_or_else(|e| panic!("{}: state encoding failed: {e}", Self::NAME))
    }

    /// Fallible [`StateCodec::encode_as`]: a KRON state must carry no extension commitment.
    fn try_encode_as(&self, id: TemplateId) -> Result<Vec<u8>, StateError> {
        debug_assert_eq!(id.base(), Self::TEMPLATE, "{}: template {} is not this kind", Self::NAME, id.name());
        encode_fields(Self::NAME, template(id), id.family() == Family::Kron, self.to_values())
    }

    /// Decodes a state span (KCC-20 template).
    fn decode(bytes: &[u8]) -> Result<Self, StateError> {
        Self::decode_as(Self::TEMPLATE, bytes)
    }

    /// Decodes a state span of the template `id` (this kind in some family).
    fn decode_as(id: TemplateId, bytes: &[u8]) -> Result<Self, StateError> {
        debug_assert_eq!(id.base(), Self::TEMPLATE, "{}: template {} is not this kind", Self::NAME, id.name());
        let fields = decode_fields(Self::NAME, template(id), id.family() == Family::Kron, bytes)?;
        let state = Self::from_values(&fields)?;
        // Canonical encoding only (fixed-width pushes): re-encoding must reproduce the bytes.
        if state.try_encode_as(id)? != bytes {
            return Err(StateError::Codec(Self::NAME, "non-canonical state encoding".into()));
        }
        Ok(state)
    }
}

/// Order states: one template per family, and one template for both families for the pair kinds.
pub trait OrderState: StateCodec {
    /// Redeem script of this instance (KCC-20 template).
    fn redeem(&self) -> Vec<u8> {
        self.redeem_for(Family::Kcc20)
    }
    /// P2SH script public key of this instance (KCC-20 template).
    fn spk(&self) -> ScriptPublicKey {
        self.spk_for(Family::Kcc20)
    }
    /// Decodes an instance from its redeem script (KCC-20 template).
    fn from_redeem(redeem: &[u8]) -> Result<Self, StateError> {
        Self::from_redeem_for(Family::Kcc20, redeem)
    }
    /// Default build of this kind in `fam`.
    ///
    /// The build of an instance is [`OrderState::template_of`].
    fn template_for(fam: Family) -> TemplateId {
        Self::TEMPLATE.in_family(fam)
    }
    /// The build of this instance in `fam`.
    ///
    /// It is the build whose maker deliveries carry the holder owner of the token program the state
    /// pins (`tokenTplHash`). A KRON program with id_type 0 holders takes the `*KronD0` build, see
    /// [`TemplateId::for_token_hash`].
    fn template_of(&self, fam: Family) -> TemplateId {
        match self.to_values().get("tokenTplHash") {
            Some(ArtifactValue::Bytes(b)) => match <[u8; 32]>::try_from(b.as_slice()) {
                Ok(h) => Self::TEMPLATE.for_token_hash(fam, &h),
                Err(_) => Self::template_for(fam),
            },
            _ => Self::template_for(fam),
        }
    }
    /// Encoded state span under the default template of `fam`.
    ///
    /// Every build of a kind has the same state layout.
    fn encode_for(&self, fam: Family) -> Vec<u8> {
        self.encode_as(Self::template_for(fam))
    }
    /// Redeem script of this instance under its template in `fam`.
    fn redeem_for(&self, fam: Family) -> Vec<u8> {
        let id = self.template_of(fam);
        template(id).redeem(&self.encode_as(id))
    }
    /// P2SH script public key of this instance under its template in `fam`.
    fn spk_for(&self, fam: Family) -> ScriptPublicKey {
        let id = self.template_of(fam);
        template(id).spk(&self.encode_as(id))
    }
    /// Decodes an instance from its redeem script under its template in `fam`.
    ///
    /// Either build of a KRON kind is accepted, but it must be the build of the token program the
    /// state pins.
    fn from_redeem_for(fam: Family, redeem: &[u8]) -> Result<Self, StateError> {
        for id in [Self::template_for(fam), Self::template_for(fam).delivery0()] {
            if let Some(st) = template(id).state_of(redeem) {
                let state = Self::decode_as(id, st)?;
                if state.template_of(fam) != id {
                    return Err(StateError::NotInstance(Self::NAME));
                }
                return Ok(state);
            }
        }
        Err(StateError::NotInstance(Self::NAME))
    }
}

/// A typed state struct and its [`FieldMap`] (JSON keys = contract field names).
macro_rules! fields_struct {
    (
        $(#[$m:meta])*
        $name:ident, $sname:literal,
        { $( $(#[$fm:meta])* $field:ident : $ty:ty => $key:literal ),* $(,)? }
    ) => {
        $(#[$m])*
        #[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
        pub struct $name {
            $(
                $(#[$fm])*
                #[serde(rename = $key, with = "crate::json::field")]
                pub $field: $ty,
            )*
        }

        impl $crate::state::FieldMap for $name {
            const NAME: &'static str = $sname;

            fn to_values(&self) -> std::collections::BTreeMap<String, silverscript_abi::ArtifactValue> {
                let mut m = std::collections::BTreeMap::new();
                $( m.insert($key.to_string(), $crate::state::FieldValue::to_value(&self.$field)); )*
                m
            }

            fn from_values(
                m: &std::collections::BTreeMap<String, silverscript_abi::ArtifactValue>,
            ) -> Result<Self, $crate::state::StateError> {
                Ok(Self {
                    $(
                        $field: {
                            let v = m.get($key).ok_or_else(|| $crate::state::StateError::Missing($sname, $key.to_string()))?;
                            $crate::state::FieldValue::from_value(v)
                                .map_err(|e| $crate::state::StateError::Type($sname, $key.to_string(), e))?
                        },
                    )*
                })
            }
        }
    };
}

macro_rules! kob_state {
    (
        $(#[$m:meta])*
        $name:ident, $tpl:expr, $sname:literal, order: $order:tt,
        { $( $(#[$fm:meta])* $field:ident : $ty:ty => $key:literal ),* $(,)? }
    ) => {
        fields_struct!($(#[$m])* $name, $sname, { $( $(#[$fm])* $field : $ty => $key ),* });

        impl StateCodec for $name {
            const TEMPLATE: TemplateId = $tpl;
        }

        kob_state!(@order $order $name);
    };
    (@order true $name:ident) => { impl OrderState for $name {} };
    (@order false $name:ident) => {};
}

kob_state!(
    /// `KobAsk`: limit sell incl. IOC/FOK, timed activation, TWAP, Dutch decay and market auctions.
    AskState, TemplateId::KobAsk, "KobAsk", order: true, {
        /// Payout, token refund and cancel key (x-only).
        maker: [u8; 32] => "maker",
        token_cov_id: [u8; 32] => "tokenCovId",
        token_tpl_hash: [u8; 32] => "tokenTplHash",
        tpl_prefix_len: i64 => "tplPrefixLen",
        tpl_suffix_len: i64 => "tplSuffixLen",
        /// Base units per whole token, the price denominator: `10^decimals`, capped at `10^9`.
        scale: i64 => "scale",
        /// Smallest fill in base units, unless the fill takes everything left.
        min_fill: i64 => "minFill",
        /// Quote, sompi per whole token (start price when decaying).
        price: i64 => "price",
        /// Optional priority tip, sompi per whole token.
        tip: i64 => "tip",
        /// 0 GTC/GTD, 1 IOC, 2 FOK.
        tif: i64 => "tif",
        active_from: i64 => "activeFrom",
        expiry_daa: i64 => "expiryDaa",
        refund_tip: i64 => "refundTip",
        interval: i64 => "interval",
        /// TWAP: most base units per fill (0 = off).
        max_fill: i64 => "maxFill",
        slope: i64 => "slope",
        price_end: i64 => "priceEnd",
        /// DAA per decay step (> 0 when decaying; 1 for a market auction).
        decay_step: i64 => "decayStep",
        /// Base units in custody (mutable): the custody holds exactly `amountLeft`.
        amount_left: i64 => "amountLeft",
        /// KCC-20 extension commitment of the custody, zero for KRON.
        ///
        /// The order takes no custody of another commitment.
        extension_commitment: [u8; 32] => "extensionCommitment",
    }
);

kob_state!(
    /// `KobBid` (limit buy incl. IOC/FOK, DCA, rising bids and market auctions).
    ///
    /// Its quantity is its escrow.
    BidState, TemplateId::KobBid, "KobBid", order: true, {
        maker: [u8; 32] => "maker",
        token_cov_id: [u8; 32] => "tokenCovId",
        token_tpl_hash: [u8; 32] => "tokenTplHash",
        tpl_prefix_len: i64 => "tplPrefixLen",
        tpl_suffix_len: i64 => "tplSuffixLen",
        extension_commitment: [u8; 32] => "extensionCommitment",
        scale: i64 => "scale",
        /// Smallest fill in base units (> 0), unless less than one minimum fill of buying power is
        /// left.
        min_fill: i64 => "minFill",
        price: i64 => "price",
        tip: i64 => "tip",
        tif: i64 => "tif",
        active_from: i64 => "activeFrom",
        expiry_daa: i64 => "expiryDaa",
        refund_tip: i64 => "refundTip",
        /// KAS never spent on fills.
        reserve: i64 => "reserve",
        /// KAS placed on each delivered token UTXO.
        delivery_carrier: i64 => "deliveryCarrier",
        interval: i64 => "interval",
        /// DCA: most base units per fill (0 = off).
        max_fill: i64 => "maxFill",
        slope: i64 => "slope",
        price_end: i64 => "priceEnd",
        /// DAA per rise step (> 0 when rising).
        decay_step: i64 => "decayStep",
    }
);

kob_state!(
    /// `KobCondAsk`: stop, stop-limit, stop-market, trailing, take-profit and OCO sell.
    ///
    /// Every buy-first if-done exit is one too, with the repeat fields written by its entry.
    CondAskState, TemplateId::KobCondAsk, "KobCondAsk", order: true, {
        maker: [u8; 32] => "maker",
        token_cov_id: [u8; 32] => "tokenCovId",
        token_tpl_hash: [u8; 32] => "tokenTplHash",
        tpl_prefix_len: i64 => "tplPrefixLen",
        tpl_suffix_len: i64 => "tplSuffixLen",
        scale: i64 => "scale",
        min_fill: i64 => "minFill",
        tip: i64 => "tip",
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
        /// Smallest trigger evidence: base units of a plain order of the same scale filled in the
        /// same transaction.
        min_touch: i64 => "minTouch",
        min_rest_daa: i64 => "minRestDaa",
        /// Mutable: 0 not armed, 1 armed (auction origin = this UTXO's DAA), >= 2 the origin.
        armed: i64 => "armed",
        /// Stop-leg auction length in DAA (0 = the whole band at once).
        band_daa: i64 => "bandDaa",
        /// Most an arming or trailing keeper takes from the carrier.
        keeper_tip: i64 => "keeperTip",
        /// Mutable: the custody holds exactly this many base units.
        amount_left: i64 => "amountLeft",
        /// Repeat IFD: covenant id of the entry this exit re-arms (0 = none).
        parent: [u8; 32] => "parent",
        /// Repeat IFD: the entry's budget rate (sompi per whole token) returned to it, rounded up.
        rpt_price: i64 => "rptPrice",
        /// Repeat IFD: from this DAA a take-profit may skip the entry.
        rpt_until: i64 => "rptUntil",
        /// KCC-20 extension commitment of the custody, zero for KRON.
        ///
        /// In an if-done exit it is the entry's `extensionCommitment`, written by the entry.
        extension_commitment: [u8; 32] => "extensionCommitment",
    }
);

kob_state!(
    /// `KobCondBid`: buy-stop, stop-limit, trailing, limit leg and OCO buy.
    ///
    /// Every sell-first if-done exit is one too, with the repeat fields written by its entry.
    CondBidState, TemplateId::KobCondBid, "KobCondBid", order: true, {
        maker: [u8; 32] => "maker",
        token_cov_id: [u8; 32] => "tokenCovId",
        token_tpl_hash: [u8; 32] => "tokenTplHash",
        tpl_prefix_len: i64 => "tplPrefixLen",
        tpl_suffix_len: i64 => "tplSuffixLen",
        extension_commitment: [u8; 32] => "extensionCommitment",
        scale: i64 => "scale",
        min_fill: i64 => "minFill",
        tip: i64 => "tip",
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
        /// Smallest trigger evidence: base units of a plain order of the same scale filled in the
        /// same transaction.
        min_touch: i64 => "minTouch",
        min_rest_daa: i64 => "minRestDaa",
        /// Mutable: base units still to buy.
        amount_left: i64 => "amountLeft",
        armed: i64 => "armed",
        band_daa: i64 => "bandDaa",
        keeper_tip: i64 => "keeperTip",
        parent: [u8; 32] => "parent",
        /// Repeat IFD: the entry's all-in proceeds rate (`price - tip`, sompi per whole token).
        rpt_price: i64 => "rptPrice",
        /// Repeat IFD: the entry's prefund rate (sompi per whole token).
        rpt_pre: i64 => "rptPre",
        rpt_until: i64 => "rptUntil",
    }
);

kob_state!(
    /// `KobIfdBid`: buy-first IFD / IFO entry, a limit or stop entry that may repeat.
    ///
    /// Every fill creates a fresh committed `KobCondAsk` exit.
    IfdBidState, TemplateId::KobIfdBid, "KobIfdBid", order: true, {
        maker: [u8; 32] => "maker",
        token_cov_id: [u8; 32] => "tokenCovId",
        token_tpl_hash: [u8; 32] => "tokenTplHash",
        tpl_prefix_len: i64 => "tplPrefixLen",
        tpl_suffix_len: i64 => "tplSuffixLen",
        extension_commitment: [u8; 32] => "extensionCommitment",
        scale: i64 => "scale",
        /// Mutable: base units still to buy.
        amount_left: i64 => "amountLeft",
        price: i64 => "price",
        tip: i64 => "tip",
        active_from: i64 => "activeFrom",
        expiry_daa: i64 => "expiryDaa",
        refund_tip: i64 => "refundTip",
        delivery_carrier: i64 => "deliveryCarrier",
        exit_carrier: i64 => "exitCarrier",
        /// Smallest fill except the one that takes the rest.
        min_fill: i64 => "minFill",
        /// 0 = limit entry, else the buy-stop trigger price.
        entry_stop: i64 => "entryStop",
        band_daa: i64 => "bandDaa",
        /// Smallest trigger evidence: base units of a plain order of the same scale filled in the
        /// same transaction.
        min_touch: i64 => "minTouch",
        min_rest_daa: i64 => "minRestDaa",
        keeper_tip: i64 => "keeperTip",
        armed: i64 => "armed",
        /// Repeat: 0 = off, else 1 + base units of re-arms left.
        rpt_amount: i64 => "rptAmount",
        /// The committed exit: the first 279 bytes of its `KobCondAsk` state, up to `amountLeft`.
        ///
        /// Each fill replaces `amountLeft`, and the entry writes the repeat fields.
        exit_state: Vec<u8> => "exitState",
    }
);

kob_state!(
    /// `KobIfdAsk`: sell-first IFD / IFO entry, a limit or stop entry that may repeat.
    ///
    /// Every fill creates a fresh `KobCondBid` exit with `amountLeft = n`.
    IfdAskState, TemplateId::KobIfdAsk, "KobIfdAsk", order: true, {
        maker: [u8; 32] => "maker",
        token_cov_id: [u8; 32] => "tokenCovId",
        token_tpl_hash: [u8; 32] => "tokenTplHash",
        tpl_prefix_len: i64 => "tplPrefixLen",
        tpl_suffix_len: i64 => "tplSuffixLen",
        scale: i64 => "scale",
        price: i64 => "price",
        tip: i64 => "tip",
        active_from: i64 => "activeFrom",
        expiry_daa: i64 => "expiryDaa",
        refund_tip: i64 => "refundTip",
        /// The buy-back budget beyond the proceeds, sompi per whole token (rounded up per fill).
        prefund: i64 => "prefund",
        exit_carrier: i64 => "exitCarrier",
        min_fill: i64 => "minFill",
        /// 0 = limit entry, else the sell-stop trigger price.
        entry_stop: i64 => "entryStop",
        band_daa: i64 => "bandDaa",
        /// Smallest trigger evidence: base units of a plain order of the same scale filled in the
        /// same transaction.
        min_touch: i64 => "minTouch",
        min_rest_daa: i64 => "minRestDaa",
        keeper_tip: i64 => "keeperTip",
        armed: i64 => "armed",
        /// Mutable: the custody holds exactly this many base units (0 = a repeating entry waiting).
        amount_left: i64 => "amountLeft",
        rpt_amount: i64 => "rptAmount",
        /// The committed exit: the first 321 bytes of its `KobCondBid` state.
        ///
        /// Each fill replaces its `amountLeft` with the fill's n, and the entry writes the repeat
        /// fields.
        exit_state: Vec<u8> => "exitState",
    }
);

kob_state!(
    /// KCC-20 token state (same 112-byte layout in every supported program).
    Kcc20State, TemplateId::Kcc20Ref, "KCC20", order: false, {
        amount: i64 => "amount",
        /// Owner: an x-only pubkey (scheme 0x00) or a covenant id (scheme 0x04).
        owner: [u8; 32] => "owner",
        owner_scheme: u8 => "owner_scheme",
        borrow_scheme: u8 => "borrow_scheme",
        borrow_guard: [u8; 32] => "borrow_guard",
        extension_commitment: [u8; 32] => "extension_commitment",
    }
);

/// KCC-20 owner scheme: P2PK (Schnorr, x-only key).
pub const SCHEME_P2PK: u8 = 0x00;
/// KCC-20 owner scheme: covenant id (KOB order custody).
pub const SCHEME_COVID: u8 = 0x04;
/// Time in force: the order rests until it is filled, cancelled or refunded.
pub const TIF_GTC: i64 = 0;
/// Time in force: fill what crosses now and return the rest.
///
/// Anyone may refund the rest [`IOC_LIFE`] DAA after the order became fillable.
pub const TIF_IOC: i64 = 1;
/// Time in force: fill everything in one transaction, or nothing.
pub const TIF_FOK: i64 = 2;
/// Trigger-evidence side: a resting ask is filled (a sell at its quote).
pub const SIDE_ASK: i64 = 1;
/// Trigger-evidence side: a resting bid is filled.
pub const SIDE_BID: i64 = 2;
/// Idle bound after which any order is refundable (90 days at 10 DAA/s).
pub const MAX_IDLE: i64 = 77_760_000;

impl Kcc20State {
    /// Token state owned by an x-only public key.
    pub fn p2pk(amount: i64, owner: [u8; 32], extension_commitment: [u8; 32]) -> Self {
        Kcc20State { amount, owner, owner_scheme: SCHEME_P2PK, borrow_scheme: 0, borrow_guard: [0; 32], extension_commitment }
    }
    /// Token state held in custody by a covenant (a KOB order).
    pub fn custody(amount: i64, covenant_id: [u8; 32], extension_commitment: [u8; 32]) -> Self {
        Kcc20State {
            amount,
            owner: covenant_id,
            owner_scheme: SCHEME_COVID,
            borrow_scheme: 0,
            borrow_guard: [0; 32],
            extension_commitment,
        }
    }
    /// Redeem script under a token program.
    pub fn redeem_with(&self, tpl: &Template) -> Vec<u8> {
        tpl.redeem(&self.encode())
    }
    /// P2SH script public key under a token program.
    pub fn spk_with(&self, tpl: &Template) -> ScriptPublicKey {
        tpl.spk(&self.encode())
    }
    /// Decodes a token redeem script of the given program.
    pub fn from_redeem_with(tpl: &Template, redeem: &[u8]) -> Result<Self, StateError> {
        Self::decode(tpl.state_of(redeem).ok_or(StateError::NotInstance("KCC20"))?)
    }
}

/// KRON token state, the 46-byte span at offset 0 of every KRON token program.
///
/// The span is `0x20 owner | 0x01 id_type | 0x08 amount (LE i64) | 0x01 is_minter`. JSON keys are
/// the contract's field names.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KronState {
    /// Owner: an x-only key (`id_type` 0, 3), a script hash (1) or a covenant id (2).
    #[serde(with = "crate::json::field")]
    pub owner: [u8; 32],
    /// Owner type: 0 pubkey (token-level signature), 1 script hash, 2 covenant id (KOB custody),
    /// 3 address presence (wallet balances, maker deliveries).
    #[serde(with = "crate::json::field")]
    pub id_type: u8,
    #[serde(with = "crate::json::field")]
    pub amount: i64,
    /// Minters are never accepted by KOB.
    #[serde(with = "crate::json::field")]
    pub is_minter: u8,
}

impl KronState {
    /// Token state owned by a key by address presence (`id_type` 3).
    ///
    /// KRON wallets hold these, and KOB delivers them to makers.
    pub fn addr(amount: i64, owner: [u8; 32]) -> Self {
        KronState { owner, id_type: KRON_TYPE_ADDR, amount, is_minter: 0 }
    }
    /// Token state owned by a key that signs for it (`id_type` 0, a token-level signature).
    ///
    /// These are the holders of a program that refuses address presence (KForge), and what the
    /// `*KronD0` order builds deliver to makers.
    pub fn pubkey(amount: i64, owner: [u8; 32]) -> Self {
        KronState { owner, id_type: crate::family::KRON_TYPE_PUBKEY, amount, is_minter: 0 }
    }
    /// Token state held in custody by a covenant (a KOB order, `id_type` 2).
    pub fn custody(amount: i64, covenant_id: [u8; 32]) -> Self {
        KronState { owner: covenant_id, id_type: KRON_TYPE_COVID, amount, is_minter: 0 }
    }
    /// The 46-byte state span.
    pub fn encode(&self) -> Vec<u8> {
        let mut v = Vec::with_capacity(KRON_STATE_LEN);
        v.push(0x20);
        v.extend_from_slice(&self.owner);
        v.extend_from_slice(&[0x01, self.id_type, 0x08]);
        v.extend_from_slice(&self.amount.to_le_bytes());
        v.extend_from_slice(&[0x01, self.is_minter]);
        debug_assert_eq!(v.len(), KRON_STATE_LEN);
        v
    }
    /// Decodes a state span (canonical push opcodes only).
    pub fn decode(bytes: &[u8]) -> Result<Self, StateError> {
        let bad = |m: &str| Err(StateError::Codec("KronState", m.to_string()));
        if bytes.len() != KRON_STATE_LEN {
            return bad(&format!("state span is {} bytes, expected {KRON_STATE_LEN}", bytes.len()));
        }
        if bytes[0] != 0x20 || bytes[33] != 0x01 || bytes[35] != 0x08 || bytes[44] != 0x01 {
            return bad("non-canonical state encoding (push opcodes)");
        }
        Ok(KronState {
            owner: bytes[1..33].try_into().expect("32"),
            id_type: bytes[34],
            amount: i64::from_le_bytes(bytes[36..44].try_into().expect("8")),
            is_minter: bytes[45],
        })
    }
    /// Redeem script under a KRON token program.
    pub fn redeem_with(&self, tpl: &TokenTemplate) -> Vec<u8> {
        tpl.redeem(&self.encode())
    }
    /// P2SH script public key under a KRON token program.
    pub fn spk_with(&self, tpl: &TokenTemplate) -> ScriptPublicKey {
        tpl.spk(&self.encode())
    }
}

/// What fixes the owner value of a key-held token state.
///
/// A token program fixes it through its profile's `owners.holder`. A family fixes it through its
/// default holder owner: KCC-20 `owner_scheme` 0, KRON `id_type` 3.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HolderOf {
    Family(Family),
    Program(TemplateId),
}

impl From<Family> for HolderOf {
    fn from(f: Family) -> Self {
        HolderOf::Family(f)
    }
}

impl From<TemplateId> for HolderOf {
    fn from(t: TemplateId) -> Self {
        HolderOf::Program(t)
    }
}

impl HolderOf {
    /// The family.
    pub fn family(self) -> Family {
        match self {
            HolderOf::Family(f) => f,
            HolderOf::Program(t) => t.family(),
        }
    }
    /// The owner value (`owner_scheme` / `id_type`) of a key-held state.
    pub fn owner_type(self) -> u8 {
        match self {
            HolderOf::Program(t) => match t.profile() {
                Some(p) => p.owners.holder,
                None => HolderOf::Family(t.family()).owner_type(),
            },
            HolderOf::Family(Family::Kcc20) => SCHEME_P2PK,
            HolderOf::Family(Family::Kron) => KRON_TYPE_ADDR,
        }
    }
}

/// A token state of either family.
///
/// JSON uses the KCC-20 or the KRON layout. A state with `id_type` or `is_minter` is KRON.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TokenState {
    Kcc20(Kcc20State),
    Kron(KronState),
}

impl From<Kcc20State> for TokenState {
    fn from(s: Kcc20State) -> Self {
        TokenState::Kcc20(s)
    }
}
impl From<KronState> for TokenState {
    fn from(s: KronState) -> Self {
        TokenState::Kron(s)
    }
}

impl Serialize for TokenState {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            TokenState::Kcc20(k) => k.serialize(s),
            TokenState::Kron(k) => k.serialize(s),
        }
    }
}

impl<'de> Deserialize<'de> for TokenState {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        let v = serde_json::Value::deserialize(d)?;
        let kron = v.as_object().is_some_and(|o| o.contains_key("id_type") || o.contains_key("is_minter"));
        if kron {
            serde_json::from_value(v).map(TokenState::Kron).map_err(D::Error::custom)
        } else {
            serde_json::from_value(v).map(TokenState::Kcc20).map_err(D::Error::custom)
        }
    }
}

impl TokenState {
    /// Family of this state's layout.
    pub fn family(&self) -> Family {
        match self {
            TokenState::Kcc20(_) => Family::Kcc20,
            TokenState::Kron(_) => Family::Kron,
        }
    }
    /// Token base units held.
    pub fn amount(&self) -> i64 {
        match self {
            TokenState::Kcc20(s) => s.amount,
            TokenState::Kron(s) => s.amount,
        }
    }
    /// Owner identifier (key or covenant id).
    pub fn owner(&self) -> [u8; 32] {
        match self {
            TokenState::Kcc20(s) => s.owner,
            TokenState::Kron(s) => s.owner,
        }
    }
    /// Owned by a key: KCC-20 `owner_scheme` 0 (P2PK), or KRON `id_type` 3 or 0.
    ///
    /// KRON `id_type` 3 is address presence and 0 a token-level signature. Which of the two a
    /// program authorises is its holder owner, see [`TokenState::is_holder`].
    pub fn is_user(&self) -> bool {
        match self {
            TokenState::Kcc20(s) => s.owner_scheme == SCHEME_P2PK,
            TokenState::Kron(s) => s.id_type == KRON_TYPE_ADDR || s.id_type == crate::family::KRON_TYPE_PUBKEY,
        }
    }
    /// Owned by a key with the holder owner of `of`, i.e. a state the builders spend for it.
    ///
    /// For a token program the holder owner is its profile's `owners.holder`, for a family its
    /// default.
    pub fn is_holder(&self, of: impl Into<HolderOf>) -> bool {
        let of = of.into();
        self.family() == of.family()
            && match self {
                TokenState::Kcc20(s) => s.owner_scheme == of.owner_type(),
                TokenState::Kron(s) => s.id_type == of.owner_type(),
            }
    }
    /// Owned by a covenant id: KCC-20 `owner_scheme` 4, KRON `id_type` 2.
    pub fn is_covenant_owned(&self) -> bool {
        match self {
            TokenState::Kcc20(s) => s.owner_scheme == SCHEME_COVID,
            TokenState::Kron(s) => s.id_type == KRON_TYPE_COVID,
        }
    }
    /// Extension commitment (all zero for KRON, which has none).
    pub fn extension(&self) -> [u8; 32] {
        match self {
            TokenState::Kcc20(s) => s.extension_commitment,
            TokenState::Kron(_) => [0; 32],
        }
    }
    /// The KCC-20 state (`None` for a KRON token).
    pub fn as_kcc20(&self) -> Option<&Kcc20State> {
        match self {
            TokenState::Kcc20(s) => Some(s),
            TokenState::Kron(_) => None,
        }
    }
    /// KCC-20: borrowing disabled. KRON: not a minter. Anything else is refused by the builders.
    pub fn is_plain(&self) -> bool {
        match self {
            TokenState::Kcc20(s) => s.borrow_scheme == 0,
            TokenState::Kron(s) => s.is_minter == 0,
        }
    }
    /// Token state owned by `owner` as a key, with the holder owner of `of`.
    ///
    /// For a token program the holder owner is its profile's `owners.holder`, e.g. KRON id_type 0
    /// for KForge. For a family it is the default, KCC-20 scheme 0 or KRON id_type 3. KRON ignores
    /// `ext`.
    pub fn user(of: impl Into<HolderOf>, amount: i64, owner: [u8; 32], ext: [u8; 32]) -> Self {
        let of = of.into();
        match of.family() {
            Family::Kcc20 => TokenState::Kcc20(Kcc20State::p2pk(amount, owner, ext)),
            Family::Kron => TokenState::Kron(KronState { owner, id_type: of.owner_type(), amount, is_minter: 0 }),
        }
    }
    /// Token state in custody of a covenant id (`ext` is ignored by KRON).
    pub fn custody(fam: Family, amount: i64, covenant_id: [u8; 32], ext: [u8; 32]) -> Self {
        match fam {
            Family::Kcc20 => TokenState::Kcc20(Kcc20State::custody(amount, covenant_id, ext)),
            Family::Kron => TokenState::Kron(KronState::custody(amount, covenant_id)),
        }
    }
    /// The same state with another amount.
    pub fn with_amount(&self, amount: i64) -> Self {
        match self {
            TokenState::Kcc20(s) => TokenState::Kcc20(Kcc20State { amount, ..s.clone() }),
            TokenState::Kron(s) => TokenState::Kron(KronState { amount, ..s.clone() }),
        }
    }
    /// The same state owned by `owner` as a key, with its family's default holder owner.
    ///
    /// The default is KCC-20 scheme 0 or KRON id_type 3.
    pub fn with_user_owner(&self, owner: [u8; 32]) -> Self {
        self.with_holder_owner(self.family(), owner)
    }
    /// The same state owned by `owner` as a key with the holder owner of `of`.
    ///
    /// This is a refund or delivery to the maker. For a token program the holder owner is its
    /// profile's `owners.holder`.
    pub fn with_holder_owner(&self, of: impl Into<HolderOf>, owner: [u8; 32]) -> Self {
        let id = of.into().owner_type();
        match self {
            TokenState::Kcc20(s) => TokenState::Kcc20(Kcc20State { owner, owner_scheme: SCHEME_P2PK, ..s.clone() }),
            TokenState::Kron(s) => TokenState::Kron(KronState { owner, id_type: id, ..s.clone() }),
        }
    }
    /// The encoded state span.
    pub fn encode(&self) -> Vec<u8> {
        match self {
            TokenState::Kcc20(s) => s.encode(),
            TokenState::Kron(s) => s.encode(),
        }
    }
    /// Decodes the state span of a token program.
    pub fn decode_with(tpl: &TokenTemplate, bytes: &[u8]) -> Result<Self, StateError> {
        match tpl.family {
            Family::Kcc20 => Ok(TokenState::Kcc20(Kcc20State::decode(bytes)?)),
            Family::Kron => Ok(TokenState::Kron(KronState::decode(bytes)?)),
        }
    }
    /// Redeem script under a token program of the same family.
    pub fn redeem_with(&self, tpl: &TokenTemplate) -> Vec<u8> {
        assert_eq!(self.family(), tpl.family, "token state and program are of different families");
        tpl.redeem(&self.encode())
    }
    /// P2SH script public key under a token program of the same family.
    pub fn spk_with(&self, tpl: &TokenTemplate) -> ScriptPublicKey {
        assert_eq!(self.family(), tpl.family, "token state and program are of different families");
        tpl.spk(&self.encode())
    }
    /// Decodes a token redeem script of the given program.
    pub fn from_redeem_with(tpl: &TokenTemplate, redeem: &[u8]) -> Result<Self, StateError> {
        Self::decode_with(tpl, tpl.state_of(redeem).ok_or(StateError::NotInstance("token"))?)
    }
}

// ---------------------------------------------------------------- the quote rule

/// Rounding of [`quote_of`], always in the maker's favour.
///
/// It rounds up what a maker receives and down what a maker pays.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Round {
    /// Floor (`c = 0` in the covenant): what a maker pays.
    Down,
    /// Ceil (`c = scale - 1` in the covenant): what a maker receives.
    Up,
}

/// Quote value of `n` base units at `rate` quote units per `scale` base units.
///
/// The value is `n * rate / scale` rounded per `round`, computed exactly as the `quoteOf` of every
/// KOB covenant:
///
/// ```text
/// q = n / scale; m = n % scale
/// quoteOf = q * rate + m * (rate / scale) + (m * (rate % scale) + c) / scale
/// c = 0 (Down) or scale - 1 (Up)
/// ```
///
/// Every product and sum is checked, because the engine fails the script on an overflow.
///
/// Returns `None` exactly where the covenant fails, i.e. when an intermediate or the result does
/// not fit in an `i64`. It also returns `None` for inputs the covenants never pass: a negative
/// amount or rate, or `scale <= 0`.
///
/// Every intermediate is at most `max(result, scale * (scale - 1))`. While
/// `scale * (scale - 1) < 2^63`, the only failure is a result that does not fit in an `i64`. The
/// builders allow only powers of ten up to `10^9`, which satisfy this.
pub fn quote_of(n: i64, rate: i64, scale: i64, round: Round) -> Option<i64> {
    if n < 0 || rate < 0 || scale <= 0 {
        return None;
    }
    let rounding = match round {
        Round::Down => 0,
        Round::Up => scale - 1,
    };
    let (whole, rest) = (n / scale, n % scale);
    let whole_value = whole.checked_mul(rate)?;
    let rest_rate_value = rest.checked_mul(rate / scale)?;
    let rest_frac_value = rest.checked_mul(rate % scale)?.checked_add(rounding)? / scale;
    whole_value.checked_add(rest_rate_value)?.checked_add(rest_frac_value)
}

/// The exact value of [`quote_of`] in 128-bit arithmetic, not limited to `i64`.
///
/// Returns `None` for the same invalid inputs.
pub fn quote_exact(n: i64, rate: i64, scale: i64, round: Round) -> Option<i128> {
    if n < 0 || rate < 0 || scale <= 0 {
        return None;
    }
    let (num, scale_wide) = (n as i128 * rate as i128, scale as i128);
    Some(match round {
        Round::Down => num / scale_wide,
        Round::Up => (num + scale_wide - 1) / scale_wide,
    })
}

/// Largest scale the builders accept, `10^9` (9 decimals).
///
/// The wallet caps every larger `10^decimals` here.
pub const MAX_SCALE: i64 = 1_000_000_000;
/// Bound on the value of a full fill, in quote units (`2^62`).
///
/// A full fill of an order at any rate it carries must be worth less than this. That is far from
/// the `i64` limit of the covenant arithmetic, and leaves room for the sums the covenants add to
/// it, such as carriers and budgets.
pub const QUOTE_LIMIT: i128 = 1 << 62;

/// `scale` is a power of ten in `1..=`[`MAX_SCALE`].
pub fn check_scale(scale: i64) -> Result<(), String> {
    if !(1..=MAX_SCALE).contains(&scale) {
        return Err(format!("scale must be within 1..=10^9 (got {scale})"));
    }
    let mut reduced = scale;
    while reduced % 10 == 0 {
        reduced /= 10;
    }
    if reduced != 1 {
        return Err(format!("scale must be a power of ten (got {scale})"));
    }
    Ok(())
}

/// Whether the full fill of `amount` base units at `rate` is worth less than [`QUOTE_LIMIT`].
///
/// `what` names the rate in the error. Rates and amounts must be >= 0.
pub fn check_quote(amount: i64, rate: i64, scale: i64, what: &str) -> Result<(), String> {
    if amount < 0 || rate < 0 {
        return Err(format!("{what}: amount and rate must be >= 0"));
    }
    let value = quote_exact(amount, rate, scale, Round::Up).ok_or_else(|| format!("{what}: invalid scale {scale}"))?;
    if value >= QUOTE_LIMIT {
        return Err(format!("{what}: the full amount {amount} at {rate} per {scale} base units is worth 2^62 or more"));
    }
    Ok(())
}

/// `a + b` for two rates (a price and its tip): `None` when it does not fit.
fn rate_sum(a: i64, b: i64) -> Option<i64> {
    a.checked_add(b)
}

// ---------------------------------------------------------------- economics

/// IOC / FOK kill: anyone may refund an IOC or FOK order this many DAA after it became fillable.
///
/// It becomes fillable at `max(UTXO DAA, activeFrom)`.
pub const IOC_LIFE: i64 = 600;
/// Most token inputs a transaction that spends a KCC-20 KOB order may carry.
///
/// Every such order refuses more. KRON orders refuse more than 4, see
/// [`crate::Family::max_tok_in`] and `family::MAX_TOK_IN_KRON`.
pub const MAX_TOK_IN: usize = crate::family::MAX_TOK_IN_KCC20;
/// Scale of the repeat merge argument `-(k · 2^53 + m)`, the `MERGE_K` of the covenants.
///
/// The amount of a booked exit is below it.
pub const MERGE_SHIFT: i64 = 1 << 53;
/// Largest exit input index the merge argument can name (`k · 2^53 + m` must fit in 63 bits).
pub const MERGE_MAX_INDEX: usize = 1023;
/// Length of the committed exit prefix a buy-first entry stores.
///
/// It is the `KobCondAsk` state up to `amountLeft`.
pub const IFD_BID_EXIT_COMMIT: usize = 279;
/// Length of the committed exit prefix a sell-first entry stores.
///
/// It is the `KobCondBid` state up to `keeperTip`.
pub const IFD_ASK_EXIT_COMMIT: usize = 321;
/// The same for a KRON entry, whose `KobCondBidKron` exit has no extension commitment.
///
/// It is 33 bytes shorter.
pub const IFD_ASK_EXIT_COMMIT_KRON: usize = 288;

/// Decay or rise origin.
///
/// It is `activeFrom`, or for a TWAP / DCA (`interval > 0`) the opening of the current slice
/// (`UTXO DAA + interval`) when that is later, so every slice is its own auction. Returns `None`
/// where the covenant's own sum overflows. The order is unfillable there, because every fill
/// computes it.
pub fn decay_origin(active_from: i64, interval: i64, utxo_daa: i64) -> Option<i64> {
    if interval > 0 {
        Some(active_from.max(utxo_daa.checked_add(interval)?))
    } else {
        Some(active_from)
    }
}

/// Decayed ask quote at time `t`, never below `price_end`.
///
/// The quote is `max(priceEnd, price - slope * floor((t - origin) / step))`. Every operation is
/// checked as in the covenant, where an overflow fails the script. Returns `None` there, and for
/// `step <= 0`, because the covenant requires `decayStep > 0`.
pub fn decay_down(price: i64, price_end: i64, slope: i64, step: i64, origin: i64, t: i64) -> Option<i64> {
    if step <= 0 {
        return None;
    }
    let moved = slope.checked_mul(t.checked_sub(origin)? / step)?;
    Some(price.checked_sub(moved)?.max(price_end))
}

/// Rising bid quote at time `t` (never above `price_end`), checked like [`decay_down`].
pub fn rise_up(price: i64, price_end: i64, slope: i64, step: i64, origin: i64, t: i64) -> Option<i64> {
    if step <= 0 {
        return None;
    }
    let moved = slope.checked_mul(t.checked_sub(origin)? / step)?;
    Some(price.checked_add(moved)?.min(price_end))
}

/// DAA score from which an order is refundable.
///
/// It is the earliest of the soft expiry, 90 days idle, and for IOC / FOK (`tif != 0`) the kill
/// time `max(UTXO DAA, activeFrom) + 600`. Returns `i64::MAX` (never) where the covenant's own
/// sums overflow. Its refund path fails there, and only the maker's cancel ends such an order.
pub fn refund_due(expiry_daa: i64, tif: i64, active_from: i64, utxo_daa: i64) -> i64 {
    let Some(mut idle) = utxo_daa.checked_add(MAX_IDLE) else { return i64::MAX };
    if tif != TIF_GTC {
        let Some(kill) = active_from.max(utxo_daa).checked_add(IOC_LIFE) else { return i64::MAX };
        idle = idle.min(kill);
    }
    expiry_daa.min(idle)
}

/// `rptUntil` a repeating entry writes into a booked exit.
///
/// It is `min(expiryDaa, UTXO DAA + 90 days)`, counted from the DAA of the entry UTXO (its last
/// activity) and never from an argument of the filler. Returns `None` where the covenant's sum
/// overflows, and such a booking fails.
pub fn rpt_until(expiry_daa: i64, utxo_daa: i64) -> Option<i64> {
    Some(expiry_daa.min(utxo_daa.checked_add(MAX_IDLE)?))
}

/// The largest fill of a repeating entry while its re-arms last.
///
/// This applies to `KobIfdBid`, `KobIfdAsk` and `KobIfdPair`. The bound is `rptAmount - 1`, and
/// every fill up to it is booked, as long as at least one minimum fill of re-arms is left
/// (`rptAmount - 1 >= minFill`). A larger fill would leave the rest of the re-arms unused for
/// good, and the covenants refuse it.
///
/// Returns `None` when there is no bound: not repeating, the re-arms used up, or fewer left than
/// one minimum fill.
pub fn rpt_fill_max(rpt_amount: i64, min_fill: i64) -> Option<i64> {
    (rpt_amount > 1 && rpt_amount > min_fill).then(|| rpt_amount - 1)
}

/// Auction origin of an armed order: `armed = 1` means the arming UTXO's own DAA score.
pub fn armed_origin(armed: i64, utxo_daa: i64) -> Option<i64> {
    match armed {
        0 => None,
        1 => Some(utxo_daa),
        o => Some(o),
    }
}

/// Stop-band basis points at auction time `t`.
///
/// The band opens linearly over `band_daa` from the origin, and `band_daa = 0` opens the whole
/// band at once. Returns `None` where the covenant's arithmetic overflows (`t - origin`,
/// `slipBps * e`).
pub fn band_bps(slip_bps: i64, band_daa: i64, origin: i64, t: i64) -> Option<i64> {
    if band_daa <= 0 {
        return Some(slip_bps);
    }
    let e = t.checked_sub(origin)?;
    if e < band_daa {
        Some(slip_bps.checked_mul(e)? / band_daa)
    } else {
        Some(slip_bps)
    }
}

/// Largest stop price the covenants accept on a stop leg.
///
/// `stopPrice * slipBps` with `slipBps <= 10000` must fit in an i64. This is `MAX_STOP` in
/// KobCondAsk and KobCondBid.
pub const MAX_STOP_PRICE: i64 = 922_337_203_685_477;

/// The covenant's stop band of a stop leg: `floor(stop * bps / 10000)`.
///
/// The covenants refuse a stop leg unless `stop <= MAX_STOP_PRICE` and `0 <= bps <= 10000`.
/// Returns `None` where the covenant refuses or fails.
pub fn stop_band(stop: i64, bps: i64) -> Option<i64> {
    if !(0..=MAX_STOP_PRICE).contains(&stop) || !(0..=10_000).contains(&bps) {
        return None;
    }
    Some(stop * bps / 10_000)
}

/// Stop band in sompi per whole token: `floor(stop * bps / 10000)`.
///
/// The multiplication comes first, as in the covenants. Rounding down is in the maker's favour on
/// both sides. Saturates for a stop above [`MAX_STOP_PRICE`], whose stop leg the covenants refuse
/// anyway.
pub fn band(stop: i64, bps: i64) -> i64 {
    stop.checked_mul(bps).map(|p| p / 10_000).unwrap_or(i64::MAX)
}

/// `armed` written into a conditional's continuation after a fill on `leg`.
///
/// `trigger` means the order is armed by touch evidence in this very transaction.
fn cond_next_armed(armed: i64, band_daa: i64, leg: i64, trigger: bool, utxo_daa: i64) -> i64 {
    if leg == 1 && armed == 0 && trigger {
        return 1;
    }
    if armed == 1 && band_daa > 0 {
        utxo_daa
    } else {
        armed
    }
}

/// Repeat fields of an exit booked by its entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Booking {
    /// The entry's covenant id.
    pub parent: [u8; 32],
    /// `rptUntil` (see [`rpt_until`]).
    pub until: i64,
}

fn ceil_div(a: i64, b: i64) -> i64 {
    if b <= 0 {
        return a;
    }
    a / b + i64::from(a % b != 0)
}

/// The minimum-fill rule of the kinds whose quantity is an explicit amount.
///
/// It is `0 < n <= left`, and `n >= minFill` unless `n` takes everything left.
pub fn min_fill_ok(n: i64, left: i64, min_fill: i64) -> bool {
    n > 0 && n <= left && (n >= min_fill || n == left)
}

impl AskState {
    /// The exact custody amount (`amountLeft` base units).
    pub fn custody_amount(&self) -> i64 {
        self.amount_left
    }
    /// Decay origin of the UTXO created at `utxo_daa` (`None` where the covenant's sum overflows).
    pub fn origin(&self, utxo_daa: i64) -> Option<i64> {
        decay_origin(self.active_from, self.interval, utxo_daa)
    }
    /// Quote at auction time `t` for the UTXO created at `utxo_daa`.
    ///
    /// It is the constant price when the order does not decay. Returns `None` where the covenant's
    /// decay arithmetic fails.
    pub fn price_at(&self, t: i64, utxo_daa: i64) -> Option<i64> {
        if self.slope != 0 {
            decay_down(self.price, self.price_end, self.slope, self.decay_step, self.origin(utxo_daa)?, t)
        } else {
            Some(self.price)
        }
    }
    /// Least the maker receives for n base units at quote `p`: `ceil(n * (p - tip) / scale)`.
    ///
    /// Returns `None` if `p < tip` or the covenant's arithmetic fails.
    pub fn proceeds_at(&self, n: i64, p: i64) -> Option<i64> {
        if p < self.tip || self.tip < 0 {
            return None;
        }
        quote_of(n, p - self.tip, self.scale, Round::Up)
    }
    /// [`AskState::proceeds_at`] at auction time `t`.
    pub fn proceeds(&self, n: i64, t: i64, utxo_daa: i64) -> Option<i64> {
        self.proceeds_at(n, self.price_at(t, utxo_daa)?)
    }
    /// The covenant's quantity rules for a fill of n.
    ///
    /// They are `0 < n <= amountLeft`, the minimum fill and the TWAP `maxFill`.
    pub fn fill_ok(&self, n: i64) -> bool {
        min_fill_ok(n, self.amount_left, self.min_fill) && (self.max_fill == 0 || n <= self.max_fill)
    }
}

/// Family of a pair order's token from its code: `1` KCC-20, `2` KRON.
///
/// The codes are those of `sFamily`, `tFamily`, `aFamily` and `bFamily`, the payload family byte.
pub fn family_of_code(code: i64) -> Option<Family> {
    u8::try_from(code).ok().and_then(Family::from_code)
}

impl BidState {
    /// Highest quote the bid can reach (the cap of a rising bid).
    pub fn price_max(&self) -> i64 {
        if self.slope != 0 && self.price_end > self.price {
            self.price_end
        } else {
            self.price
        }
    }
    /// The budget rate `pMax + tip` (sompi per whole token) at which a fill consumes the escrow.
    pub fn budget_rate(&self) -> Option<i64> {
        rate_sum(self.price_max(), self.tip)
    }
    /// Rise origin (`None` where the covenant's sum overflows).
    pub fn origin(&self, utxo_daa: i64) -> Option<i64> {
        decay_origin(self.active_from, self.interval, utxo_daa)
    }
    /// Quote at auction time `t` (`None` where the covenant's rise arithmetic fails).
    pub fn price_at(&self, t: i64, utxo_daa: i64) -> Option<i64> {
        if self.slope != 0 {
            rise_up(self.price, self.price_end, self.slope, self.decay_step, self.origin(utxo_daa)?, t)
        } else {
            Some(self.price)
        }
    }
    /// Budget a fill of n consumes from the escrow: `used(n) = ceil(n * (pMax + tip) / scale)`.
    pub fn used(&self, n: i64) -> Option<i64> {
        if self.tip < 0 {
            return None;
        }
        quote_of(n, self.budget_rate()?, self.scale, Round::Up)
    }
    /// Most the maker pays for n base units at quote `p`: `floor(n * (p + tip) / scale)`.
    ///
    /// This is the covenant's `allIn`.
    pub fn spend_at(&self, n: i64, p: i64) -> Option<i64> {
        if self.tip < 0 {
            return None;
        }
        quote_of(n, rate_sum(p, self.tip)?, self.scale, Round::Down)
    }
    /// [`BidState::spend_at`] at auction time `t`.
    pub fn spend(&self, n: i64, t: i64, utxo_daa: i64) -> Option<i64> {
        self.spend_at(n, self.price_at(t, utxo_daa)?)
    }
    /// Whether a bid whose escrow keeps `left` after a fill can continue.
    ///
    /// It can when one more minimum fill of buying power is left:
    /// `left − deliveryCarrier − reserve >= used(minFill)`.
    pub fn can_continue(&self, left: i64) -> bool {
        match (self.used(self.min_fill), left.checked_sub(self.delivery_carrier).and_then(|x| x.checked_sub(self.reserve))) {
            (Some(u), Some(free)) => free >= u,
            _ => false,
        }
    }
    /// Remaining buying power of an escrow of `value` sompi.
    ///
    /// It is the largest n with `used(n) <= value − deliveryCarrier − reserve`, and 0 when not even
    /// one base unit is funded.
    ///
    /// It is O(1) and total over any state, including a hostile one decoded from the chain. It is 0
    /// when `tip < 0`, when the budget rate `pMax + tip` overflows or is not positive, when `scale`
    /// fails [`check_scale`], or when the budget is not positive. Such a bid cannot be filled.
    ///
    /// Otherwise `used(n)` is the exact `ceil(n * rate / scale)` wherever that fits in an `i64`: a
    /// scale of at most `10^9` keeps every other intermediate of [`quote_of`] below `2^63`. The
    /// covenant can therefore spend at most `min(budget, i64::MAX)`, and the result is the largest n
    /// with `n * rate <= min(budget, i64::MAX) * scale`, i.e. `floor(min(budget, i64::MAX) * scale
    /// / rate)`, capped at `i64::MAX`. Clamping the budget is the direct form of the largest n at
    /// which the covenant's arithmetic does not overflow; it only matters for a negative carrier or
    /// reserve. The result is then checked against [`BidState::used`], and 0 is returned should it
    /// ever disagree.
    pub fn buying_power(&self, value: i64) -> i64 {
        if self.tip < 0 || check_scale(self.scale).is_err() {
            return 0;
        }
        let Some(rate) = self.budget_rate().filter(|r| *r > 0) else { return 0 };
        let budget = value as i128 - self.delivery_carrier as i128 - self.reserve as i128;
        if budget <= 0 {
            return 0;
        }
        let budget = budget.min(i64::MAX as i128);
        // ceil(n * rate / scale) <= budget  <=>  n * rate <= budget * scale
        let n = (budget * self.scale as i128 / rate as i128).min(i64::MAX as i128) as i64;
        match self.used(n) {
            Some(u) if u as i128 <= budget => n,
            _ => 0,
        }
    }
    /// The covenant's rules for a fill of n of an escrow of `value`.
    ///
    /// They are `n > 0`, `maxFill`, `used(n)` leaves at least the reserve, and `n >= minFill`
    /// unless the bid then cannot continue. They also require `minFill > 0`.
    pub fn fill_ok(&self, n: i64, value: i64) -> bool {
        if n <= 0 || self.min_fill <= 0 || (self.max_fill != 0 && n > self.max_fill) {
            return false;
        }
        let Some(left) = self.used(n).and_then(|u| value.checked_sub(u)) else { return false };
        left >= self.reserve && (n >= self.min_fill || !self.can_continue(left))
    }
    /// Escrow for `amount` base units delivered in at most `fills` fills.
    ///
    /// It is `used(amount)` plus `fills − 1` sompi of rounding, one delivery carrier per fill and
    /// the reserve. Each fill's budget is rounded up, so any split of the amount into at most
    /// `fills` fills is funded.
    pub fn escrow(&self, amount: i64, fills: i64) -> Option<i64> {
        let fills = fills.max(1);
        self.used(amount)?.checked_add(fills - 1)?.checked_add(fills.checked_mul(self.delivery_carrier)?)?.checked_add(self.reserve)
    }
}

impl CondAskState {
    /// The exact custody amount (`amountLeft` base units).
    pub fn custody_amount(&self) -> i64 {
        self.amount_left
    }
    /// True for an exit booked by a repeating entry.
    pub fn is_booked(&self) -> bool {
        self.parent != [0; 32]
    }
    /// Stop-leg price for a fill at auction time `t`.
    ///
    /// `trigger` means the order is armed by touch evidence in the same transaction, which pays the
    /// stop itself when `bandDaa > 0`.
    pub fn stop_at(&self, trigger: bool, t: i64, utxo_daa: i64) -> Option<i64> {
        let bps = match (trigger, armed_origin(self.armed, utxo_daa)) {
            (true, _) | (false, None) => {
                if self.band_daa > 0 {
                    0
                } else {
                    self.slip_bps
                }
            }
            (false, Some(o)) => band_bps(self.slip_bps, self.band_daa, o, t)?,
        };
        stop_band(self.stop_price, bps).map(|b| self.stop_price - b)
    }
    /// The band floor (the stop-leg's worst price).
    pub fn stop_floor(&self) -> i64 {
        self.stop_price - band(self.stop_price, self.slip_bps)
    }
    /// Leg price at time `t`: leg 0 is the take-profit, leg 1 the stop.
    ///
    /// Returns `None` where the covenant's stop arithmetic fails: a stop above `MAX_STOP_PRICE`, or
    /// an overflow.
    pub fn leg_price(&self, leg: i64, trigger: bool, t: i64, utxo_daa: i64) -> Option<i64> {
        if leg == 0 {
            Some(self.tp_price)
        } else {
            self.stop_at(trigger, t, utxo_daa)
        }
    }
    /// Least the maker receives for n base units at a leg price:
    /// `ceil(n * (legPrice − tip) / scale)`.
    ///
    /// Returns `None` when the leg price is below the tip, because the covenant refuses that fill.
    pub fn proceeds(&self, n: i64, leg_price: i64) -> Option<i64> {
        if leg_price < self.tip || self.tip < 0 {
            return None;
        }
        quote_of(n, leg_price - self.tip, self.scale, Round::Up)
    }
    /// Repeat IFD: the budget a take-profit of n returns to the entry,
    /// `ceil(n * rptPrice / scale)`.
    pub fn rpt_budget(&self, n: i64) -> Option<i64> {
        quote_of(n, self.rpt_price, self.scale, Round::Up)
    }
    /// The covenant's quantity rule for a fill of n.
    pub fn fill_ok(&self, n: i64) -> bool {
        min_fill_ok(n, self.amount_left, self.min_fill)
    }
    /// `armed` of the continuation after a fill.
    pub fn next_armed(&self, leg: i64, trigger: bool, utxo_daa: i64) -> i64 {
        cond_next_armed(self.armed, self.band_daa, leg, trigger, utxo_daa)
    }
    /// Trailing ratchet from side-2 evidence (a resting bid filled) quoting `rp`.
    ///
    /// Returns every step it justifies, capped below `tpPrice`, and 0 when none is justified.
    pub fn trail_steps(&self, rp: i64) -> i64 {
        if self.trail_step <= 0 {
            return 0;
        }
        let mut steps = (rp - self.trail_gap - self.stop_price).div_euclid(self.trail_step);
        if self.tp_price > 0 {
            steps = steps.min((self.tp_price - 1 - self.stop_price).div_euclid(self.trail_step));
        }
        steps.max(0)
    }
}

impl CondBidState {
    /// True for an exit booked by a repeating entry.
    pub fn is_booked(&self) -> bool {
        self.parent != [0; 32]
    }
    /// Stop-leg price for a fill at auction time `t` (the band opens upwards).
    pub fn stop_at(&self, trigger: bool, t: i64, utxo_daa: i64) -> Option<i64> {
        let bps = match (trigger, armed_origin(self.armed, utxo_daa)) {
            (true, _) | (false, None) => {
                if self.band_daa > 0 {
                    0
                } else {
                    self.slip_bps
                }
            }
            (false, Some(o)) => band_bps(self.slip_bps, self.band_daa, o, t)?,
        };
        self.stop_price.checked_add(stop_band(self.stop_price, bps)?)
    }
    /// The band ceiling (the stop leg's worst price).
    pub fn stop_ceiling(&self) -> i64 {
        self.stop_price.saturating_add(band(self.stop_price, self.slip_bps))
    }
    /// Leg price at time `t`: leg 0 is the `tpPrice` leg, leg 1 the stop.
    ///
    /// Returns `None` where the covenant's stop arithmetic fails.
    pub fn leg_price(&self, leg: i64, trigger: bool, t: i64, utxo_daa: i64) -> Option<i64> {
        if leg == 0 {
            Some(self.tp_price)
        } else {
            self.stop_at(trigger, t, utxo_daa)
        }
    }
    /// Worst quote either leg can pay.
    pub fn worst(&self) -> i64 {
        self.tp_price.max(if self.stop_price > 0 { self.stop_ceiling() } else { 0 })
    }
    /// What a keeper's `update` may take of the order UTXO's `value` sompi, the covenant's rule.
    ///
    /// The update arms the order, or trails it to `new_stop`. The keeper takes at most `keeperTip`,
    /// and never what buying `amountLeft` in one fill at the dearest leg needs. That leg is the
    /// take-profit or limit, or `new_stop` with its whole band, and the need is
    /// `floor(amountLeft × (worst + tip) / scale)` plus the delivery carrier. The take is 0 when
    /// the order holds no more than that. Returns `None` where the covenant's arithmetic fails.
    pub fn keeper_take(&self, value: i64, new_stop: i64) -> Option<i64> {
        let mut worst = self.tp_price;
        if (0..=10_000).contains(&self.slip_bps) && new_stop <= MAX_STOP_PRICE {
            let top = new_stop.checked_add(new_stop.checked_mul(self.slip_bps)? / 10_000)?;
            worst = worst.max(top);
        }
        let need =
            quote_of(self.amount_left, worst.checked_add(self.tip)?, self.scale, Round::Down)?.checked_add(self.delivery_carrier)?;
        let floor = value.checked_sub(self.keeper_tip)?.max(need).min(value);
        Some(value - floor)
    }
    /// Most the maker pays for n base units at a leg price: `floor(n * (legPrice + tip) / scale)`.
    pub fn spend(&self, n: i64, leg_price: i64) -> Option<i64> {
        if self.tip < 0 {
            return None;
        }
        quote_of(n, rate_sum(leg_price, self.tip)?, self.scale, Round::Down)
    }
    /// Repeat IFD: the entry's proceeds for a take-profit of n, `ceil(n * rptPrice / scale)`.
    ///
    /// The maker's output is at least this minus the buy-back spend.
    pub fn rpt_proceeds(&self, n: i64) -> Option<i64> {
        quote_of(n, self.rpt_price, self.scale, Round::Up)
    }
    /// Repeat IFD: the prefund of n returned to the entry, `ceil(n * rptPre / scale)`.
    pub fn rpt_prefund(&self, n: i64) -> Option<i64> {
        quote_of(n, self.rpt_pre, self.scale, Round::Up)
    }
    /// The covenant's quantity rule for a fill of n.
    pub fn fill_ok(&self, n: i64) -> bool {
        min_fill_ok(n, self.amount_left, self.min_fill)
    }
    /// `armed` of the continuation after a fill.
    pub fn next_armed(&self, leg: i64, trigger: bool, utxo_daa: i64) -> i64 {
        cond_next_armed(self.armed, self.band_daa, leg, trigger, utxo_daa)
    }
    /// Most fills the order can take (`⌈amountLeft / minFill⌉`).
    pub fn max_fills(&self) -> i64 {
        ceil_div(self.amount_left, self.min_fill.max(1))
    }
    /// Escrow for all `amountLeft` at the worst leg, delivered in at most `fills` fills.
    ///
    /// Each fill's spend is rounded down, so the spend of the whole amount covers any split.
    pub fn escrow(&self, fills: i64) -> Option<i64> {
        self.spend(self.amount_left, self.worst())?.checked_add(fills.max(1).checked_mul(self.delivery_carrier)?)
    }
    /// Trailing ratchet from side-1 evidence (a resting ask filled) quoting `rp`.
    ///
    /// Returns every step it justifies, capped above `tpPrice` and 0, and 0 when none is justified.
    pub fn trail_steps(&self, rp: i64) -> i64 {
        if self.trail_step <= 0 {
            return 0;
        }
        let steps = (self.stop_price - self.trail_gap - rp).div_euclid(self.trail_step);
        let floor = self.tp_price.max(0);
        let floor_steps = (self.stop_price - floor - 1).div_euclid(self.trail_step);
        steps.min(floor_steps).max(0)
    }
}

/// Stop-entry price of an if-done entry at auction time `t`.
///
/// The entry is an auction from `entry_stop` to the limit `price` over `band_daa` from the arming
/// time. Returns `None` where the covenant's arithmetic overflows (`t - origin`,
/// `(price - entryStop) * e`).
fn entry_price(price: i64, entry_stop: i64, band_daa: i64, armed: i64, trigger: bool, t: i64, utxo_daa: i64) -> Option<i64> {
    if entry_stop <= 0 {
        return Some(price);
    }
    Some(match (trigger, armed_origin(armed, utxo_daa)) {
        (true, _) | (false, None) => {
            if band_daa > 0 {
                entry_stop
            } else {
                price
            }
        }
        (false, Some(origin)) => {
            let elapsed = t.checked_sub(origin)?;
            if band_daa > 0 && elapsed < band_daa {
                entry_stop.checked_add(price.checked_sub(entry_stop)?.checked_mul(elapsed)? / band_daa)?
            } else {
                price
            }
        }
    })
}

/// `armed` of an entry continued by a merge.
///
/// An entry with nothing left restarts unarmed, so a new cycle waits for a new trigger. An entry
/// armed by `update` and not filled yet (`armed = 1`, band auction) records its auction origin,
/// the DAA of the armed UTXO, as a fill does. The merge then does not restart the band.
fn entry_merged_armed(amount_left: i64, band_daa: i64, armed: i64, utxo_daa: i64) -> i64 {
    if amount_left == 0 {
        0
    } else if armed == 1 && band_daa > 0 {
        utxo_daa
    } else {
        armed
    }
}

fn entry_next_armed(entry_stop: i64, band_daa: i64, armed: i64, utxo_daa: i64) -> i64 {
    if entry_stop <= 0 {
        return armed;
    }
    match armed {
        0 => 1,
        1 if band_daa > 0 => utxo_daa,
        a => a,
    }
}

/// Encoded repeat fields an entry appends, exactly as the covenants splice them.
///
/// The fields are `0x20 parent`, `0x08 rptPrice`, an optional `0x08 rptPre` and `0x08 rptUntil`.
fn repeat_tail(parent: [u8; 32], rpt_price: i64, rpt_pre: Option<i64>, until: i64) -> Vec<u8> {
    let mut script = vec![0x20];
    script.extend_from_slice(&parent);
    script.push(0x08);
    script.extend_from_slice(&rpt_price.to_le_bytes());
    if let Some(p) = rpt_pre {
        script.push(0x08);
        script.extend_from_slice(&p.to_le_bytes());
    }
    script.push(0x08);
    script.extend_from_slice(&until.to_le_bytes());
    script
}

/// The extension commitment a buy-first entry writes behind the repeat fields of its exit.
///
/// The span is `0x20 ext`, where `ext` is the entry's own `extensionCommitment`, the commitment of
/// the custody it delivers. It is zero for KRON, whose exit has no such field.
fn ext_tail(ext: [u8; 32]) -> Vec<u8> {
    let mut script = vec![0x20];
    script.extend_from_slice(&ext);
    script
}

impl IfdBidState {
    /// The exit prefix a buy-first entry commits to (the exit must not be booked).
    pub fn commit_exit(exit: &CondAskState) -> Vec<u8> {
        exit.encode()[..IFD_BID_EXIT_COMMIT].to_vec()
    }
    /// The committed exit order (plain, with the committed `amountLeft`).
    pub fn exit(&self) -> Result<CondAskState, StateError> {
        if self.exit_state.len() != IFD_BID_EXIT_COMMIT {
            return Err(StateError::Codec("KobIfdBid", format!("exitState must be {IFD_BID_EXIT_COMMIT} bytes")));
        }
        CondAskState::decode(
            &[self.exit_state.clone(), repeat_tail([0; 32], 0, None, 0), ext_tail(self.extension_commitment)].concat(),
        )
    }
    /// The exit a fill of n creates (booked when the entry repeats and `rptAmount > n`).
    pub fn exit_for(&self, n: i64, booking: Option<Booking>) -> Result<CondAskState, StateError> {
        let mut exit = CondAskState { amount_left: n, ..self.exit()? };
        if let Some(terms) = booking {
            exit.parent = terms.parent;
            exit.rpt_price = self.budget_rate().ok_or(StateError::Codec("KobIfdBid", "price + tip overflows".into()))?;
            exit.rpt_until = terms.until;
        }
        Ok(exit)
    }
    /// Whether `x` is exactly an exit this entry books with parent `me`.
    ///
    /// Every immutable field must match, while the mutable `stopPrice`, `armed` and `amountLeft`
    /// may differ. The entry's merge requires this.
    pub fn books_exit(&self, me: [u8; 32], x: &CondAskState) -> bool {
        let booking = Booking { parent: me, until: x.rpt_until };
        self.exit_for(x.amount_left, Some(booking))
            .map(|e| CondAskState { stop_price: x.stop_price, armed: x.armed, ..e } == *x)
            .unwrap_or(false)
    }
    /// The all-in budget rate `price + tip` (sompi per whole token): a booked exit's `rptPrice`.
    pub fn budget_rate(&self) -> Option<i64> {
        rate_sum(self.price, self.tip)
    }
    /// Entry quote at time `t`: a stop entry's auction, or the limit otherwise.
    ///
    /// Returns `None` where the covenant's arithmetic fails.
    pub fn price_at(&self, trigger: bool, t: i64, utxo_daa: i64) -> Option<i64> {
        entry_price(self.price, self.entry_stop, self.band_daa, self.armed, trigger, t, utxo_daa)
    }
    /// `armed` of the continuation after a fill.
    pub fn next_armed(&self, utxo_daa: i64) -> i64 {
        entry_next_armed(self.entry_stop, self.band_daa, self.armed, utxo_daa)
    }
    /// `armed` of the continuation after a merge ([`entry_merged_armed`]).
    pub fn merged_armed(&self, utxo_daa: i64) -> i64 {
        entry_merged_armed(self.amount_left, self.band_daa, self.armed, utxo_daa)
    }
    /// Most paid for n base units at quote `p`: `floor(n * (p + tip) / scale)`.
    pub fn spend(&self, n: i64, p: i64) -> Option<i64> {
        if self.tip < 0 {
            return None;
        }
        quote_of(n, rate_sum(p, self.tip)?, self.scale, Round::Down)
    }
    /// What a merge of m returns to the entry: `ceil(m * (price + tip) / scale)`.
    pub fn merge_budget(&self, m: i64) -> Option<i64> {
        quote_of(m, self.budget_rate()?, self.scale, Round::Up)
    }
    /// The covenant's quantity rule for a fill of n.
    pub fn fill_ok(&self, n: i64) -> bool {
        min_fill_ok(n, self.amount_left, self.min_fill)
    }
    /// Most fills the entry can take (`⌈amountLeft / minFill⌉`).
    pub fn max_fills(&self) -> i64 {
        ceil_div(self.amount_left, self.min_fill.max(1))
    }
    /// Escrow of the entry.
    ///
    /// It is the spend of the whole amount at the limit, one delivery and one exit carrier per
    /// possible fill, and the entry's own carrier when it repeats, since it outlives its last fill.
    /// Each fill's spend is rounded down, so the spend of the whole amount covers any split.
    pub fn escrow(&self) -> Option<i64> {
        self.spend(self.amount_left, self.price)?
            .checked_add(self.max_fills().checked_mul(self.delivery_carrier.checked_add(self.exit_carrier)?)?)?
            .checked_add(if self.rpt_amount > 0 { self.exit_carrier } else { 0 })
    }
}

impl IfdAskState {
    /// The exit prefix a KCC-20 sell-first entry commits to (the exit must not be booked).
    pub fn commit_exit(exit: &CondBidState) -> Vec<u8> {
        Self::commit_exit_for(Family::Kcc20, exit)
    }
    /// The exit prefix a sell-first entry of `fam` commits to.
    ///
    /// It is 321 bytes for KCC-20 and 288 bytes for KRON, whose exit has no extension commitment.
    pub fn commit_exit_for(fam: Family, exit: &CondBidState) -> Vec<u8> {
        let n = match fam {
            Family::Kcc20 => IFD_ASK_EXIT_COMMIT,
            Family::Kron => IFD_ASK_EXIT_COMMIT_KRON,
        };
        exit.encode_for(fam)[..n].to_vec()
    }
    /// Family of the committed exit (the prefix length tells: 321 KCC-20, 288 KRON).
    pub fn exit_family(&self) -> Result<Family, StateError> {
        match self.exit_state.len() {
            IFD_ASK_EXIT_COMMIT => Ok(Family::Kcc20),
            IFD_ASK_EXIT_COMMIT_KRON => Ok(Family::Kron),
            n => Err(StateError::Codec(
                "KobIfdAsk",
                format!("exitState must be {IFD_ASK_EXIT_COMMIT} (KCC-20) or {IFD_ASK_EXIT_COMMIT_KRON} (KRON) bytes, got {n}"),
            )),
        }
    }
    /// The committed exit order (plain, with the committed `amountLeft`).
    pub fn exit(&self) -> Result<CondBidState, StateError> {
        let fam = self.exit_family()?;
        CondBidState::decode_as(
            TemplateId::KobCondBid.in_family(fam),
            &[self.exit_state.clone(), repeat_tail([0; 32], 0, Some(0), 0)].concat(),
        )
    }
    /// The exit a fill of n creates (booked when the entry repeats and `rptAmount > n`).
    pub fn exit_for(&self, n: i64, booking: Option<Booking>) -> Result<CondBidState, StateError> {
        let mut exit = CondBidState { amount_left: n, ..self.exit()? };
        if let Some(terms) = booking {
            exit.parent = terms.parent;
            exit.rpt_price = self.proceeds_rate();
            exit.rpt_pre = self.prefund;
            exit.rpt_until = terms.until;
        }
        Ok(exit)
    }
    /// Whether `x` is exactly an exit this entry books with parent `me`.
    ///
    /// Every immutable field must match, while the mutable `stopPrice`, `armed` and `amountLeft`
    /// may differ. The entry's merge requires this.
    pub fn books_exit(&self, me: [u8; 32], x: &CondBidState) -> bool {
        let booking = Booking { parent: me, until: x.rpt_until };
        self.exit_for(x.amount_left, Some(booking))
            .map(|e| CondBidState { stop_price: x.stop_price, armed: x.armed, ..e } == *x)
            .unwrap_or(false)
    }
    /// The exact custody amount (`amountLeft` base units).
    pub fn custody_amount(&self) -> i64 {
        self.amount_left
    }
    /// The all-in proceeds rate at the limit, `price − tip` in sompi per whole token.
    ///
    /// It is the `rptPrice` of a booked exit.
    pub fn proceeds_rate(&self) -> i64 {
        self.price - self.tip
    }
    /// Entry quote at time `t` (`None` where the covenant's arithmetic fails).
    pub fn price_at(&self, trigger: bool, t: i64, utxo_daa: i64) -> Option<i64> {
        entry_price(self.price, self.entry_stop, self.band_daa, self.armed, trigger, t, utxo_daa)
    }
    /// `armed` of the continuation after a fill.
    pub fn next_armed(&self, utxo_daa: i64) -> i64 {
        entry_next_armed(self.entry_stop, self.band_daa, self.armed, utxo_daa)
    }
    /// `armed` of the continuation after a merge ([`entry_merged_armed`]).
    pub fn merged_armed(&self, utxo_daa: i64) -> i64 {
        entry_merged_armed(self.amount_left, self.band_daa, self.armed, utxo_daa)
    }
    /// Least proceeds of n base units at quote `p`: `ceil(n * (p − tip) / scale)`.
    ///
    /// The covenant requires `price >= tip`, so `p >= price >= tip`. Returns `None` otherwise.
    pub fn proceeds(&self, n: i64, p: i64) -> Option<i64> {
        if self.price < self.tip || p < self.tip || self.tip < 0 {
            return None;
        }
        quote_of(n, p - self.tip, self.scale, Round::Up)
    }
    /// Prefund of n base units, `ceil(n * prefund / scale)`.
    ///
    /// A fill moves it to the exit, and a merge returns it.
    pub fn prefund_of(&self, n: i64) -> Option<i64> {
        quote_of(n, self.prefund, self.scale, Round::Up)
    }
    /// What a merge returns from an exit that sells out holding `exit_value`.
    ///
    /// It is `max(prefund(m), exit_value − ceil(m * (price − tip) / scale))`, the covenant's floor
    /// over the entry's own value.
    pub fn merge_sellout_back(&self, m: i64, exit_value: i64) -> Option<i64> {
        let pre = self.prefund_of(m)?;
        let proceeds = quote_of(m, self.proceeds_rate(), self.scale, Round::Up)?;
        Some(pre.max(exit_value.checked_sub(proceeds)?))
    }
    /// The covenant's quantity rule for a fill of n.
    pub fn fill_ok(&self, n: i64) -> bool {
        min_fill_ok(n, self.amount_left, self.min_fill)
    }
    /// Most fills the entry can take (`⌈amountLeft / minFill⌉`).
    pub fn max_fills(&self) -> i64 {
        ceil_div(self.amount_left, self.min_fill.max(1))
    }
    /// Entry value: its carrier plus the prefund of the whole amount and its rounding.
    ///
    /// The rounding is `max_fills − 1` sompi, since each fill's prefund is rounded up, so any split
    /// is funded. One exit carrier per possible fill is added.
    pub fn escrow(&self, carrier: i64) -> Option<i64> {
        let fills = self.max_fills();
        carrier
            .checked_add(self.prefund_of(self.amount_left)?)?
            .checked_add((fills - 1).max(0))?
            .checked_add(fills.checked_mul(self.exit_carrier)?)
    }
}

/// Any order state, tagged by template name (JSON: `{"kind": "KobAsk", "state": {..}}`).
///
/// The KRON kinds carry the same typed states as their KCC-20 counterparts, since the KRON
/// adapters have the same fields. See [`StateCodec`] for the extension commitment. They differ in
/// the template they encode under, so the variant names the family. The pair kinds are one
/// template for both families, and an order's family is the family of its base token A.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "state")]
pub enum AnyState {
    KobAsk(AskState),
    KobBid(BidState),
    KobCondAsk(CondAskState),
    KobCondBid(CondBidState),
    KobIfdBid(IfdBidState),
    KobIfdAsk(IfdAskState),
    KobAskKron(AskState),
    KobBidKron(BidState),
    KobCondAskKron(CondAskState),
    KobCondBidKron(CondBidState),
    KobIfdBidKron(IfdBidState),
    KobIfdAskKron(IfdAskState),
    KobPair(PairState),
    KobCondPair(CondPairState),
    KobIfdPair(IfdPairState),
}

impl AnyState {
    /// The order template of this instance.
    ///
    /// A KRON kind's build follows from the token program the state pins. A program whose holders
    /// are id_type 0 takes the `*KronD0` build, whose maker deliveries are id_type 0. Any other
    /// program takes the default build, see [`TemplateId::for_token_hash`].
    pub fn template_id(&self) -> TemplateId {
        let kind = self.kind();
        match self.token_tpl_hash() {
            Some(h) if kind.family() == Family::Kron && !kind.is_pair() => kind.for_token_hash(Family::Kron, &h),
            _ => kind,
        }
    }
    /// The order kind of the variant (a KRON kind: its default build).
    fn kind(&self) -> TemplateId {
        match self {
            AnyState::KobAsk(_) => TemplateId::KobAsk,
            AnyState::KobBid(_) => TemplateId::KobBid,
            AnyState::KobCondAsk(_) => TemplateId::KobCondAsk,
            AnyState::KobCondBid(_) => TemplateId::KobCondBid,
            AnyState::KobIfdBid(_) => TemplateId::KobIfdBid,
            AnyState::KobIfdAsk(_) => TemplateId::KobIfdAsk,
            AnyState::KobAskKron(_) => TemplateId::KobAskKron,
            AnyState::KobBidKron(_) => TemplateId::KobBidKron,
            AnyState::KobCondAskKron(_) => TemplateId::KobCondAskKron,
            AnyState::KobCondBidKron(_) => TemplateId::KobCondBidKron,
            AnyState::KobIfdBidKron(_) => TemplateId::KobIfdBidKron,
            AnyState::KobIfdAskKron(_) => TemplateId::KobIfdAskKron,
            AnyState::KobPair(_) => TemplateId::KobPair,
            AnyState::KobCondPair(_) => TemplateId::KobCondPair,
            AnyState::KobIfdPair(_) => TemplateId::KobIfdPair,
        }
    }
    /// Token family of this order, for a pair order the family of its base token A.
    ///
    /// An invalid family code gives KCC-20, and [`AnyState::validate`] refuses it.
    pub fn family(&self) -> Family {
        match self.pair_tokens() {
            Some(t) => t.a.family_of().unwrap_or(Family::Kcc20),
            None => self.template_id().family(),
        }
    }
    /// The two tokens of a pair order (base A, quote B); `None` for the KAS kinds.
    pub fn pair_tokens(&self) -> Option<PairTokens> {
        match self {
            AnyState::KobPair(s) => Some(s.tokens()),
            AnyState::KobCondPair(s) => Some(s.tokens()),
            AnyState::KobIfdPair(s) => Some(s.tokens()),
            _ => None,
        }
    }
    /// True for the pair kinds.
    pub fn is_pair(&self) -> bool {
        self.template_id().is_pair()
    }
    /// The same state as the kind of `fam` (`KobBid` -> `KobBidKron` and back).
    ///
    /// A pair order is returned unchanged, because the families of its tokens are state fields.
    pub fn into_family(self, fam: Family) -> AnyState {
        let kron = fam == Family::Kron;
        match self {
            AnyState::KobAsk(s) | AnyState::KobAskKron(s) => {
                if kron {
                    AnyState::KobAskKron(s)
                } else {
                    AnyState::KobAsk(s)
                }
            }
            AnyState::KobBid(s) | AnyState::KobBidKron(s) => {
                if kron {
                    AnyState::KobBidKron(s)
                } else {
                    AnyState::KobBid(s)
                }
            }
            AnyState::KobCondAsk(s) | AnyState::KobCondAskKron(s) => {
                if kron {
                    AnyState::KobCondAskKron(s)
                } else {
                    AnyState::KobCondAsk(s)
                }
            }
            AnyState::KobCondBid(s) | AnyState::KobCondBidKron(s) => {
                if kron {
                    AnyState::KobCondBidKron(s)
                } else {
                    AnyState::KobCondBid(s)
                }
            }
            AnyState::KobIfdBid(s) | AnyState::KobIfdBidKron(s) => {
                if kron {
                    AnyState::KobIfdBidKron(s)
                } else {
                    AnyState::KobIfdBid(s)
                }
            }
            AnyState::KobIfdAsk(s) | AnyState::KobIfdAskKron(s) => {
                if kron {
                    AnyState::KobIfdAskKron(s)
                } else {
                    AnyState::KobIfdAsk(s)
                }
            }
            s @ (AnyState::KobPair(_) | AnyState::KobCondPair(_) | AnyState::KobIfdPair(_)) => s,
        }
    }
    /// Token extension commitment the state carries.
    ///
    /// Returns `None` for the kinds that carry none. A sell-first entry names it in its committed
    /// exit, see [`AnyState::custody_ext`], and a pair order names it per token.
    pub fn extension_commitment(&self) -> Option<[u8; 32]> {
        match self {
            AnyState::KobAsk(s) | AnyState::KobAskKron(s) => Some(s.extension_commitment),
            AnyState::KobCondAsk(s) | AnyState::KobCondAskKron(s) => Some(s.extension_commitment),
            AnyState::KobBid(s) | AnyState::KobBidKron(s) => Some(s.extension_commitment),
            AnyState::KobCondBid(s) | AnyState::KobCondBidKron(s) => Some(s.extension_commitment),
            AnyState::KobIfdBid(s) | AnyState::KobIfdBidKron(s) => Some(s.extension_commitment),
            _ => None,
        }
    }
    /// The KCC-20 extension commitment the order's covenant requires of its custody of `token`.
    ///
    /// It is the `extensionCommitment` of a `KobAsk` or `KobCondAsk`, and that of the committed
    /// exit of a `KobIfdAsk`, the token its exits buy back and a merge's new custody. For the pair
    /// orders it is `sExt`, `aExt` or `bExt`. Units of the token's covenant id with another
    /// commitment are another token, and the covenant refuses them as its custody. It is zero for a
    /// KRON token, which has none.
    ///
    /// Returns `None` when the order holds no custody of that token, or the state cannot name one.
    pub fn custody_ext(&self, token: [u8; 32]) -> Option<[u8; 32]> {
        if let Some(t) = self.pair_tokens() {
            return if token == t.a.cov_id {
                t.a.ext
            } else if token == t.b.cov_id {
                t.b.ext
            } else {
                None
            };
        }
        if token != self.token_cov_id() {
            return None;
        }
        match self {
            AnyState::KobAsk(s) | AnyState::KobAskKron(s) => Some(s.extension_commitment),
            AnyState::KobCondAsk(s) | AnyState::KobCondAskKron(s) => Some(s.extension_commitment),
            AnyState::KobIfdAsk(s) | AnyState::KobIfdAskKron(s) => s.exit().ok().map(|x| x.extension_commitment),
            _ => None,
        }
    }
    /// Family consistency of the state.
    ///
    /// - A KRON kind carries no extension commitment. The field must be zero, since the KRON
    ///   template has no such field.
    /// - A sell-first KRON entry commits a KRON exit.
    /// - A pair order has a valid side, valid family codes, no extension commitment for a KRON
    ///   token, two different tokens and, for an entry, a committed exit of the right width.
    pub fn validate(&self) -> Result<(), StateError> {
        if self.family() == Family::Kron && self.extension_commitment().is_some_and(|e| e != [0; 32]) {
            return Err(StateError::Codec(
                "AnyState",
                format!("{}: KRON tokens have no extension commitment (must be zero)", self.template_id().name()),
            ));
        }
        match self {
            AnyState::KobIfdBid(s) | AnyState::KobIfdBidKron(s) => {
                // The committed exit is a fixed-width span: any other length makes the state
                // unencodable.
                if s.exit_state.len() != IFD_BID_EXIT_COMMIT {
                    return Err(StateError::Codec(
                        "AnyState",
                        format!(
                            "{}: exitState must be {IFD_BID_EXIT_COMMIT} bytes, got {}",
                            self.template_id().name(),
                            s.exit_state.len()
                        ),
                    ));
                }
                // The merge amounts are computed at each side's own scale, and the covenant
                // compares the exit only to the committed bytes. An exit at another scale than its
                // entry would leak on every merge.
                if s.exit()?.scale != s.scale {
                    return Err(StateError::Codec(
                        "AnyState",
                        format!("{}: the committed exit's scale differs from the entry's", self.template_id().name()),
                    ));
                }
            }
            AnyState::KobIfdAsk(s) | AnyState::KobIfdAskKron(s) => {
                if s.exit_family()? != self.family() {
                    return Err(StateError::Codec(
                        "AnyState",
                        format!("{}: the committed exit is of the other family", self.template_id().name()),
                    ));
                }
                if s.exit()?.scale != s.scale {
                    return Err(StateError::Codec(
                        "AnyState",
                        format!("{}: the committed exit's scale differs from the entry's", self.template_id().name()),
                    ));
                }
            }
            AnyState::KobPair(s) => s.validate()?,
            AnyState::KobCondPair(s) => s.validate()?,
            AnyState::KobIfdPair(s) => s.validate()?,
            _ => {}
        }
        Ok(())
    }
    /// The encoded state, or the reason it cannot be encoded.
    ///
    /// A hostile or malformed state fails, e.g. a committed exit of the wrong width or a KRON
    /// extension commitment. Use this on any state that did not come from a builder of this crate.
    pub fn try_encode(&self) -> Result<Vec<u8>, StateError> {
        let id = self.template_id();
        match self {
            AnyState::KobAsk(s) | AnyState::KobAskKron(s) => s.try_encode_as(id),
            AnyState::KobBid(s) | AnyState::KobBidKron(s) => s.try_encode_as(id),
            AnyState::KobCondAsk(s) | AnyState::KobCondAskKron(s) => s.try_encode_as(id),
            AnyState::KobCondBid(s) | AnyState::KobCondBidKron(s) => s.try_encode_as(id),
            AnyState::KobIfdBid(s) | AnyState::KobIfdBidKron(s) => s.try_encode_as(id),
            AnyState::KobIfdAsk(s) | AnyState::KobIfdAskKron(s) => s.try_encode_as(id),
            AnyState::KobPair(s) => s.try_encode_as(id),
            AnyState::KobCondPair(s) => s.try_encode_as(id),
            AnyState::KobIfdPair(s) => s.try_encode_as(id),
        }
    }
    /// The encoded state. Panics on a state [`AnyState::try_encode`] refuses.
    ///
    /// Every state that passed [`AnyState::validate`] encodes, and all builder entry points and
    /// decoders validate.
    pub fn encode(&self) -> Vec<u8> {
        self.try_encode().unwrap_or_else(|e| panic!("{e}"))
    }
    /// Decodes the state span of an instance of the order template `id`.
    ///
    /// The state must pass [`AnyState::validate`], and `id` must be the build of the token program
    /// the state pins.
    pub fn decode(id: TemplateId, bytes: &[u8]) -> Result<AnyState, StateError> {
        let state = match id {
            TemplateId::KobAsk => AnyState::KobAsk(AskState::decode_as(id, bytes)?),
            TemplateId::KobBid => AnyState::KobBid(BidState::decode_as(id, bytes)?),
            TemplateId::KobCondAsk => AnyState::KobCondAsk(CondAskState::decode_as(id, bytes)?),
            TemplateId::KobCondBid => AnyState::KobCondBid(CondBidState::decode_as(id, bytes)?),
            TemplateId::KobIfdBid => AnyState::KobIfdBid(IfdBidState::decode_as(id, bytes)?),
            TemplateId::KobIfdAsk => AnyState::KobIfdAsk(IfdAskState::decode_as(id, bytes)?),
            TemplateId::KobAskKron => AnyState::KobAskKron(AskState::decode_as(id, bytes)?),
            TemplateId::KobBidKron => AnyState::KobBidKron(BidState::decode_as(id, bytes)?),
            TemplateId::KobCondAskKron => AnyState::KobCondAskKron(CondAskState::decode_as(id, bytes)?),
            TemplateId::KobCondBidKron => AnyState::KobCondBidKron(CondBidState::decode_as(id, bytes)?),
            TemplateId::KobIfdBidKron => AnyState::KobIfdBidKron(IfdBidState::decode_as(id, bytes)?),
            TemplateId::KobIfdAskKron => AnyState::KobIfdAskKron(IfdAskState::decode_as(id, bytes)?),
            TemplateId::KobAskKronD0 => AnyState::KobAskKron(AskState::decode_as(id, bytes)?),
            TemplateId::KobBidKronD0 => AnyState::KobBidKron(BidState::decode_as(id, bytes)?),
            TemplateId::KobCondAskKronD0 => AnyState::KobCondAskKron(CondAskState::decode_as(id, bytes)?),
            TemplateId::KobCondBidKronD0 => AnyState::KobCondBidKron(CondBidState::decode_as(id, bytes)?),
            TemplateId::KobIfdBidKronD0 => AnyState::KobIfdBidKron(IfdBidState::decode_as(id, bytes)?),
            TemplateId::KobIfdAskKronD0 => AnyState::KobIfdAskKron(IfdAskState::decode_as(id, bytes)?),
            TemplateId::KobPair => AnyState::KobPair(PairState::decode_as(id, bytes)?),
            TemplateId::KobCondPair => AnyState::KobCondPair(CondPairState::decode_as(id, bytes)?),
            TemplateId::KobIfdPair => AnyState::KobIfdPair(IfdPairState::decode_as(id, bytes)?),
            other => return Err(StateError::Codec("AnyState", format!("{} is not an order template", other.name()))),
        };
        state.validate()?;
        // The build of a KRON kind is the token program's, and both builds have the same state
        // span. An instance of one build whose state pins a program of the other is not an order
        // KOB recognises, because it would be a delivery the maker could not spend.
        if state.template_id() != id {
            return Err(StateError::Codec(
                "AnyState",
                format!(
                    "{}: the state's token program takes the {} build (maker deliveries of the program's holder owner)",
                    id.name(),
                    state.template_id().name()
                ),
            ));
        }
        Ok(state)
    }
    /// [`AnyState::decode`] of a state named by its kind, whatever the build.
    ///
    /// A `KOB1` record names the kind by family and kind code. The template is then
    /// [`AnyState::template_id`], the build of the token program the state pins.
    pub fn decode_kind(kind: TemplateId, bytes: &[u8]) -> Result<AnyState, StateError> {
        match AnyState::decode(kind, bytes) {
            Ok(s) => Ok(s),
            Err(e) => match kind.delivery0() {
                d0 if d0 != kind => AnyState::decode(d0, bytes).map_err(|_| e),
                _ => Err(e),
            },
        }
    }
    /// Redeem script of this instance under its template.
    pub fn redeem(&self) -> Vec<u8> {
        template(self.template_id()).redeem(&self.encode())
    }
    /// P2SH script public key of this instance under its template.
    pub fn spk(&self) -> ScriptPublicKey {
        template(self.template_id()).spk(&self.encode())
    }
    /// Maker key of an order.
    pub fn maker(&self) -> [u8; 32] {
        match self {
            AnyState::KobAsk(s) | AnyState::KobAskKron(s) => s.maker,
            AnyState::KobBid(s) | AnyState::KobBidKron(s) => s.maker,
            AnyState::KobCondAsk(s) | AnyState::KobCondAskKron(s) => s.maker,
            AnyState::KobCondBid(s) | AnyState::KobCondBidKron(s) => s.maker,
            AnyState::KobIfdBid(s) | AnyState::KobIfdBidKron(s) => s.maker,
            AnyState::KobIfdAsk(s) | AnyState::KobIfdAskKron(s) => s.maker,
            AnyState::KobPair(s) => s.maker,
            AnyState::KobCondPair(s) => s.maker,
            AnyState::KobIfdPair(s) => s.maker,
        }
    }
    /// True for the kinds that hold tokens in covenant-id custody.
    ///
    /// These are asks, conditional asks, sell-first entries, and every pair order but an entry with
    /// nothing in custody, see [`AnyState::custodies`].
    pub fn holds_tokens(&self) -> bool {
        matches!(
            self,
            AnyState::KobAsk(_)
                | AnyState::KobCondAsk(_)
                | AnyState::KobIfdAsk(_)
                | AnyState::KobAskKron(_)
                | AnyState::KobCondAskKron(_)
                | AnyState::KobIfdAskKron(_)
                | AnyState::KobPair(_)
                | AnyState::KobCondPair(_)
        ) || matches!(self, AnyState::KobIfdPair(s) if s.amount_left > 0 && !s.is_buy_first() || s.custody > 0)
    }
    /// The exact custodies a token-holding order holds, in record order:
    /// `(token covenant id, amount)`.
    ///
    /// - A KAS kind has one custody, `amountLeft` of its token.
    /// - `KobPair` and `KobCondPair` hold the custody of S.
    /// - A buy-first `KobIfdPair` holds the B escrow. A sell-first one holds the A custody
    ///   (`amountLeft`), then the B prefund. Each is listed only when non-zero.
    pub fn custodies(&self) -> Vec<([u8; 32], i64)> {
        match self {
            AnyState::KobPair(s) => vec![(s.s_cov_id, s.custody)],
            AnyState::KobCondPair(s) => vec![(s.s_cov_id, s.custody)],
            AnyState::KobIfdPair(s) => {
                let mut v = vec![];
                if !s.is_buy_first() && s.amount_left > 0 {
                    v.push((s.a_cov_id, s.amount_left));
                }
                if s.custody > 0 {
                    v.push((s.b_cov_id, s.custody));
                }
                v
            }
            other if other.holds_tokens() => vec![(other.token_cov_id(), other.amount_left().expect("token-holding kinds"))],
            _ => vec![],
        }
    }
    /// Token covenant id (a pair order: of its base token A).
    pub fn token_cov_id(&self) -> [u8; 32] {
        match self {
            AnyState::KobAsk(s) | AnyState::KobAskKron(s) => s.token_cov_id,
            AnyState::KobBid(s) | AnyState::KobBidKron(s) => s.token_cov_id,
            AnyState::KobCondAsk(s) | AnyState::KobCondAskKron(s) => s.token_cov_id,
            AnyState::KobCondBid(s) | AnyState::KobCondBidKron(s) => s.token_cov_id,
            AnyState::KobIfdBid(s) | AnyState::KobIfdBidKron(s) => s.token_cov_id,
            AnyState::KobIfdAsk(s) | AnyState::KobIfdAskKron(s) => s.token_cov_id,
            p => p.pair_tokens().expect("pair kind").a.cov_id,
        }
    }
    /// Token template hash of an order; every order kind pins one.
    ///
    /// For a pair order it is that of its base token A.
    pub fn token_tpl_hash(&self) -> Option<[u8; 32]> {
        Some(match self {
            AnyState::KobAsk(s) | AnyState::KobAskKron(s) => s.token_tpl_hash,
            AnyState::KobBid(s) | AnyState::KobBidKron(s) => s.token_tpl_hash,
            AnyState::KobCondAsk(s) | AnyState::KobCondAskKron(s) => s.token_tpl_hash,
            AnyState::KobCondBid(s) | AnyState::KobCondBidKron(s) => s.token_tpl_hash,
            AnyState::KobIfdBid(s) | AnyState::KobIfdBidKron(s) => s.token_tpl_hash,
            AnyState::KobIfdAsk(s) | AnyState::KobIfdAskKron(s) => s.token_tpl_hash,
            p => p.pair_tokens().expect("pair kind").a.tpl_hash,
        })
    }
    /// Token template prefix and suffix lengths the order pins (a pair order: of its base token A).
    pub fn token_tpl_lens(&self) -> (i64, i64) {
        match self {
            AnyState::KobAsk(s) | AnyState::KobAskKron(s) => (s.tpl_prefix_len, s.tpl_suffix_len),
            AnyState::KobBid(s) | AnyState::KobBidKron(s) => (s.tpl_prefix_len, s.tpl_suffix_len),
            AnyState::KobCondAsk(s) | AnyState::KobCondAskKron(s) => (s.tpl_prefix_len, s.tpl_suffix_len),
            AnyState::KobCondBid(s) | AnyState::KobCondBidKron(s) => (s.tpl_prefix_len, s.tpl_suffix_len),
            AnyState::KobIfdBid(s) | AnyState::KobIfdBidKron(s) => (s.tpl_prefix_len, s.tpl_suffix_len),
            AnyState::KobIfdAsk(s) | AnyState::KobIfdAskKron(s) => (s.tpl_prefix_len, s.tpl_suffix_len),
            p => {
                let a = p.pair_tokens().expect("pair kind").a;
                (a.prefix_len, a.suffix_len)
            }
        }
    }
    /// Base units per whole token (`scale`) of the order's base token.
    pub fn scale(&self) -> i64 {
        match self {
            AnyState::KobAsk(s) | AnyState::KobAskKron(s) => s.scale,
            AnyState::KobBid(s) | AnyState::KobBidKron(s) => s.scale,
            AnyState::KobCondAsk(s) | AnyState::KobCondAskKron(s) => s.scale,
            AnyState::KobCondBid(s) | AnyState::KobCondBidKron(s) => s.scale,
            AnyState::KobIfdBid(s) | AnyState::KobIfdBidKron(s) => s.scale,
            AnyState::KobIfdAsk(s) | AnyState::KobIfdAskKron(s) => s.scale,
            p => p.pair_tokens().expect("pair kind").a.scale,
        }
    }
    /// Smallest fill (`minFill`, base units).
    pub fn min_fill(&self) -> i64 {
        match self {
            AnyState::KobAsk(s) | AnyState::KobAskKron(s) => s.min_fill,
            AnyState::KobBid(s) | AnyState::KobBidKron(s) => s.min_fill,
            AnyState::KobCondAsk(s) | AnyState::KobCondAskKron(s) => s.min_fill,
            AnyState::KobCondBid(s) | AnyState::KobCondBidKron(s) => s.min_fill,
            AnyState::KobIfdBid(s) | AnyState::KobIfdBidKron(s) => s.min_fill,
            AnyState::KobIfdAsk(s) | AnyState::KobIfdAskKron(s) => s.min_fill,
            AnyState::KobPair(s) => s.min_fill,
            AnyState::KobCondPair(s) => s.min_fill,
            AnyState::KobIfdPair(s) => s.min_fill,
        }
    }
    /// Expiry and refund tip (every order kind has them).
    pub fn expiry(&self) -> Option<(i64, i64)> {
        Some(match self {
            AnyState::KobAsk(s) | AnyState::KobAskKron(s) => (s.expiry_daa, s.refund_tip),
            AnyState::KobBid(s) | AnyState::KobBidKron(s) => (s.expiry_daa, s.refund_tip),
            AnyState::KobCondAsk(s) | AnyState::KobCondAskKron(s) => (s.expiry_daa, s.refund_tip),
            AnyState::KobCondBid(s) | AnyState::KobCondBidKron(s) => (s.expiry_daa, s.refund_tip),
            AnyState::KobIfdBid(s) | AnyState::KobIfdBidKron(s) => (s.expiry_daa, s.refund_tip),
            AnyState::KobIfdAsk(s) | AnyState::KobIfdAskKron(s) => (s.expiry_daa, s.refund_tip),
            AnyState::KobPair(s) => (s.expiry_daa, s.refund_tip),
            AnyState::KobCondPair(s) => (s.expiry_daa, s.refund_tip),
            AnyState::KobIfdPair(s) => (s.expiry_daa, s.refund_tip),
        })
    }
    /// Base units still open (`amountLeft`; `None` for a bid, whose quantity is its budget).
    pub fn amount_left(&self) -> Option<i64> {
        Some(match self {
            AnyState::KobAsk(s) | AnyState::KobAskKron(s) => s.amount_left,
            AnyState::KobCondAsk(s) | AnyState::KobCondAskKron(s) => s.amount_left,
            AnyState::KobCondBid(s) | AnyState::KobCondBidKron(s) => s.amount_left,
            AnyState::KobIfdBid(s) | AnyState::KobIfdBidKron(s) => s.amount_left,
            AnyState::KobIfdAsk(s) | AnyState::KobIfdAskKron(s) => s.amount_left,
            AnyState::KobPair(s) => s.amount_left,
            AnyState::KobCondPair(s) => s.amount_left,
            AnyState::KobIfdPair(s) => s.amount_left,
            AnyState::KobBid(_) | AnyState::KobBidKron(_) => return None,
        })
    }
    /// The exact amount of a token-holding order's first custody.
    ///
    /// For a KAS kind it is `amountLeft`, for a pair order the first of [`AnyState::custodies`].
    pub fn custody_amount(&self) -> Option<i64> {
        if self.is_pair() {
            return self.custodies().first().map(|c| c.1);
        }
        self.holds_tokens().then(|| self.amount_left().expect("token-holding kinds have an amount"))
    }
    /// DAA score from which anyone may refund the order whose UTXO was created at `utxo_daa`.
    ///
    /// It covers the soft expiry, 90 days idle and the IOC / FOK kill. Every order kind has one.
    pub fn refund_due(&self, utxo_daa: i64) -> Option<i64> {
        Some(match self {
            AnyState::KobAsk(s) | AnyState::KobAskKron(s) => refund_due(s.expiry_daa, s.tif, s.active_from, utxo_daa),
            AnyState::KobBid(s) | AnyState::KobBidKron(s) => refund_due(s.expiry_daa, s.tif, s.active_from, utxo_daa),
            AnyState::KobCondAsk(s) | AnyState::KobCondAskKron(s) => refund_due(s.expiry_daa, TIF_GTC, s.active_from, utxo_daa),
            AnyState::KobCondBid(s) | AnyState::KobCondBidKron(s) => refund_due(s.expiry_daa, TIF_GTC, s.active_from, utxo_daa),
            AnyState::KobIfdBid(s) | AnyState::KobIfdBidKron(s) => refund_due(s.expiry_daa, TIF_GTC, s.active_from, utxo_daa),
            AnyState::KobIfdAsk(s) | AnyState::KobIfdAskKron(s) => refund_due(s.expiry_daa, TIF_GTC, s.active_from, utxo_daa),
            AnyState::KobPair(s) => refund_due(s.expiry_daa, s.tif, s.active_from, utxo_daa),
            AnyState::KobCondPair(s) => refund_due(s.expiry_daa, TIF_GTC, s.active_from, utxo_daa),
            AnyState::KobIfdPair(s) => refund_due(s.expiry_daa, TIF_GTC, s.active_from, utxo_daa),
        })
    }
    /// Keeper tip an arming or trailing update may take, for the orders with an `update` entry.
    pub fn keeper_tip(&self) -> Option<i64> {
        match self {
            AnyState::KobCondAsk(s) | AnyState::KobCondAskKron(s) => Some(s.keeper_tip),
            AnyState::KobCondBid(s) | AnyState::KobCondBidKron(s) => Some(s.keeper_tip),
            AnyState::KobIfdBid(s) | AnyState::KobIfdBidKron(s) => Some(s.keeper_tip),
            AnyState::KobIfdAsk(s) | AnyState::KobIfdAskKron(s) => Some(s.keeper_tip),
            AnyState::KobCondPair(s) => Some(s.keeper_tip),
            AnyState::KobIfdPair(s) => Some(s.keeper_tip),
            _ => None,
        }
    }
    /// The numeric gate of a placed order.
    ///
    /// Builders refuse what fails it, and an indexer may flag it.
    ///
    /// The scale must be a power of ten in `1..=10^9`. For the amount the order carries, the full
    /// fill at every rate it carries must be worth less than `2^62` ([`check_quote`]). A bid
    /// carries no amount, since its quantity is its escrow, but its rates must still fit.
    pub fn check_numbers(&self) -> Result<(), String> {
        match self {
            AnyState::KobPair(s) => return s.check_numbers(),
            AnyState::KobCondPair(s) => return s.check_numbers(),
            AnyState::KobIfdPair(s) => return s.check_numbers(),
            _ => {}
        }
        let name = self.template_id().name();
        check_scale(self.scale()).map_err(|e| format!("{name}: {e}"))?;
        let sum = |a: i64, b: i64, what: &str| a.checked_add(b).ok_or_else(|| format!("{name}: {what} overflows"));
        let check = |amount: i64, rate: i64, what: &str| check_quote(amount, rate.max(0), self.scale(), &format!("{name} {what}"));
        match self {
            AnyState::KobAsk(s) | AnyState::KobAskKron(s) => {
                check(s.amount_left, s.price.max(s.price_end), "price")?;
            }
            AnyState::KobBid(s) | AnyState::KobBidKron(s) => {
                // A bid's amount is its escrow. The rates must leave room for the KAS of one fill.
                sum(s.price_max(), s.tip, "pMax + tip")?;
            }
            AnyState::KobCondAsk(s) | AnyState::KobCondAskKron(s) => {
                check(s.amount_left, s.tp_price.max(s.stop_price), "leg price")?;
                check(s.amount_left, s.rpt_price, "rptPrice")?;
            }
            AnyState::KobCondBid(s) | AnyState::KobCondBidKron(s) => {
                check(s.amount_left, sum(s.worst(), s.tip, "worst + tip")?, "worst leg + tip")?;
                check(s.amount_left, s.rpt_price, "rptPrice")?;
                check(s.amount_left, s.rpt_pre, "rptPre")?;
            }
            AnyState::KobIfdBid(s) | AnyState::KobIfdBidKron(s) => {
                check(s.amount_left, sum(s.price, s.tip, "price + tip")?, "price + tip")?;
                if let Ok(x) = s.exit() {
                    check(s.amount_left, x.tp_price.max(x.stop_price), "exit leg price")?;
                }
            }
            AnyState::KobIfdAsk(s) | AnyState::KobIfdAskKron(s) => {
                check(s.amount_left, s.price.max(s.entry_stop), "price")?;
                check(s.amount_left, s.prefund, "prefund")?;
                if let Ok(x) = s.exit() {
                    check(s.amount_left, sum(x.worst(), x.tip, "exit worst + tip")?, "exit worst leg + tip")?;
                }
            }
            AnyState::KobPair(_) | AnyState::KobCondPair(_) | AnyState::KobIfdPair(_) => unreachable!("checked above"),
        }
        Ok(())
    }
}

/// Mutable fields of each order template and their bytecode payload windows `[start, end)`.
///
/// The windows hold the 8-byte values the covenants splice into their continuations, see
/// `docs/spec/matcher.md` §1.1. Every other byte of an order's redeem script is fixed for the life
/// of its covenant id. The KRON bid-side kinds have no extension commitment, so their windows sit
/// 33 bytes earlier. Both builds of a KRON kind have the same windows.
pub fn mutable_windows(id: TemplateId) -> &'static [(&'static str, usize, usize)] {
    match id.canonical() {
        TemplateId::KobAsk | TemplateId::KobAskKron => &[("amountLeft", 236, 244)],
        TemplateId::KobPair => &[("amountLeft", 398, 406), ("custody", 407, 415)],
        TemplateId::KobCondPair => &[("stopPrice", 416, 424), ("armed", 425, 433), ("amountLeft", 434, 442), ("custody", 443, 451)],
        TemplateId::KobIfdPair => &[("armed", 440, 448), ("amountLeft", 449, 457), ("custody", 458, 466), ("rptAmount", 467, 475)],
        TemplateId::KobCondAsk | TemplateId::KobCondAskKron => {
            &[("stopPrice", 182, 190), ("armed", 245, 253), ("amountLeft", 272, 280)]
        }
        TemplateId::KobCondBid => &[("stopPrice", 224, 232), ("amountLeft", 287, 295), ("armed", 296, 304)],
        TemplateId::KobCondBidKron => &[("stopPrice", 191, 199), ("amountLeft", 254, 262), ("armed", 263, 271)],
        TemplateId::KobIfdBid => &[("amountLeft", 161, 169), ("armed", 287, 295), ("rptAmount", 296, 304)],
        TemplateId::KobIfdBidKron => &[("amountLeft", 128, 136), ("armed", 254, 262), ("rptAmount", 263, 271)],
        TemplateId::KobIfdAsk | TemplateId::KobIfdAskKron => &[("armed", 245, 253), ("amountLeft", 254, 262), ("rptAmount", 263, 271)],
        _ => &[],
    }
}

/// Bytecode windows of the repeat fields an if-done entry writes into its exits (immutable).
pub fn repeat_window(id: TemplateId) -> Option<(usize, usize)> {
    match id.canonical() {
        TemplateId::KobCondAsk | TemplateId::KobCondAskKron => Some((280, 331)),
        TemplateId::KobCondBid => Some((322, 382)),
        TemplateId::KobCondBidKron => Some((289, 349)),
        TemplateId::KobCondPair => Some((451, 511)),
        _ => None,
    }
}

/// Encodes a state given as `{"kind": .., "state": {..}}` JSON.
pub fn encode_state_json(json: &str) -> Result<Vec<u8>, String> {
    let s: AnyState = serde_json::from_str(json).map_err(|e| e.to_string())?;
    s.validate().map_err(|e| e.to_string())?;
    s.try_encode().map_err(|e| e.to_string())
}

mod pair;
pub use pair::*;

#[cfg(test)]
mod tests;
