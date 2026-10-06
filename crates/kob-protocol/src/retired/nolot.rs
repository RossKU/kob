//! The state layout of the one retired template without lots: the protocol v3 cross limit (`KobCross`, 360-byte state,
//! committed by the v3 no-lot revision 8dd4ebf on 2026-10-05 and replaced by the pair orders in 82672b9 before any
//! deployment). Amounts are base units and the price is B base units per whole A (`scale` base units of A); one template
//! for both families of token A (`aFamily`) and of token B (`bFamily`).
//!
//! Like the lot layouts ([`super::lot`]) this type only decodes and re-encodes state spans for the maker's cancel
//! ([`crate::build::build_cancel_retired`]): the custody of A holds exactly `amountLeft`.

use crate::state::fields_struct;

fields_struct!(
    /// `KobCross` (protocol v3, no lots): cross limit selling token A for token B at a guaranteed rate, filled only next to
    /// orders that settle both of its tokens in the same transaction.
    CrossState, "KobCross (v3 no-lot layout)", {
        /// B deliveries, A refunds and returns, cancel key (x-only).
        maker: [u8; 32] => "maker",
        /// Token A (sold, escrowed).
        token_cov_id: [u8; 32] => "tokenCovId",
        token_tpl_hash: [u8; 32] => "tokenTplHash",
        tpl_prefix_len: i64 => "tplPrefixLen",
        tpl_suffix_len: i64 => "tplSuffixLen",
        /// Family code of token A: 1 KCC-20, 2 KRON.
        a_family: i64 => "aFamily",
        /// Base units of A per whole A (the price's denominator).
        scale: i64 => "scale",
        /// Least fill in base units of A (unless a fill takes everything left).
        min_fill: i64 => "minFill",
        /// Family code of token B: 1 KCC-20, 2 KRON.
        b_family: i64 => "bFamily",
        /// Token B (bought).
        b_cov_id: [u8; 32] => "bCovId",
        b_tpl_hash: [u8; 32] => "bTplHash",
        b_prefix_len: i64 => "bPrefixLen",
        b_suffix_len: i64 => "bSuffixLen",
        /// KCC-20 extension commitment of the maker's B deliveries (zero for a KRON B).
        b_ext: [u8; 32] => "bExt",
        /// The rate: B base units per whole A the maker receives at least (an auction's start).
        price: i64 => "price",
        /// Optional priority tip, sompi per whole A (KAS, prefunded in the order UTXO).
        tip: i64 => "tip",
        /// 0 GTC/GTD, 1 IOC, 2 FOK.
        tif: i64 => "tif",
        active_from: i64 => "activeFrom",
        expiry_daa: i64 => "expiryDaa",
        refund_tip: i64 => "refundTip",
        /// Sompi the order moves onto each B delivery (prefunded in the order UTXO).
        delivery_carrier: i64 => "deliveryCarrier",
        /// Auction: the worst rate the rate relaxes to (`priceEnd <= price`; 0 when not auctioning).
        price_end: i64 => "priceEnd",
        /// Auction length in DAA from `activeFrom` (0 = a fixed rate `price`).
        auction_daa: i64 => "auctionDaa",
        /// Base units of A in custody (mutable): the custody holds exactly `amountLeft`.
        amount_left: i64 => "amountLeft",
    }
);
