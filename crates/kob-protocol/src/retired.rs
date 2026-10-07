//! Retired order templates: spend-only support, so a live order placed under an older template can always be cancelled.
//!
//! A template change (any byte of an order covenant) gives every order placed afterwards a new script, and every order placed
//! before keeps the OLD one until it is spent. The builders, the indexer and the wallet know only the pinned templates, so
//! without this module the live orders of a retired template (their KAS carriers and their custody tokens) could be ended
//! only by special tooling: the covenant still lets the maker `cancel`, but nothing here could build that spend.
//!
//! Policy (`docs/spec/template-retirement.md`): no order template is retired without
//!
//! 1. its committed artifact kept under `contracts/retired/<Kind>-<hash8>.json` (byte-identical to the artifact that was
//!    pinned, reviewed through `contracts/SHA256SUMS`) and listed in `SOURCES` with the kind it predates;
//! 2. a decoder from its state span ([`decode`], into the lot layouts of [`lot`]) and the maker's cancel
//!    ([`crate::build::build_cancel_retired`]): custody, strays and the carrier back to the maker, signed by the maker key;
//! 3. the indexer's retired layout row (`kob-executor` `indexer::layouts`) so record logs stay readable.
//!
//! Retired templates are spend-only: nothing builds an order, a fill, an amend or a refund for them (a keeper or an old
//! matcher may still settle them on chain; this crate does not). Every retired template but one is a LOT template
//! (protocol v2.3 to v2.6: amounts in lots of `lotUnits x unit` base units), so their states have their own types
//! ([`lot::LotState`]); the one without lots is the protocol v3 cross limit ([`nolot::CrossState`], below), and
//! [`decode_any`] / [`RetiredState`] read both:
//!
//! * the protocol v2.6 lot templates pinned until the v3 no-lot revision (36d569c, retired 2026-10-05 by 8dd4ebf), every
//!   kind of both families (`KobCrossKron` is now the KRON family of the one `KobCross` kind): the layouts the payload
//!   records of versions 2 and 3 describe ([`payload_layout`]);
//! * the cross limits before the pair-market auction (a 333-byte state without `bLotEnd` and `auctionDaa`: b75b3e5 and
//!   c8f3b01, retired 2026-09-30 and 2026-10-01): they read as an order without an auction (`bLotEnd = bLot`,
//!   `auctionDaa = 0`);
//! * the order templates pinned just before cost pass 1 (a7072a1, 2026-10-01: smaller code, the same state layouts), all
//!   kinds of both families, and the four if-done entries before 2026-10-02 (69c9014): only the code changed;
//! * the receipt-era templates (protocol v2.3 to v2.5, before 2a94c3c, 2026-09-30), the testnet-10 deployment build of the
//!   R_ID-dependent KCC-20 conditional and if-done templates included: the conditional and if-done kinds with the receipt
//!   minimum `minRcptUnits` where the touch trigger has `minTouchUnits` (`RENAMED`).
//!
//! * the protocol v3 cross limit (`KobCross`, 360-byte state without lots, one template for token A of both families by
//!   `aFamily`: committed by the v3 no-lot revision 8dd4ebf on 2026-10-05, replaced by the pair orders in 82672b9). It was
//!   never deployed (no live order is known); it is kept for safety, its cancel built like every other.
//! * the protocol v3 sell-first entries `KobIfdAsk` / `KobIfdAskKron` before their refund required tokens held (retired
//!   2026-10-06; the refund of an empty repeating entry now requires tokens held). Only their
//!   code changed: their states are today's ([`Retired::is_current_layout`], read as [`RetiredState::Current`]).
//!
//! A booked exit of a retired entry is an order of the conditional template that entry inlines, retired as well (its
//! cancel only). The merge back into a retired entry is never built.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use silverscript_abi::ArtifactValue;

use crate::artifacts::{parse_artifact, template_from_artifact, Template, TemplateId};
use crate::error::{invalid, Result};
use crate::family::Family;
use crate::json::to_hex;
use crate::state::{decode_fields, encode_fields, AnyState, FieldMap};

pub mod lot;
pub mod nolot;

use lot::LotState;

/// One retired order template.
pub struct Retired {
    /// The order kind it is an older template of (KCC-20 or KRON variant; `KobCross` for the cross limits of both
    /// families).
    pub kind: TemplateId,
    /// The family of the tokens it escrows (a cross limit: of its token A; the protocol v3 cross limit, of either family by
    /// its `aFamily`, is listed as KCC-20: [`Retired::serves`]).
    pub family: Family,
    /// The retired artifact (`id` is [`Self::kind`]; prefix, suffix, state length and hash of the OLD template).
    pub template: Template,
    /// What it was and when it was retired.
    pub note: &'static str,
}

impl Retired {
    /// Hex of the retired template hash.
    pub fn hash_hex(&self) -> String {
        to_hex(&self.template.hash)
    }
    /// The kind's name in its family as it was pinned (`KobAskKron`, `KobCrossKron`, ...).
    pub fn kind_name(&self) -> String {
        self.family.kind_name(self.kind.base().name())
    }
    /// Whether this is the protocol v3 cross limit (no lots, token A of either family by `aFamily`): its state is a
    /// [`nolot::CrossState`] ([`decode_any`]), never a [`LotState`].
    pub fn is_no_lot(&self) -> bool {
        self.template.contract().runtime_state.fields.iter().any(|f| f.name == "aFamily")
    }
    /// Whether this template has TODAY's state layout of its kind (the same fields, types and length as the pinned template:
    /// only its code changed): its state is today's state type ([`RetiredState::Current`], [`decode_any`]), never a [`LotState`].
    pub fn is_current_layout(&self) -> bool {
        crate::artifacts::try_template(self.kind).is_some_and(|p| {
            p.state_len == self.template.state_len && p.contract().runtime_state == self.template.contract().runtime_state
        })
    }
    /// Whether an order of this template can escrow a token of `family` (its own family; the v3 cross limit: either).
    pub fn serves(&self, family: Family) -> bool {
        self.is_no_lot() || self.family == family
    }
}

/// The retired templates this build can spend: (kind, family, artifact json, pinned hash, note). The first six keep their
/// place: the first `KobCross` is the 333-byte cross limit and the first `KobIfdAsk` the entry retired on 2026-10-02; the
/// last fourteen are the protocol v2.6 lot templates of the payload records of versions 2 and 3 (the protocol v3 cross
/// limit comes just before them).
const SOURCES: &[(TemplateId, Family, &str, &str, &str)] = &[
    (
        TemplateId::KobCross,
        Family::Kcc20,
        include_str!("../../../contracts/retired/KobCross-70b1dcd3.json"),
        "70b1dcd3e5f2c3821f031d5346680d0772fb37851760aea8fa841e80424a92c7",
        "cross limit (v2.6, from c8f3b01 2026-09-30) before the pair-market auction; 333-byte state; TN10 soak 5e4c00e; retired 2026-10-01 (58cc732)",
    ),
    (
        TemplateId::KobCross,
        Family::Kron,
        include_str!("../../../contracts/retired/KobCrossKron-7371a59d.json"),
        "7371a59d8d5502089e7dbdff1d2f78b52028d58a70d2c037759895f384578f87",
        "KRON cross limit (v2.6, from c8f3b01 2026-09-30) before the pair-market auction; 333-byte state; pinned by the TN10 build 5e4c00e; retired 2026-10-01 (58cc732)",
    ),
    (
        TemplateId::KobIfdBid,
        Family::Kcc20,
        include_str!("../../../contracts/retired/KobIfdBid-35ed6bed.json"),
        "35ed6bed435f75faaac2f2c7f843312af9d5a4e5a6d404f34ad1ac0690feebc7",
        "buy-first entry before update required lots and the stop below the limit; same state; TN10 soak ae87d77; retired 2026-10-02 (69c9014)",
    ),
    (
        TemplateId::KobIfdAsk,
        Family::Kcc20,
        include_str!("../../../contracts/retired/KobIfdAsk-789ddaa1.json"),
        "789ddaa134f29a1f5fe23387963e05c51d29c737d2d44a196823a5623df3dad3",
        "sell-first entry before update required lots and the stop above the limit; same state; TN10 soak ae87d77; retired 2026-10-02 (69c9014)",
    ),
    (
        TemplateId::KobIfdBidKron,
        Family::Kron,
        include_str!("../../../contracts/retired/KobIfdBidKron-d16e03ff.json"),
        "d16e03fff6199106e85790c7fcc962269f0f2019521507f720b063841b2429a6",
        "KRON buy-first entry before update required lots and the stop below the limit; same state; pinned by the TN10 build ae87d77; retired 2026-10-02 (69c9014)",
    ),
    (
        TemplateId::KobIfdAskKron,
        Family::Kron,
        include_str!("../../../contracts/retired/KobIfdAskKron-6d19a13d.json"),
        "6d19a13d97d88187f48b3fac6a6cd6bf0f8d50f05f64c4b2069237fed90459ac",
        "KRON sell-first entry before update required lots and the stop above the limit; same state; pinned by the TN10 build ae87d77; retired 2026-10-02 (69c9014)",
    ),
    // ---- protocol v2.6 before cost pass 1 (a7072a1, 2026-10-01: every order template 6-15 % smaller, same state layouts);
    // the TN10 soak 5e4c00e, 84d8efd, eaa93db (2026-10-01)
    (
        TemplateId::KobAsk,
        Family::Kcc20,
        include_str!("../../../contracts/retired/KobAsk-ccc62fa3.json"),
        "ccc62fa3730ada83b7b0e1bedcef4fc9635742a5a1972cf918be6639cf73ed02",
        "limit sell (v2.4 M6 to v2.6, from 2a94c3c 2026-09-30) before cost pass 1; same state; TN10 soak 5e4c00e to eaa93db; retired 2026-10-01 (a7072a1)",
    ),
    (
        TemplateId::KobAskKron,
        Family::Kron,
        include_str!("../../../contracts/retired/KobAskKron-452ec1f8.json"),
        "452ec1f8ea9f4debbe8182809ed945b83c6e55572675ad64caffb9f1972054ab",
        "KRON limit sell (v2.4 M6 to v2.6, from 2a94c3c 2026-09-30) before cost pass 1; same state; pinned by the TN10 builds 5e4c00e to eaa93db; retired 2026-10-01 (a7072a1)",
    ),
    (
        TemplateId::KobBid,
        Family::Kcc20,
        include_str!("../../../contracts/retired/KobBid-39af759e.json"),
        "39af759ef52f23a265efb6ea9c7b857c8a777940dae5aa65b8c7ae4e5e360d09",
        "limit buy (v2.3 to v2.6, from a85c793 2026-09-29) before cost pass 1; same state; real-wallet TN10 runs 2026-09-29, TN10 soak 5e8a249 to eaa93db (the 6 unlocated KAS-only bids of the 10-01 flood); retired 2026-10-01 (a7072a1)",
    ),
    (
        TemplateId::KobBidKron,
        Family::Kron,
        include_str!("../../../contracts/retired/KobBidKron-87c84a09.json"),
        "87c84a09979ee4b097ea0b7c2ef1bec48d0d765c2b820136278c17f75f9ccf96",
        "KRON limit buy (v2.3 to v2.6, from 119f76b 2026-09-29) before cost pass 1; same state; pinned by the TN10 builds 5e8a249 to eaa93db; retired 2026-10-01 (a7072a1)",
    ),
    (
        TemplateId::KobCondAsk,
        Family::Kcc20,
        include_str!("../../../contracts/retired/KobCondAsk-d17183a8.json"),
        "d17183a89c58a7eef3f0bbd0f501f0161721a1a15565bf860defe1fdecd7c991",
        "conditional sell (v2.6 touch trigger, from d849ef1 2026-09-30) before cost pass 1; same state; TN10 soak 5e4c00e to eaa93db; retired 2026-10-01 (a7072a1)",
    ),
    (
        TemplateId::KobCondAskKron,
        Family::Kron,
        include_str!("../../../contracts/retired/KobCondAskKron-a2f64f94.json"),
        "a2f64f94a9e1e4e86b2fd97c2b97ef92d0398597d32b5fe181318df44e573128",
        "KRON conditional sell (v2.6 touch trigger, from d849ef1 2026-09-30) before cost pass 1; same state; pinned by the TN10 builds 5e4c00e to eaa93db; retired 2026-10-01 (a7072a1)",
    ),
    (
        TemplateId::KobCondBid,
        Family::Kcc20,
        include_str!("../../../contracts/retired/KobCondBid-f959c978.json"),
        "f959c978dde45ad2ff23498a742ca489617cea411cb6d933264e47b2d2c26ed6",
        "conditional buy (v2.6 touch trigger, from d849ef1 2026-09-30) before cost pass 1; same state; TN10 soak 5e4c00e to eaa93db; retired 2026-10-01 (a7072a1)",
    ),
    (
        TemplateId::KobCondBidKron,
        Family::Kron,
        include_str!("../../../contracts/retired/KobCondBidKron-339a1bb1.json"),
        "339a1bb18e291f1a94bd0faafa3ece05d0bdbd9ee84d370830df6fe8f97cff45",
        "KRON conditional buy (v2.6 touch trigger, from d849ef1 2026-09-30) before cost pass 1; same state; pinned by the TN10 builds 5e4c00e to eaa93db; retired 2026-10-01 (a7072a1)",
    ),
    (
        TemplateId::KobIfdAsk,
        Family::Kcc20,
        include_str!("../../../contracts/retired/KobIfdAsk-8afde064.json"),
        "8afde0646e060be0efd60c263aa0d719106046aa95dcc8a4aa79c625f69b2e93",
        "sell-first entry (v2.6 touch trigger, from d849ef1 2026-09-30) before cost pass 1; same state; TN10 soak 5e4c00e to eaa93db; retired 2026-10-01 (a7072a1)",
    ),
    (
        TemplateId::KobIfdAskKron,
        Family::Kron,
        include_str!("../../../contracts/retired/KobIfdAskKron-3c73e5c4.json"),
        "3c73e5c4e0a4dd1278e7cbec7586ff7fe6eaa8fcad19af3a111e43b0f19bcbc8",
        "KRON sell-first entry (v2.6 touch trigger, from d849ef1 2026-09-30) before cost pass 1; same state; pinned by the TN10 builds 5e4c00e to eaa93db; retired 2026-10-01 (a7072a1)",
    ),
    (
        TemplateId::KobIfdBid,
        Family::Kcc20,
        include_str!("../../../contracts/retired/KobIfdBid-824b42ee.json"),
        "824b42ee47df65e236fef29896225c9a39cdd4153fc8698957f6c826fbcb79ca",
        "buy-first entry (v2.6 touch trigger, from d849ef1 2026-09-30) before cost pass 1; same state; TN10 soak 5e4c00e to eaa93db; retired 2026-10-01 (a7072a1)",
    ),
    (
        TemplateId::KobIfdBidKron,
        Family::Kron,
        include_str!("../../../contracts/retired/KobIfdBidKron-6af8728d.json"),
        "6af8728dab4e5daf75d57f05c2cc3bf5e1c9d94b6ee639046854ef02d6f9bc62",
        "KRON buy-first entry (v2.6 touch trigger, from d849ef1 2026-09-30) before cost pass 1; same state; pinned by the TN10 builds 5e4c00e to eaa93db; retired 2026-10-01 (a7072a1)",
    ),
    (
        TemplateId::KobCross,
        Family::Kcc20,
        include_str!("../../../contracts/retired/KobCross-b23ae732.json"),
        "b23ae732e42b52bf04dc9fde31a1c2d6601cede9c3a1151d102b9933ee32ec4b",
        "cross limit with the pair-market auction (58cc732, 2026-10-01) before cost pass 1; same state; TN10 soak 84d8efd, eaa93db; retired 2026-10-01 (a7072a1)",
    ),
    (
        TemplateId::KobCross,
        Family::Kron,
        include_str!("../../../contracts/retired/KobCrossKron-87a01b1d.json"),
        "87a01b1dcbb079d4bd9811789cdee6195b9e1ca596853dd8ece09a3359d5eca0",
        "KRON cross limit with the pair-market auction (58cc732, 2026-10-01) before cost pass 1; same state; pinned by the TN10 builds 84d8efd, eaa93db; retired 2026-10-01 (a7072a1)",
    ),
    // ---- the first cross limit (b75b3e5, 2026-09-30), replaced the same day (c8f3b01): the 333-byte layout of 70b1dcd3
    (
        TemplateId::KobCross,
        Family::Kcc20,
        include_str!("../../../contracts/retired/KobCross-0b146748.json"),
        "0b1467488070f986ade9237c8af63b09404054c7f9ae91b3d60ba18e2a152929",
        "first cross limit (b75b3e5, 2026-09-30) before a cross limit with A = B could be refunded; 333-byte state (no auction); retired 2026-09-30 (c8f3b01)",
    ),
    (
        TemplateId::KobCross,
        Family::Kron,
        include_str!("../../../contracts/retired/KobCrossKron-d1d8d498.json"),
        "d1d8d49890ff9325523f616b810912e386639a355415b10bc62b70625d6196bc",
        "first KRON cross limit (b75b3e5, 2026-09-30) before a cross limit with A = B could be refunded; 333-byte state (no auction); retired 2026-09-30 (c8f3b01)",
    ),
    // ---- protocol v2.3 to v2.5, the trade-receipt era (before 2a94c3c, 2026-09-30): the real-wallet TN10 runs of 2026-09-29
    // (reference build) and the first TN10 soak 5e8a249 (deploy-tn10: the R_ID-dependent KCC-20 conditional and if-done
    // templates of contracts/deploy/testnet-10, every other kind the reference build)
    (
        TemplateId::KobAsk,
        Family::Kcc20,
        include_str!("../../../contracts/retired/KobAsk-21cf26fc.json"),
        "21cf26fc19afe61423645f5a80bbef0255a5438df15f6cd753d89c440edc7b2d",
        "limit sell (v2.3 to v2.5, from a85c793 2026-09-29); same state; real-wallet TN10 runs 2026-09-29, TN10 soak 5e8a249; retired 2026-09-30 (2a94c3c, v2.4 M6 fixes: positional IOC return)",
    ),
    (
        TemplateId::KobAskKron,
        Family::Kron,
        include_str!("../../../contracts/retired/KobAskKron-301774bd.json"),
        "301774bd22a192e52ac565b08e2c9488896348aa1eaa8a8e94e460d439f5f82f",
        "KRON limit sell (v2.3 to v2.5, from 119f76b 2026-09-29); same state; pinned by the TN10 build 5e8a249; retired 2026-09-30 (2a94c3c, v2.4 M6 fixes)",
    ),
    (
        TemplateId::KobCondAsk,
        Family::Kcc20,
        include_str!("../../../contracts/retired/KobCondAsk-92d36ff2.json"),
        "92d36ff2ecad7a04679b41de616c56cf8c633d8179a4acedec9fff8eae82ae28",
        "conditional sell of the receipt era (v2.4/v2.5, 3826d9f 2026-09-29), reference build (placeholder R_ID); state as today's with minRcptUnits where minTouchUnits is; real-wallet TN10 runs 2026-09-29 (OCO); retired 2026-09-30 (2a94c3c)",
    ),
    (
        TemplateId::KobCondAskKron,
        Family::Kron,
        include_str!("../../../contracts/retired/KobCondAskKron-69591752.json"),
        "695917526f63e76cfbcc45f65e82f0a606699427115cb1adf5519f99a625b3d3",
        "KRON conditional sell of the receipt era (v2.4/v2.5, 119f76b 2026-09-29), placeholder R_ID; state as today's with minRcptUnits where minTouchUnits is; pinned by the TN10 build 5e8a249; retired 2026-09-30 (2a94c3c)",
    ),
    (
        TemplateId::KobCondBid,
        Family::Kcc20,
        include_str!("../../../contracts/retired/KobCondBid-5ed4931a.json"),
        "5ed4931a49d7370dc2c77512f3fb7866dc5051014493f306b0b3d80b6ac25196",
        "conditional buy of the receipt era (v2.4/v2.5, 3826d9f 2026-09-29), reference build (placeholder R_ID); state as today's with minRcptUnits where minTouchUnits is; pinned by the real-wallet TN10 build 2026-09-29; retired 2026-09-30 (2a94c3c)",
    ),
    (
        TemplateId::KobCondBidKron,
        Family::Kron,
        include_str!("../../../contracts/retired/KobCondBidKron-5c110649.json"),
        "5c1106498d6572d683f7d75afca22654aa650dc366733fb7e80d070d60ad7ee9",
        "KRON conditional buy of the receipt era (v2.4/v2.5, 119f76b 2026-09-29), placeholder R_ID; state as today's with minRcptUnits where minTouchUnits is; pinned by the TN10 build 5e8a249; retired 2026-09-30 (2a94c3c)",
    ),
    (
        TemplateId::KobIfdAsk,
        Family::Kcc20,
        include_str!("../../../contracts/retired/KobIfdAsk-526b6b77.json"),
        "526b6b77322c1e620d51dacc7c31678d8e3f2ec28d11077a4bdf02d272185df5",
        "sell-first entry of the receipt era (v2.4/v2.5, 3826d9f 2026-09-29), reference build (placeholder R_ID); state as today's with minRcptUnits where minTouchUnits is; pinned by the real-wallet TN10 build 2026-09-29; retired 2026-09-30 (2a94c3c)",
    ),
    (
        TemplateId::KobIfdAskKron,
        Family::Kron,
        include_str!("../../../contracts/retired/KobIfdAskKron-de56abde.json"),
        "de56abde8b6bfa99c2eb32589443907fb4bfd15fd9564756f991cb4badd7fe98",
        "KRON sell-first entry of the receipt era (v2.4/v2.5, ada6d8d 2026-09-29), placeholder R_ID; state as today's with minRcptUnits where minTouchUnits is; pinned by the TN10 build 5e8a249; retired 2026-09-30 (2a94c3c)",
    ),
    (
        TemplateId::KobIfdBid,
        Family::Kcc20,
        include_str!("../../../contracts/retired/KobIfdBid-c3c9e206.json"),
        "c3c9e2067fe67ee932d2fa848c827e2907fa6c4ddb8237e4f42b775c0a62579a",
        "buy-first entry of the receipt era (v2.4/v2.5, 3826d9f 2026-09-29), reference build (placeholder R_ID); state as today's with minRcptUnits where minTouchUnits is; real-wallet TN10 runs 2026-09-29 (IFD); retired 2026-09-30 (2a94c3c)",
    ),
    (
        TemplateId::KobIfdBidKron,
        Family::Kron,
        include_str!("../../../contracts/retired/KobIfdBidKron-0784282f.json"),
        "0784282f33c59af7eee628c90c63430639817f246faaa9aba80a53ed47e08703",
        "KRON buy-first entry of the receipt era (v2.4/v2.5, 119f76b 2026-09-29), placeholder R_ID; state as today's with minRcptUnits where minTouchUnits is; pinned by the TN10 build 5e8a249; retired 2026-09-30 (2a94c3c)",
    ),
    (
        TemplateId::KobCondAsk,
        Family::Kcc20,
        include_str!("../../../contracts/retired/KobCondAsk-9fa11943.json"),
        "9fa11943c280056239e61f3690d734adbfd5667e8ec8b1721fe6bcb794f8fe9f",
        "conditional sell, testnet-10 deployment build of the receipt era (6db1f08 2026-09-29, R_ID of receipt genesis 64ae62c1); state as today's with minRcptUnits where minTouchUnits is; TN10 soak 5e8a249; retired 2026-09-30 (2a94c3c)",
    ),
    (
        TemplateId::KobCondBid,
        Family::Kcc20,
        include_str!("../../../contracts/retired/KobCondBid-040f5ac8.json"),
        "040f5ac8e23b4360a7956028fa05d3f370110cbe1b13fe87c6213317d17e32ad",
        "conditional buy, testnet-10 deployment build of the receipt era (6db1f08 2026-09-29, R_ID of receipt genesis 64ae62c1); state as today's with minRcptUnits where minTouchUnits is; TN10 soak 5e8a249; retired 2026-09-30 (2a94c3c)",
    ),
    (
        TemplateId::KobIfdAsk,
        Family::Kcc20,
        include_str!("../../../contracts/retired/KobIfdAsk-dd3054d0.json"),
        "dd3054d09dfedab7244735bfd2f5e8ba46c0be8cbce01f9463033c94c8f13dab",
        "sell-first entry, testnet-10 deployment build of the receipt era (6db1f08 2026-09-29, R_ID of receipt genesis 64ae62c1); state as today's with minRcptUnits where minTouchUnits is; TN10 soak 5e8a249; retired 2026-09-30 (2a94c3c)",
    ),
    (
        TemplateId::KobIfdBid,
        Family::Kcc20,
        include_str!("../../../contracts/retired/KobIfdBid-ef5a575f.json"),
        "ef5a575fbf1003bf3779be7efa169c795dc39a03348123091a0ed319fc42eeda",
        "buy-first entry, testnet-10 deployment build of the receipt era (6db1f08 2026-09-29, R_ID of receipt genesis 64ae62c1); state as today's with minRcptUnits where minTouchUnits is; TN10 soak 5e8a249; retired 2026-09-30 (2a94c3c)",
    ),
    // ---- protocol v3 templates with TODAY's state layout, retired because only their code changed (decoded with today's
    // state types: `RetiredState::Current`).
    (
        TemplateId::KobIfdAsk,
        Family::Kcc20,
        include_str!("../../../contracts/retired/KobIfdAsk-189b9c32.json"),
        "189b9c3297cac33c0de6b5defefcbee5deee42306e8aa72b4615d8f2d4515a17",
        "sell-first entry (protocol v3) whose refund (settle n = 0) did not require tokens held (an empty repeating entry accepted a refund); today's state layout; retired 2026-10-06",
    ),
    (
        TemplateId::KobIfdAskKron,
        Family::Kron,
        include_str!("../../../contracts/retired/KobIfdAskKron-85d87838.json"),
        "85d8783813f861d16d2679d13fd062d81ce572f7fe4efa111943bd5a6a6b4ac8",
        "KRON sell-first entry (protocol v3) whose refund (settle n = 0) did not require tokens held (see KobIfdAsk 189b9c32); today's state layout; retired 2026-10-06",
    ),
    // ---- the protocol v3 cross limit (no lots; token A of either family by aFamily), committed by the v3 no-lot revision
    // (8dd4ebf, 2026-10-05) and replaced by the pair orders (82672b9) before any deployment.
    (
        TemplateId::KobCross,
        Family::Kcc20,
        include_str!("../../../contracts/retired/KobCross-ea23f1fe.json"),
        "ea23f1fec9719d56a31f4b54b15cc3e6a286af0384b7e7769f571dfd39a74b89",
        "cross limit (protocol v3, no lots, token A of either family by aFamily): committed by the v3 no-lot revision (8dd4ebf 2026-10-05), replaced by the pair orders (82672b9) before any deployment",
    ),
    // ---- protocol v2.6 lot templates, pinned from 2026-10-01 (cost pass 1, a7072a1; the if-done entries from 69c9014) to
    // 36d569c: retired 2026-10-05 by the v3 no-lot revision (8dd4ebf). Payload versions 2 and 3 name these layouts.
    (
        TemplateId::KobAsk,
        Family::Kcc20,
        include_str!("../../../contracts/retired/KobAsk-66853f16.json"),
        "66853f1603149ff6371a06655ac58fa9f775ad80fce0141876c31dc41364c3c2",
        "limit sell: protocol v2.6 lot templates, retired 2026-10-05 by the v3 no-lot revision",
    ),
    (
        TemplateId::KobBid,
        Family::Kcc20,
        include_str!("../../../contracts/retired/KobBid-20858974.json"),
        "208589746fefe2ea9082cef18b75b0e5a31f9e1b059421dc0caeef96b3d55728",
        "limit buy: protocol v2.6 lot templates, retired 2026-10-05 by the v3 no-lot revision",
    ),
    (
        TemplateId::KobCondAsk,
        Family::Kcc20,
        include_str!("../../../contracts/retired/KobCondAsk-fe740586.json"),
        "fe74058698d86e6c22aae188441e853398f770a71c53f8a2b3d7229821e7d3ff",
        "conditional sell: protocol v2.6 lot templates, retired 2026-10-05 by the v3 no-lot revision",
    ),
    (
        TemplateId::KobCondBid,
        Family::Kcc20,
        include_str!("../../../contracts/retired/KobCondBid-3a5f1e22.json"),
        "3a5f1e2227c115360a2ef4143f597d31c6e1e5e00f44244ab22851f5458680d3",
        "conditional buy: protocol v2.6 lot templates, retired 2026-10-05 by the v3 no-lot revision",
    ),
    (
        TemplateId::KobIfdBid,
        Family::Kcc20,
        include_str!("../../../contracts/retired/KobIfdBid-ca757658.json"),
        "ca757658e4872b64efb9a25c39aeefb68a2622dcc93b04426261377e2bb4d199",
        "buy-first entry: protocol v2.6 lot templates, retired 2026-10-05 by the v3 no-lot revision",
    ),
    (
        TemplateId::KobIfdAsk,
        Family::Kcc20,
        include_str!("../../../contracts/retired/KobIfdAsk-138d4fd8.json"),
        "138d4fd83941a072ceaaa3e1ce95f565f2a8ec21e529d43287245839b17a79e8",
        "sell-first entry: protocol v2.6 lot templates, retired 2026-10-05 by the v3 no-lot revision",
    ),
    (
        TemplateId::KobCross,
        Family::Kcc20,
        include_str!("../../../contracts/retired/KobCross-692cfdd0.json"),
        "692cfdd07dae71749db2d0ad8249eb70c7476e9ef67a4f8f4c09c409e991b752",
        "cross limit (KCC-20 custody of A): protocol v2.6 lot templates, retired 2026-10-05 by the v3 no-lot revision",
    ),
    (
        TemplateId::KobAskKron,
        Family::Kron,
        include_str!("../../../contracts/retired/KobAskKron-3389eeb2.json"),
        "3389eeb2e971cacbbbbda193582d38db5f1bf56acff55478d9afd4a4d956b7d7",
        "KRON limit sell: protocol v2.6 lot templates, retired 2026-10-05 by the v3 no-lot revision",
    ),
    (
        TemplateId::KobBidKron,
        Family::Kron,
        include_str!("../../../contracts/retired/KobBidKron-a1161255.json"),
        "a116125531ae2aaa1218a836c6891c11174faa15427da34672c7b11d19f0151e",
        "KRON limit buy: protocol v2.6 lot templates, retired 2026-10-05 by the v3 no-lot revision",
    ),
    (
        TemplateId::KobCondAskKron,
        Family::Kron,
        include_str!("../../../contracts/retired/KobCondAskKron-219c53b2.json"),
        "219c53b28b33579f699fdb93cff1c7c3f2bba2ee4033a1a22cfde6980807343e",
        "KRON conditional sell: protocol v2.6 lot templates, retired 2026-10-05 by the v3 no-lot revision",
    ),
    (
        TemplateId::KobCondBidKron,
        Family::Kron,
        include_str!("../../../contracts/retired/KobCondBidKron-0cc7eeb9.json"),
        "0cc7eeb9b02f90f0fa031acb48826dba4b35ce614ea6e659ef4772ebe38f9cb4",
        "KRON conditional buy: protocol v2.6 lot templates, retired 2026-10-05 by the v3 no-lot revision",
    ),
    (
        TemplateId::KobIfdBidKron,
        Family::Kron,
        include_str!("../../../contracts/retired/KobIfdBidKron-354fb5de.json"),
        "354fb5de47e5f9141da32cb3d8f282323200d915b9bacbec126a370479a8c232",
        "KRON buy-first entry: protocol v2.6 lot templates, retired 2026-10-05 by the v3 no-lot revision",
    ),
    (
        TemplateId::KobIfdAskKron,
        Family::Kron,
        include_str!("../../../contracts/retired/KobIfdAskKron-fd13762a.json"),
        "fd13762a9157ff89b996f38bd9641937ab983a7c7375537b356747a972043540",
        "KRON sell-first entry: protocol v2.6 lot templates, retired 2026-10-05 by the v3 no-lot revision",
    ),
    (
        TemplateId::KobCross,
        Family::Kron,
        include_str!("../../../contracts/retired/KobCrossKron-ef254324.json"),
        "ef254324cc270929a9b5b70116af1739983faa3dd4bb7278dc5b01044d631354",
        "cross limit (KRON custody of A, KobCrossKron): protocol v2.6 lot templates, retired 2026-10-05 by the v3 no-lot revision",
    ),
];

/// Every retired template this build can spend (checked on first use: the artifact's recorded hash equals its bytecode's and
/// the pinned one above, and it is not a pinned template of today).
pub fn retired() -> &'static [Retired] {
    static R: OnceLock<Vec<Retired>> = OnceLock::new();
    R.get_or_init(|| {
        SOURCES
            .iter()
            .map(|(kind, family, json, hash, note)| {
                let t = template_from_artifact(*kind, parse_artifact(json).expect("retired artifact json"))
                    .expect("retired artifact hash matches its bytecode");
                assert_eq!(to_hex(&t.hash), *hash, "retired {} artifact is not the pinned retired hash", kind.name());
                assert!(crate::artifacts::try_template(*kind).is_none_or(|p| p.hash != t.hash), "a pinned template is not retired");
                Retired { kind: *kind, family: *family, template: t, note }
            })
            .collect()
    })
}

/// The retired template with this hash, if this build can spend it.
pub fn by_hash(hash: &[u8; 32]) -> Option<&'static Retired> {
    retired().iter().find(|r| &r.template.hash == hash)
}

/// The retired template of a redeem script and its state span, if the script is an instance of one.
pub fn identify(redeem: &[u8]) -> Option<(&'static Retired, &[u8])> {
    retired().iter().find_map(|r| r.template.state_of(redeem).map(|s| (r, s)))
}

/// Number of protocol v2.6 lot templates at the end of `SOURCES` (the layouts of payload versions 2 and 3).
const PAYLOAD_LAYOUTS: usize = 14;

/// The protocol v2.6 lot template of an order kind code in a family: the layout a version-3 payload record of that kind
/// describes (version 3 names no template hash; its records were written for these templates and for the if-done entries
/// retired on 2026-10-02, which have the same state layout: [`same_layout`]).
pub fn payload_layout(family: Family, kind_code: u8) -> Option<&'static Retired> {
    let all = retired();
    all[all.len() - PAYLOAD_LAYOUTS..].iter().find(|r| r.family == family && r.kind.kind_code() == Some(kind_code))
}

/// Whether two retired templates have the same state layout (the same fields in the same order and types, under the same
/// or the [`RENAMED`] names): a state span of one is a state span of the other.
pub fn same_layout(a: &Retired, b: &Retired) -> bool {
    let (fa, fb) = (&a.template.contract().runtime_state.fields, &b.template.contract().runtime_state.fields);
    a.template.state_len == b.template.state_len
        && fa.len() == fb.len()
        && fa.iter().zip(fb).all(|(x, y)| x.ty == y.ty && renamed(&x.name) == renamed(&y.name))
}

/// State fields renamed since a retired template, at the same position with the same type: (old name, the v2.6 name). The
/// receipt era (protocol v2.3 to v2.5) had `minRcptUnits`, the least receipt size that arms a stop, where the touch trigger
/// (v2.6) has `minTouchUnits`, the least fill that touches it: the old value is carried in that field (only a stop
/// reads it, never a cancel).
pub const RENAMED: [(&str, &str); 1] = [("minRcptUnits", "minTouchUnits")];

fn renamed(name: &str) -> &str {
    RENAMED.iter().find(|(old, _)| *old == name).map(|(_, new)| *new).unwrap_or(name)
}

/// The fields the v2.6 cross limit's pair-market auction added (`bLotEnd`, `auctionDaa`): a cross limit of a template
/// without them reads with `bLotEnd = bLot`, `auctionDaa = 0` (an order without an auction).
const CROSS_AUCTION_FIELDS: [&str; 2] = ["bLotEnd", "auctionDaa"];

/// The state of an order of a retired template, in the v2.6 lot layout of its kind ([`LotState`]). Canonical spans only
/// (re-encoding gives the same bytes).
pub fn decode(r: &Retired, state: &[u8]) -> Result<LotState> {
    if r.is_no_lot() {
        return invalid("the protocol v3 cross limit has no lot layout (read it with retired::decode_any)");
    }
    if r.is_current_layout() {
        return invalid("this retired template has today's state layout, no lot layout (read it with retired::decode_any)");
    }
    let raw = decode_fields("retired order", &r.template, r.family == Family::Kron, state)?;
    let mut m: BTreeMap<String, ArtifactValue> = raw.into_iter().map(|(k, v)| (renamed(&k).to_string(), v)).collect();
    if r.kind == TemplateId::KobCross && !m.contains_key(CROSS_AUCTION_FIELDS[0]) {
        let b_lot = m.get("bLot").cloned().unwrap_or(ArtifactValue::Int(0));
        m.insert(CROSS_AUCTION_FIELDS[0].to_string(), b_lot);
        m.insert(CROSS_AUCTION_FIELDS[1].to_string(), ArtifactValue::Int(0));
    }
    let s = match r.kind.base() {
        TemplateId::KobAsk => LotState::KobAsk(lot::AskState::from_values(&m)?),
        TemplateId::KobBid => LotState::KobBid(lot::BidState::from_values(&m)?),
        TemplateId::KobCondAsk => LotState::KobCondAsk(lot::CondAskState::from_values(&m)?),
        TemplateId::KobCondBid => LotState::KobCondBid(lot::CondBidState::from_values(&m)?),
        TemplateId::KobIfdBid => LotState::KobIfdBid(lot::IfdBidState::from_values(&m)?),
        TemplateId::KobIfdAsk => LotState::KobIfdAsk(lot::IfdAskState::from_values(&m)?),
        TemplateId::KobCross => LotState::KobCross(lot::CrossState::from_values(&m)?),
        other => return invalid(format!("no decoder for a retired {} template", other.name())),
    };
    if encode(r, &s)? != state {
        return invalid("non-canonical state of the retired template");
    }
    Ok(s)
}

/// The retired template's state span of a decoded state ([`decode`]'s inverse).
pub fn encode(r: &Retired, s: &LotState) -> Result<Vec<u8>> {
    if r.is_no_lot() || r.is_current_layout() {
        return invalid("this retired template has no lot layout (write it with retired::encode_any)");
    }
    if s.kind_name() != r.kind.base().name() {
        return invalid(format!("a {} state is not a {} state", s.kind_name(), r.kind_name()));
    }
    let values = match s {
        LotState::KobAsk(x) => x.to_values(),
        LotState::KobBid(x) => x.to_values(),
        LotState::KobCondAsk(x) => x.to_values(),
        LotState::KobCondBid(x) => x.to_values(),
        LotState::KobIfdBid(x) => x.to_values(),
        LotState::KobIfdAsk(x) => x.to_values(),
        LotState::KobCross(x) => x.to_values(),
    };
    let fields = &r.template.contract().runtime_state.fields;
    if let LotState::KobCross(x) = s {
        if !fields.iter().any(|f| f.name == CROSS_AUCTION_FIELDS[0]) && (x.b_lot_end != x.b_lot || x.auction_daa != 0) {
            return invalid("a cross limit of this retired template has no auction (bLotEnd = bLot, auctionDaa = 0)");
        }
    }
    // a KRON bid-side state has no extension commitment: a lot state carrying a non-zero one is not a KRON order
    if let Some(ArtifactValue::Bytes(b)) = values.get(crate::state::EXT_KEY) {
        if !fields.iter().any(|f| f.name == crate::state::EXT_KEY) && b.iter().any(|x| *x != 0) {
            return invalid("KRON tokens have no extension commitment (must be zero)");
        }
    }
    // exactly the artifact's own fields, under its own names (the receipt era's minRcptUnits)
    let mut v = BTreeMap::new();
    for f in fields {
        let val = values
            .get(renamed(&f.name))
            .cloned()
            .ok_or_else(|| crate::Error::Invalid(format!("the retired state lacks the field {}", f.name)))?;
        v.insert(f.name.clone(), val);
    }
    Ok(encode_fields("retired order", &r.template, false, v)?)
}

/// A cross limit's token B: (`bFamily` code, covenant id, program template hash, prefix and suffix lengths).
pub type CrossTokenB = (i64, [u8; 32], [u8; 32], i64, i64);

/// The state of an order of any retired template: a lot layout ([`LotState`]), the protocol v3 cross limit without lots
/// ([`nolot::CrossState`]), or today's state type of its kind for a template with today's layout
/// ([`Retired::is_current_layout`]: only its code changed).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "layout", content = "order", rename_all = "camelCase")]
pub enum RetiredState {
    Lot(LotState),
    NoLotCross(nolot::CrossState),
    Current(AnyState),
}

impl RetiredState {
    /// Name of the kind (`KobAsk`, ...; the KCC-20 name in both families).
    pub fn kind_name(&self) -> &'static str {
        match self {
            RetiredState::Lot(l) => l.kind_name(),
            RetiredState::NoLotCross(_) => "KobCross",
            RetiredState::Current(a) => a.template_id().base().name(),
        }
    }
    /// Maker key (payouts, refunds, the cancel signature).
    pub fn maker(&self) -> [u8; 32] {
        match self {
            RetiredState::Lot(l) => l.maker(),
            RetiredState::NoLotCross(x) => x.maker,
            RetiredState::Current(a) => a.maker(),
        }
    }
    /// The order's token (a cross limit: token A): covenant id, program template hash, prefix and suffix lengths.
    pub fn token(&self) -> ([u8; 32], [u8; 32], i64, i64) {
        match self {
            RetiredState::Lot(l) => l.token(),
            RetiredState::NoLotCross(x) => (x.token_cov_id, x.token_tpl_hash, x.tpl_prefix_len, x.tpl_suffix_len),
            RetiredState::Current(a) => {
                let (pre, suf) = a.token_tpl_lens();
                (a.token_cov_id(), a.token_tpl_hash().unwrap_or([0; 32]), pre, suf)
            }
        }
    }
    /// True for the kinds that hold tokens in covenant-id custody.
    pub fn holds_tokens(&self) -> bool {
        match self {
            RetiredState::Lot(l) => l.holds_tokens(),
            RetiredState::NoLotCross(_) => true,
            RetiredState::Current(a) => a.holds_tokens(),
        }
    }
    /// The exact custody amount of a token-holding order (lots: `lotsLeft x lotUnits x unit`; the v3 cross limit and today's
    /// layouts: `amountLeft`), `None` for the other kinds and for an amount no custody can hold.
    pub fn custody_amount(&self) -> Option<i64> {
        match self {
            RetiredState::Lot(l) => l.custody_amount(),
            RetiredState::NoLotCross(x) => (x.amount_left >= 0).then_some(x.amount_left),
            RetiredState::Current(a) => a.custody_amount().filter(|n| *n >= 0),
        }
    }
    /// The family of token A of a v3 cross limit (`aFamily`; `None` for a lot template, whose family is its template's,
    /// and for an invalid code).
    pub fn a_family(&self) -> Option<Family> {
        match self {
            RetiredState::Lot(_) | RetiredState::Current(_) => None,
            RetiredState::NoLotCross(x) => crate::state::family_of_code(x.a_family),
        }
    }
    /// Token B of a cross limit: (`bFamily` code, covenant id, program template hash, prefix and suffix lengths).
    pub fn cross_b(&self) -> Option<CrossTokenB> {
        match self {
            RetiredState::Lot(l) => l.as_cross().map(|x| (x.b_family, x.b_cov_id, x.b_tpl_hash, x.b_prefix_len, x.b_suffix_len)),
            RetiredState::NoLotCross(x) => Some((x.b_family, x.b_cov_id, x.b_tpl_hash, x.b_prefix_len, x.b_suffix_len)),
            RetiredState::Current(_) => None,
        }
    }
}

/// The state of an order of any retired template ([`decode`] for the lot templates; the protocol v3 cross limit in its own
/// layout; a template with today's layout: today's state type of its kind). Canonical spans only.
pub fn decode_any(r: &Retired, state: &[u8]) -> Result<RetiredState> {
    if r.is_current_layout() {
        let a = AnyState::decode_unvalidated(r.kind, state).map_err(|e| crate::Error::Invalid(e.to_string()))?;
        let s = RetiredState::Current(a);
        if encode_any(r, &s)? != state {
            return invalid("non-canonical state of the retired template");
        }
        return Ok(s);
    }
    if !r.is_no_lot() {
        return decode(r, state).map(RetiredState::Lot);
    }
    let m = decode_fields("retired order", &r.template, false, state)?;
    let s = RetiredState::NoLotCross(nolot::CrossState::from_values(&m)?);
    if encode_any(r, &s)? != state {
        return invalid("non-canonical state of the retired template");
    }
    Ok(s)
}

/// The retired template's state span of a decoded state ([`decode_any`]'s inverse).
pub fn encode_any(r: &Retired, s: &RetiredState) -> Result<Vec<u8>> {
    if let RetiredState::Current(a) = s {
        if !r.is_current_layout() || a.template_id() != r.kind {
            return invalid(format!(
                "a {} state is not a state of the retired {} template {}",
                a.template_id().name(),
                r.kind_name(),
                &r.hash_hex()[..8]
            ));
        }
        return a.try_encode().map_err(|e| crate::Error::Invalid(e.to_string()));
    }
    match (r.is_no_lot(), s) {
        (false, RetiredState::Lot(l)) => encode(r, l),
        (true, RetiredState::NoLotCross(x)) => {
            let values = x.to_values();
            let mut v = BTreeMap::new();
            for f in &r.template.contract().runtime_state.fields {
                let val = values
                    .get(&f.name)
                    .cloned()
                    .ok_or_else(|| crate::Error::Invalid(format!("the retired state lacks the field {}", f.name)))?;
                v.insert(f.name.clone(), val);
            }
            Ok(encode_fields("retired order", &r.template, false, v)?)
        }
        _ => invalid(format!(
            "a {} state is not a state of the retired {} template {}",
            s.kind_name(),
            r.kind_name(),
            &r.hash_hex()[..8]
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(r: &Retired) -> Vec<u8> {
        let t = &r.template;
        t.contract().compiled.bytecode[t.prefix.len()..t.prefix.len() + t.state_len].to_vec()
    }

    #[test]
    fn retired_templates_load_decode_and_differ_from_the_pinned_ones() {
        assert_eq!(retired().len(), SOURCES.len());
        assert_eq!(retired().len(), 53);
        for (i, r) in retired().iter().enumerate() {
            assert!(by_hash(&r.template.hash).is_some());
            assert!(retired()[..i].iter().all(|o| o.template.hash != r.template.hash), "listed once: {}", r.note);
            if let Some(p) = crate::artifacts::try_template(r.kind) {
                // a different layout, or today's layout under different code
                assert_ne!(r.template.hash, p.hash, "{}", r.note);
                if !r.is_current_layout() {
                    assert_ne!(r.template.contract().runtime_state, p.contract().runtime_state, "{}", r.note);
                }
            }
            // the compiled example instance decodes into its lot layout and re-encodes to the same bytes
            let st = span(r);
            let s = decode_any(r, &st).unwrap_or_else(|e| panic!("{}: {e}", r.note));
            assert_eq!(encode_any(r, &s).unwrap(), st, "{}", r.note);
            let lot = !r.is_no_lot() && !r.is_current_layout();
            assert_eq!(decode(r, &st).is_ok(), lot, "{}: the lot reader reads every lot template only", r.note);
            let found = identify(&r.template.contract().compiled.bytecode).map(|(x, _)| x.template.hash);
            assert_eq!(found, Some(r.template.hash), "{}", r.note);
            // the maker's cancel exists and takes only the maker's signature
            let cancel = r.template.contract().entries.get("cancel").expect("the maker's cancel exists");
            assert_eq!(cancel.params.len(), 1, "{}", r.note);
            assert_eq!(cancel.params[0].ty, silverscript_abi::TypeArtifact::Sig, "{}", r.note);
            if r.template.state_len == 333 {
                // the cross limits before the pair-market auction: no auction fields
                assert_eq!(r.kind, TemplateId::KobCross, "{}", r.note);
                let RetiredState::Lot(s) = s else { panic!("{}: a lot template", r.note) };
                let x = s.as_cross().unwrap();
                assert_eq!((x.b_lot_end, x.auction_daa), (x.b_lot, 0));
                let auctioned = LotState::KobCross(lot::CrossState { auction_daa: 5, ..x.clone() });
                assert!(encode(r, &auctioned).is_err());
            }
        }
    }

    #[test]
    fn payload_layouts_are_the_v2_6_lot_templates_of_both_families() {
        for fam in [Family::Kcc20, Family::Kron] {
            for code in [1u8, 2, 3, 4, 5, 6, 8] {
                let r = payload_layout(fam, code).unwrap_or_else(|| panic!("{fam:?} {code}"));
                assert_eq!((r.family, r.kind.kind_code()), (fam, Some(code)));
                assert!(r.note.contains("v3 no-lot revision"), "{}", r.note);
            }
            assert!(payload_layout(fam, 7).is_none());
        }
        // the if-done entries retired on 2026-10-02 share the v2.6 layout of their kind
        let early: Vec<_> = retired().iter().filter(|r| r.note.contains("(69c9014)")).collect();
        assert_eq!(early.len(), 4);
        for r in early {
            let v26 = payload_layout(r.family, r.kind.kind_code().unwrap()).unwrap();
            assert!(same_layout(r, v26), "{}", r.note);
        }
    }

    #[test]
    fn receipt_era_templates_read_the_renamed_receipt_minimum() {
        let receipt_era: Vec<_> =
            retired().iter().filter(|r| r.template.contract().runtime_state.fields.iter().any(|f| f.name == "minRcptUnits")).collect();
        // conditional and if-done kinds, both families, plus the testnet-10 deployment builds of the KCC-20 four
        assert_eq!(receipt_era.len(), 12);
        for r in receipt_era {
            let v26 = payload_layout(r.family, r.kind.kind_code().unwrap()).unwrap();
            assert!(same_layout(r, v26), "{}", r.note);
            let s = decode(r, &span(r)).unwrap();
            assert!(serde_json::to_string(&s).unwrap().contains("minTouchUnits"));
        }
    }

    #[test]
    fn lot_custody_is_lots_times_lot_units_times_unit() {
        for r in retired() {
            let s = decode_any(r, &span(r)).unwrap();
            assert_eq!(s.custody_amount().is_some(), s.holds_tokens(), "{}", r.note);
            if let RetiredState::Lot(LotState::KobAsk(a)) = &s {
                assert_eq!(s.custody_amount(), Some(a.lots_left * a.lot_units * a.unit));
            }
        }
    }

    /// The protocol v3 cross limit (360 bytes, no lots): one template for token A of both families, its custody exactly
    /// `amountLeft` base units, its state only in its own layout (a lot state never encodes under it).
    #[test]
    fn the_v3_cross_limit_without_lots_is_retired() {
        let v3: Vec<_> = retired().iter().filter(|r| r.is_no_lot()).collect();
        assert_eq!(v3.len(), 1);
        let r = v3[0];
        assert_eq!((r.kind, r.template.state_len, &r.hash_hex()[..8]), (TemplateId::KobCross, 360, "ea23f1fe"));
        assert!(r.serves(Family::Kcc20) && r.serves(Family::Kron));
        let RetiredState::NoLotCross(x) = decode_any(r, &span(r)).unwrap() else { panic!("the no-lot layout") };
        let kron = nolot::CrossState { a_family: 2, amount_left: 777, ..x.clone() };
        let s = RetiredState::NoLotCross(kron);
        let bytes = encode_any(r, &s).unwrap();
        assert_eq!(decode_any(r, &bytes).unwrap(), s);
        assert_eq!((s.custody_amount(), s.a_family()), (Some(777), Some(Family::Kron)));
        // a lot state never encodes under it, nor its state under a lot template
        let lot = retired().iter().find(|o| o.kind == TemplateId::KobCross && !o.is_no_lot()).unwrap();
        let lot_state = decode_any(lot, &span(lot)).unwrap();
        assert!(encode_any(r, &lot_state).is_err());
        assert!(encode_any(lot, &s).is_err());
        // the v2.6 payload layouts are still the last fourteen
        assert!(retired()[retired().len() - 14..].iter().all(|o| !o.is_no_lot()));
    }

    /// The protocol v3 sell-first entries retired 2026-10-06 (refund without tokens held): today's layout, read with today's
    /// state type, cancel only; the pinned templates of their kinds are different code with the same state.
    #[test]
    fn the_v3_sell_first_entries_are_retired_with_todays_layout() {
        let cur: Vec<_> = retired().iter().filter(|r| r.is_current_layout()).collect();
        let names: Vec<_> = cur.iter().map(|r| (r.kind, r.hash_hex()[..8].to_string())).collect();
        assert_eq!(names, vec![(TemplateId::KobIfdAsk, "189b9c32".into()), (TemplateId::KobIfdAskKron, "85d87838".into())]);
        for r in cur {
            let p = crate::artifacts::template(r.kind);
            assert_eq!((r.template.state_len, &r.template.contract().runtime_state), (p.state_len, &p.contract().runtime_state));
            let s = decode_any(r, &span(r)).unwrap();
            let RetiredState::Current(a) = &s else { panic!("{}: today's layout", r.note) };
            assert_eq!(a.template_id(), r.kind);
            assert!(s.holds_tokens());
            assert_eq!(s.custody_amount(), a.amount_left());
            // a state of another kind never encodes under it, nor a lot state
            let other = retired().iter().find(|o| !o.is_current_layout() && !o.is_no_lot() && o.family == r.family).unwrap();
            assert!(encode_any(r, &decode_any(other, &span(other)).unwrap()).is_err());
            assert!(encode_any(other, &s).is_err());
            assert!(decode(r, &span(r)).is_err());
        }
    }
}
