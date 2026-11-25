//! The cancel-only table: order templates this build does not pin, kept so their makers can cancel.
//!
//! See `docs/spec/template-retirement.md`.
//!
//! A contract change gives the orders placed afterwards a new template, while every order placed
//! before keeps its script until it is spent. Such an order is unknown to this build
//! ([`crate::artifacts::PINNED`] does not list its template). Nothing fills it, arms or trails its
//! stop, merges its repeat or refunds it as a keeper. No book lists it, and no placement record of
//! it is accepted.
//!
//! This table adds exactly one thing: the maker's `cancel(sig)` of such an order. Its custody, its
//! strays and its carrier go back to the maker ([`crate::build::CancelOrder::retired`],
//! [`crate::build::CancelItem::retired`]). The signature plan of that input is
//! [`crate::tx::SigPlan::RetiredCancel`]. No other entry of a retired template is ever built or
//! signed.
//!
//! Every retired template here shares a state layout and the `cancel(sig)` entry of today's kind of
//! the same name. A template retired before the delivery block (bid-delivery-spec §2) has the
//! kind's [`StateLayout::Legacy`] layout: the state without the block, which decodes with every
//! block field zero. That is checked on first use: the state span is a layout of the kind
//! ([`crate::state::layout_of_len`]), the legacy one for a kind with a delivery block, and the
//! ABI and dispatch tag of the cancel entry are today's. So its terms decode with today's codec
//! ([`crate::state::AnyState::decode`]), and only its bytecode differs.
//!
//! The bytes are the compiled bytecode of the retired artifact, taken from the last commit that
//! carried it (`data/retired/<Kind>.<hash prefix>.bin`, `scripts/extract-retired.mjs`). Each
//! template hash is recomputed from them (`template_hash(prefix, suffix)`), so a changed file
//! fails to load. The script engine tests
//! (`crates/kob-tests/tests/kob/retired_cancel_tests.rs`) validate the cancel of every retired
//! template and show that its other entries are not reachable through this table.

use std::sync::OnceLock;

use kaspa_consensus_core::tx::ScriptPublicKey;
use kaspa_txscript::pay_to_script_hash_script;
use serde::Serialize;
use silverscript_abi::{ArtifactValue, TypeArtifact};

use crate::artifacts::{template, try_template, TemplateId, PINNED};
use crate::json::{hex32, to_hex};
use crate::script::entry_sigscript;
use crate::state::{layout_of_len, state_len_in, AnyState, StateLayout};
use crate::{Error, Result};

/// The only entry a retired template is ever spent through.
pub const CANCEL_ENTRY: &str = "cancel";
/// Dispatch tag of `cancel(sig)` in every retired template (and in today's order kinds).
pub const CANCEL_TAG: [u8; 4] = [0xa0, 0x89, 0x31, 0x09];

/// The change that retired a template.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Retirement {
    /// A terminal bid fill returns the spare KAS to its maker. Retired by `acf39cf`; the last
    /// release carrying these is `59d9ea30`.
    BidRefund,
    /// Trigger evidence refuses a negative interval (`docs/security/AUDIT-2026-10.md`, section 12).
    /// Retired by `da35841`; the last commit carrying these is `60a4d11`.
    F1,
    /// The buy-side kinds carry the delivery block (`docs/design/bid-delivery-spec.md`), and the
    /// kinds that embed them changed with them. Retired by `0714451`; the last commit
    /// carrying these is `d12b28e1`.
    BidDelivery,
    /// A bid fill below `minFill` takes all the buying power left: the KRON kinds that changed
    /// with `KobBidKron` (their KCC-20 twins are [`Retirement::BidDelivery`]). Retired by
    /// `71a6c32`; the last commit carrying these is `1652c9ac`.
    BidMinFill,
    /// The pre-mainnet re-pin (TPLGEN, 2026-11-25): every order kind, for the native-lane pin, the state-span check
    /// (audit R10CC-1), the zero borrow guard, refund `amountLeft > 0` and the `MAX_STOP` trail. Retired by `9dc6f0f`;
    /// the last commit carrying these is `428653bf3`. Their state has the delivery block where the kind has one.
    PreMainnet,
}

impl Retirement {
    /// The commit that retired the templates.
    pub fn retired_by(self) -> &'static str {
        match self {
            Retirement::BidRefund => "acf39cf",
            Retirement::F1 => "da35841",
            Retirement::BidDelivery => "0714451",
            Retirement::BidMinFill => "71a6c32",
            Retirement::PreMainnet => "9dc6f0f",
        }
    }

    /// The state layout the orders of this retirement were placed with, for a kind `kind`.
    ///
    /// Every retirement so far is of templates placed before the delivery block (bid-delivery-spec
    /// §2): [`StateLayout::Legacy`]. A retirement of templates that already carry the block (the
    /// generation retired by the pre-mainnet re-pin) returns the kind's layout when it was retired,
    /// so each row's span is read in the layout its orders have, not one fixed layout (TPLGEN step 1).
    pub fn layout(self, kind: TemplateId) -> StateLayout {
        match self {
            Retirement::BidRefund | Retirement::F1 | Retirement::BidDelivery | Retirement::BidMinFill => StateLayout::Legacy,
            // the state of these kinds is unchanged by the re-pin: today's layout
            Retirement::PreMainnet => crate::state::pinned_layout(kind),
        }
    }

    /// The last commit that carries the templates (their artifacts and sources).
    pub fn last_release(self) -> &'static str {
        match self {
            Retirement::BidRefund => "59d9ea30",
            Retirement::F1 => "60a4d11",
            Retirement::BidDelivery => "d12b28e1",
            Retirement::BidMinFill => "1652c9ac",
            Retirement::PreMainnet => "428653bf3",
        }
    }
}

/// One retired order template: today's kind whose state layout and cancel entry it shares, and its
/// own bytecode.
#[derive(Clone, Debug)]
pub struct RetiredTemplate {
    /// Today's order kind of the same name (state codec, cancel entry ABI, signing-plan roles).
    pub kind: TemplateId,
    /// blake3 template hash (`template_hash(prefix, suffix)`), the hash its orders were placed
    /// under.
    pub hash: [u8; 32],
    /// The change that retired it.
    pub retirement: Retirement,
    /// Bytecode before the state span.
    pub prefix: Vec<u8>,
    /// Bytecode after the state span.
    pub suffix: Vec<u8>,
    /// Length of the state span: that of `kind` in [`RetiredTemplate::layout`].
    pub state_len: usize,
    /// The state layout of `kind` its orders were placed with ([`StateLayout::Legacy`] for every
    /// template retired so far).
    pub layout: StateLayout,
}

impl RetiredTemplate {
    /// Redeem script of an order with this state span: `prefix ‖ state ‖ suffix`.
    pub fn redeem(&self, state: &[u8]) -> Result<Vec<u8>> {
        if state.len() != self.state_len {
            return Err(Error::Invalid(format!("{}: state span must be {} bytes, not {}", self.label(), self.state_len, state.len())));
        }
        Ok([self.prefix.as_slice(), state, self.suffix.as_slice()].concat())
    }

    /// P2SH script public key of an order with this state span.
    pub fn spk(&self, state: &[u8]) -> Result<ScriptPublicKey> {
        Ok(pay_to_script_hash_script(&self.redeem(state)?))
    }

    /// State span of a redeem script, if it is an instance of this template.
    pub fn state_of<'a>(&self, redeem: &'a [u8]) -> Option<&'a [u8]> {
        if redeem.len() != self.prefix.len() + self.state_len + self.suffix.len()
            || !redeem.starts_with(&self.prefix)
            || !redeem.ends_with(&self.suffix)
        {
            return None;
        }
        Some(&redeem[self.prefix.len()..self.prefix.len() + self.state_len])
    }

    /// The state span of an order of this template: `s` (of `kind`) encoded in its layout.
    ///
    /// A state with a delivery block field set has no legacy span, and fails.
    pub fn encode_state(&self, s: &AnyState) -> Result<Vec<u8>> {
        if s.template_id() != self.kind {
            return Err(Error::Invalid(format!(
                "{}: a {} state is not a {} state",
                self.label(),
                s.template_id().name(),
                self.kind.name()
            )));
        }
        s.try_encode_layout(self.layout).map_err(|e| Error::Invalid(format!("{}: {e}", self.label())))
    }

    /// Hex of the template hash.
    pub fn hash_hex(&self) -> String {
        to_hex(&self.hash)
    }

    /// `KobBid b995661f`: the kind and the template hash prefix, as
    /// `docs/spec/template-retirement.md` names it.
    pub fn label(&self) -> String {
        format!("{} {}", self.kind.name(), &self.hash_hex()[..8])
    }

    /// The signature script of the maker's `cancel(sig)` of an order with this state span.
    ///
    /// It is `push(sig) ‖ push(CANCEL_TAG) ‖ push(redeem)`, ABI-encoded with today's `cancel` entry
    /// of `kind`. The ABI is the same, and that is checked on load.
    pub fn cancel_sigscript(&self, state: &[u8], sig: &[u8]) -> Result<Vec<u8>> {
        let redeem = self.redeem(state)?;
        entry_sigscript(template(self.kind), &redeem, CANCEL_ENTRY, &[ArtifactValue::Bytes(sig.to_vec())]).map_err(Error::Invalid)
    }
}

/// What a client lists of a retired template (kob-wasm `retiredTemplates`, `kob registry`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RetiredInfo {
    /// Today's kind (`KobBid`).
    pub kind: String,
    /// Template hash.
    #[serde(with = "crate::json::field")]
    pub hash: [u8; 32],
    /// `KobBid b995661f`.
    pub label: String,
    /// The commit that retired the template.
    pub retired_by: String,
    /// The last commit that carries the template.
    pub last_release: String,
    /// Always `["cancel"]`: the only entry this build spends it through.
    pub entries: Vec<String>,
}

macro_rules! retired_table {
    ($(($kind:ident, $ret:ident, $hash:literal)),* $(,)?) => {
        const TABLE: &[(TemplateId, Retirement, &str, &[u8])] = &[
            $((
                TemplateId::$kind,
                Retirement::$ret,
                $hash,
                include_bytes!(concat!("../data/retired/", stringify!($kind), ".", $hash, ".bin")),
            ),)*
        ];
    };
}

// Each row is today's kind, the retirement, and the template hash prefix of the file name. The
// full hashes are below.
retired_table![
    (KobBid, BidRefund, "b995661f"),
    (KobCondAsk, BidRefund, "770314aa"),
    (KobCondBid, BidRefund, "26042aff"),
    (KobIfdBid, BidRefund, "5740a65d"),
    (KobIfdAsk, BidRefund, "afb74446"),
    (KobCondPair, BidRefund, "699c30cc"),
    (KobIfdPair, BidRefund, "bb9937a1"),
    (KobBidKron, BidRefund, "6ec1a3dd"),
    (KobCondAskKron, BidRefund, "63263983"),
    (KobCondBidKron, BidRefund, "f3181d85"),
    (KobIfdBidKron, BidRefund, "b240e0ac"),
    (KobIfdAskKron, BidRefund, "f7e04243"),
    (KobBidKronD0, BidRefund, "436fbbbb"),
    (KobCondAskKronD0, BidRefund, "2a0e231b"),
    (KobCondBidKronD0, BidRefund, "9d6203f3"),
    (KobIfdBidKronD0, BidRefund, "02ceb778"),
    (KobIfdAskKronD0, BidRefund, "70b05a68"),
    (KobCondAsk, F1, "02a34664"),
    (KobCondBid, F1, "28d702e2"),
    (KobIfdBid, F1, "cc77a57d"),
    (KobIfdAsk, F1, "eb2c6e97"),
    (KobCondPair, F1, "3cae3ae3"),
    (KobIfdPair, F1, "670bbecf"),
    (KobCondAskKron, F1, "27fa0ee2"),
    (KobCondBidKron, F1, "296a2ffb"),
    (KobIfdBidKron, F1, "b705e2c3"),
    (KobIfdAskKron, F1, "4797dc30"),
    (KobCondAskKronD0, F1, "e334e85e"),
    (KobCondBidKronD0, F1, "f47fe24d"),
    (KobIfdBidKronD0, F1, "c9cbbbdb"),
    (KobIfdAskKronD0, F1, "4ee20455"),
    (KobBid, BidDelivery, "dfcd78f1"),
    (KobCondAsk, BidDelivery, "23431719"),
    (KobCondBid, BidDelivery, "07952223"),
    (KobIfdBid, BidDelivery, "956f81e8"),
    (KobIfdAsk, BidDelivery, "2328ea8f"),
    (KobPair, BidDelivery, "c95c9234"),
    (KobCondPair, BidDelivery, "0d9918cc"),
    (KobIfdPair, BidDelivery, "43c721c5"),
    (KobBidKron, BidMinFill, "7cb4f6ab"),
    (KobCondAskKron, BidMinFill, "e9930d36"),
    (KobCondBidKron, BidMinFill, "8aa82c81"),
    (KobIfdBidKron, BidMinFill, "644eeb9c"),
    (KobIfdAskKron, BidMinFill, "2557e020"),
    (KobBidKronD0, BidMinFill, "bdf2d399"),
    (KobCondAskKronD0, BidMinFill, "b66e1ca0"),
    (KobCondBidKronD0, BidMinFill, "9e2bac11"),
    (KobIfdBidKronD0, BidMinFill, "c4742e78"),
    (KobIfdAskKronD0, BidMinFill, "7ae3d8b7"),
    (KobAsk, PreMainnet, "070bb3b2"),
    (KobBid, PreMainnet, "2f6079ae"),
    (KobCondAsk, PreMainnet, "209cf0da"),
    (KobCondBid, PreMainnet, "aad44664"),
    (KobIfdBid, PreMainnet, "3ea83cf4"),
    (KobIfdAsk, PreMainnet, "812905a9"),
    (KobPair, PreMainnet, "d3d775de"),
    (KobCondPair, PreMainnet, "46fd3606"),
    (KobIfdPair, PreMainnet, "59bc8aec"),
    (KobCurveAsk, PreMainnet, "1e9bee3b"),
    (KobAskKron, PreMainnet, "f7274b79"),
    (KobBidKron, PreMainnet, "4dc2420d"),
    (KobCondAskKron, PreMainnet, "75ad2407"),
    (KobCondBidKron, PreMainnet, "401b2e29"),
    (KobIfdBidKron, PreMainnet, "10c1a47f"),
    (KobIfdAskKron, PreMainnet, "8e030d24"),
    (KobAskKronD0, PreMainnet, "f43ae142"),
    (KobBidKronD0, PreMainnet, "8a7ee676"),
    (KobCondAskKronD0, PreMainnet, "b2b6fe24"),
    (KobCondBidKronD0, PreMainnet, "82fce430"),
    (KobIfdBidKronD0, PreMainnet, "ebc5bff8"),
    (KobIfdAskKronD0, PreMainnet, "daf934fd"),
];

/// The full template hashes of [`TABLE`], in its order: the hashes the retired orders were placed
/// under. Each is recomputed from its bytes on load.
const HASHES: [&str; 71] = [
    "b995661f8b17c7c558b85975e361c26b00a2d57214cf631083a464b835e54fa3",
    "770314aa3391b20333c98037343f538f1903002362f31bd599abfca16aa67247",
    "26042aff0dc58c2cbaa0efd166c38944942d75524cc55033dde80502c4558166",
    "5740a65dfdcb27685a10f951de281f556c4d2de86a7d3d101412c9c2244fd026",
    "afb74446c24e7e8e3d0587bffd659afdc2e9c516d633d02a0a6308589b4d9934",
    "699c30cc6355c82fee1bacd6fd5f7239a80d98fb222699ef8407dd671ddc923f",
    "bb9937a199d1d7e1b1f62c2eb55418840670901a6042110129ad4368aaf6b8a4",
    "6ec1a3dd4a287b73295a08db5f75fedcac4966539d793e9d1a659711ad888efc",
    "6326398362aef7a510dfafb97cbc5918b54c3489e9a9f114f51bc369205778c6",
    "f3181d85a5ecfa64066eb0f72ee2f32f984ac2ce4023ab59bfe7c16b89d0a975",
    "b240e0ac9296b59db386aaf6024ff67c686964b2ea5288a3a24de8eebcc56179",
    "f7e04243d361652339900cea285e2ec9edc31220780bf224b8dfeb0efed0c089",
    "436fbbbb676f05d6402745298976aa6f1fe59faa1d7eb354060ba87e97966531",
    "2a0e231b97a3e0d7ee2ffd0c037a6535bb8d93957ced62eefda407ddd30ca0b8",
    "9d6203f395a6f7aa4e9cff03b0a79eb6ab8d0a39a18979b5aa332f41a799031e",
    "02ceb7781d99d861d214aa02a9f59571caa5bc351d04fa79148b50d0058954fc",
    "70b05a68bc1eca88c0231170a90d5f2b8e867e92b1e196b9288a481902fe64e7",
    "02a346649370ad91eec9775a45cbfe7f113cbbfe3b6d841413e3754a89dd658f",
    "28d702e2aea1f090349644b7a8b7efcb88a87c29bce7aa5fbf5c4c2303c1e8f6",
    "cc77a57d6e804837ce1ae3d5469a430470b537392b5c27292f623f33f4d29382",
    "eb2c6e97e8f4639866f5b15e5f7fba62b623517bdd0f49992cf90f6746f6e589",
    "3cae3ae30e758bf71475149a7c9d5eea12e8ebcf02382960c873b5b7da525d10",
    "670bbecf58c0c2fe97e0ba54982a44a19ea535492dd4bfb261794e53289adf1c",
    "27fa0ee2b17e75aabb6cccf6cd68f3f56568caeb4170031e44e3cd954947d3b7",
    "296a2ffb289ee74abd936c2e1bc25692638d7efc5accfcd8cc2c9b9e96774dd3",
    "b705e2c3db279403276d0d0ccfe1e26dd0775620e383d13c98a2dca00892a335",
    "4797dc300d40ce968d8f26000c890e74f85f1c0d24ee227dac936a69ec034d12",
    "e334e85e308e20bdeb3c3350f7fac0e97484d39f974409c9e09174c66a9c6c3e",
    "f47fe24d8d4566d0074ecf5627031d69348fa4ec3ece48ba8739685775ea3fa3",
    "c9cbbbdbd0ecdde70c0f3130ca1a7d2802f8682bbde5532907ef3c2589252f52",
    "4ee20455e942d978f3be267c0b99deffa4c43ee55b9bfc49c53610500ca238f2",
    "dfcd78f15cdadccb7db6f8094cb42421a409642a8b28041518a6bd2ec54bd5ef",
    "23431719bdf1d5ffee8e4cd5aa50afa2c3513f60acca13cc3226c83f1eb9113a",
    "079522237b973923ed05be828d6f62036ac642c3ee9fe493d921a7062b75e135",
    "956f81e8c0f1dc9192da65307a1f29c58c4c2f56a6ebf2703346538736459c06",
    "2328ea8fd57ee9b4a09602df23dcb12e8bda7664da670ab9d41a2aa8f058f30e",
    "c95c92344f08c42992699b0e467cf32fe573dda9b879250438d60a13cab4a9c6",
    "0d9918cc8f99bf46baf9f4544a413129aae5881b1e97326f69208057e441b98a",
    "43c721c5190432b1788ca4f79ab56c180f6ee2f9d958bb1cdd8ebeb64831a087",
    "7cb4f6abe8ab4282469fd3e70d3372122f9dbe6b0e03a5b192fc5c1393062582",
    "e9930d360eb9c124ea68bc47a8c0e91b25ff919dc5f91dbfc3f1a9e9c8a10579",
    "8aa82c81037a03820ed769623e6e3e33d130f48c0a2348db8d5ba1f87a85ba5b",
    "644eeb9c0057cf8ef3489234a2d414ca9c69a1b42683438c8cbd64acd46b41ed",
    "2557e020b41456a68e8d0fa8a43072012aca7d5c7aa653d74a2e0d22934a7a21",
    "bdf2d39982a399f2a42fc893aec503fa043103909aea8365f5f56c731585f8ba",
    "b66e1ca03bb35f0cf0be62c85e50536368b247d8dd65c88f7bf99d2f794d9616",
    "9e2bac11bf1398637b2868b863f6f46f42bc3be5510e840a65cca9ae3bde014e",
    "c4742e78f68cdeb2f7fd9ad85debbedd8767b66d09100cbf00deccbf789e8e21",
    "7ae3d8b793c2c97aad575a50577c045b4c881e6777d4d63de7020246f3daf345",
    "070bb3b2425cc800c02e2e9465241a8b94abdce93e712fd3baf19474b63b887c",
    "2f6079aed0be4c60632509761a407ba2545f0487a667dc228cfebeaacfd00d8d",
    "209cf0daf867c690ef3bd79b6f814719f51697477bc8837eea0da04427483aaa",
    "aad44664cc02b48271b3e26f87404a04f610dd8823c16f11f3fa738437b7f987",
    "3ea83cf4094fb389e4e4cc2c81b575c81213d8ad24844a9554983232e234f638",
    "812905a9016193c4aa23c7ebe947dd82cc0b8401996657a6bc7aaaee2dce7003",
    "d3d775de3dfb4091c8d3b631a018c89accac22565bf370e10d9172eb759a6ec8",
    "46fd3606270f2e790973f0280b837d4a9837d7a86cd3470ad0cc430e18d285e4",
    "59bc8aec09b5b4d1d6907be3eceea63d148f4f4e6ff120a2ffa6a96818569e71",
    "1e9bee3b01f67f802dba564ada22d28ca5f5e2bbc0eaf60005d9a8665d46d8b9",
    "f7274b79b081fbbf05d14b006359883c144304adb0ec0c6f9b8741feaef8f76d",
    "4dc2420ddafe56fc71345252e27666b3397cfc47884de86c0dd5c03419e83dcf",
    "75ad2407fc2a61df87e40ddfd9c77f5ed14584ae2ceb628018d3f5cd05003204",
    "401b2e29586dcee7c253b9002e57dbd0bdf6240e596478f5c95ce9b6413a9c1d",
    "10c1a47fe58714f679a8c33cd98fd900f974cdd1c1b41dad38fc7cbb01901a03",
    "8e030d2490555677cbcbfa76b8bee84a1bab76abf1f433d8084f1e2124fc7dc8",
    "f43ae142343fe54313e2f742931c76da11cb49e66d823280829a3f91ed227c2c",
    "8a7ee676f706b1e762b7b7ccaa7977e58650f0432e5c9f62083370c774eb136b",
    "b2b6fe249859902d1f82ff61621f5525926c8db032ceec4716c48105587c306a",
    "82fce430931931fd2fe574345a01bc9bf8bf4402910b1740f0ae8085c99c3324",
    "ebc5bff8bae73fb07ed37cb15795d4e0ef7ebe17290e745b6645224ea2361947",
    "daf934fd6a1040ed15800ba944da929d3e12886bce1d0f00374c0a96202d6cae",
];

fn load() -> std::result::Result<Vec<RetiredTemplate>, String> {
    if TABLE.len() != HASHES.len() {
        return Err(format!("retired table: {} entries but {} hashes", TABLE.len(), HASHES.len()));
    }
    let mut out: Vec<RetiredTemplate> = Vec::with_capacity(TABLE.len());
    for ((kind, retirement, prefix8, bytes), full) in TABLE.iter().zip(HASHES) {
        let name = format!("{} {prefix8}", kind.name());
        let hash = hex32(full).map_err(|e| format!("{name}: {e}"))?;
        if !full.starts_with(prefix8) {
            return Err(format!("{name}: the hash {full} does not start with its file name's prefix"));
        }
        let cur = try_template(*kind).ok_or_else(|| format!("{name}: {} is not an order template of this build", kind.name()))?;
        if kind.kind_code().is_none() {
            return Err(format!("{name}: {} is not an order kind", kind.name()));
        }
        // today's kind: the state span at the same place (the order kinds have no compiler-owned
        // context fields), in the layout of the row's retirement
        let layout = retirement.layout(*kind);
        let (prefix, suffix, len) = span_split(*kind, layout, cur.prefix.len(), bytes, &hash).map_err(|e| format!("{name}: {e}"))?;
        if PINNED.iter().any(|(_, h)| *h == full) {
            return Err(format!("{name}: {full} is a pinned template, not a retired one"));
        }
        if out.iter().any(|r| r.hash == hash) {
            return Err(format!("{name}: listed twice"));
        }
        // the cancel entry it is spent through is today's: `cancel(sig s)`, dispatch tag CANCEL_TAG
        let entry = cur.contract().entries.get(CANCEL_ENTRY).ok_or_else(|| format!("{}: no cancel entry", kind.name()))?;
        let one_sig = matches!(entry.params.as_slice(), [p] if matches!(p.ty, TypeArtifact::Sig));
        if *entry.dispatch_tag.as_bytes() != CANCEL_TAG || !one_sig {
            return Err(format!("{}: today's cancel entry is not cancel(sig) with tag {}", kind.name(), to_hex(&CANCEL_TAG)));
        }
        // the tag must be in the retired bytecode too (its dispatcher compares the pushed tag with
        // it)
        if !bytes.windows(4).any(|w| w == CANCEL_TAG) {
            return Err(format!("{name}: the bytecode does not carry the cancel dispatch tag"));
        }
        out.push(RetiredTemplate { kind: *kind, hash, retirement: *retirement, prefix, suffix, state_len: len, layout });
    }
    Ok(out)
}

/// The prefix, suffix and state length of a template's bytes `bytes` of the kind `kind` whose orders
/// have `layout`, with the state span at `offset`, checked against its template hash `hash`.
fn span_split(
    kind: TemplateId,
    layout: StateLayout,
    offset: usize,
    bytes: &[u8],
    hash: &[u8; 32],
) -> std::result::Result<(Vec<u8>, Vec<u8>, usize), String> {
    let len = state_len_in(kind, layout).ok_or_else(|| format!("{} has no {layout:?} layout", kind.name()))?;
    if layout_of_len(kind, len) != Some(layout) {
        return Err(format!("a {len}-byte span is not the {layout:?} layout of {}", kind.name()));
    }
    if bytes.len() < offset + len {
        return Err(format!("{} bytes cannot hold a state span {offset}+{len}", bytes.len()));
    }
    let prefix = bytes[..offset].to_vec();
    let suffix = bytes[offset + len..].to_vec();
    let computed = silverscript_abi::template_hash(&prefix, &suffix);
    if computed != *hash {
        return Err(format!("the bytes hash to {}, not {}", to_hex(&computed), to_hex(hash)));
    }
    Ok((prefix, suffix, len))
}

/// Every retired template of the cancel-only table.
///
/// The table is checked on first use. A broken table panics, like a broken artifact.
pub fn retired() -> &'static [RetiredTemplate] {
    static T: OnceLock<Vec<RetiredTemplate>> = OnceLock::new();
    T.get_or_init(|| load().unwrap_or_else(|e| panic!("retired templates: {e}")))
}

/// Checks the table without panicking (front ends report a broken build).
pub fn self_check() -> std::result::Result<(), String> {
    load().map(|_| ())
}

/// The retired template with this template hash.
pub fn retired_by_hash(hash: &[u8; 32]) -> Option<&'static RetiredTemplate> {
    retired().iter().find(|r| &r.hash == hash)
}

/// The retired template with this hash, which must be one of `kind` (the kind the order's state
/// decodes as).
pub fn retired_for(hash: &[u8; 32], kind: TemplateId) -> Result<&'static RetiredTemplate> {
    let r = retired_by_hash(hash)
        .ok_or_else(|| Error::Invalid(format!("template {} is not in the cancel-only table of retired templates", to_hex(hash))))?;
    if r.kind != kind {
        return Err(Error::Invalid(format!("the retired template {} is a {}, not a {}", r.label(), r.kind.name(), kind.name())));
    }
    Ok(r)
}

/// [`RetiredInfo`] of every retired template.
pub fn retired_infos() -> Vec<RetiredInfo> {
    retired()
        .iter()
        .map(|r| RetiredInfo {
            kind: r.kind.name().into(),
            hash: r.hash,
            label: r.label(),
            retired_by: r.retirement.retired_by().into(),
            last_release: r.retirement.last_release().into(),
            entries: vec![CANCEL_ENTRY.into()],
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// TPLGEN step 1: a row's span is read in its retirement's layout. Today's delivery-block kinds, retired at the
    /// re-pin, keep the block: their bytes (today's artifacts, as the next retirement's files will be) split at the
    /// state with the delivery layout and hash to their template, and the legacy layout does not fit them.
    #[test]
    fn a_retired_row_is_read_in_its_retirements_layout() {
        use crate::state::{delivery_block, pinned_layout};
        let mut delivering = 0;
        for (id, _) in PINNED.iter() {
            let id = *id;
            let t = template(id);
            let bytes = [t.prefix.as_slice(), &vec![0u8; t.state_len], t.suffix.as_slice()].concat();
            let layout = pinned_layout(id);
            let (p, s, len) = span_split(id, layout, t.prefix.len(), &bytes, &t.hash).unwrap_or_else(|e| panic!("{}: {e}", id.name()));
            assert_eq!((p.len(), s.len(), len), (t.prefix.len(), t.suffix.len(), t.state_len), "{}", id.name());
            if delivery_block(id).is_some() {
                assert_eq!(layout, StateLayout::Delivery, "{}", id.name());
                assert!(span_split(id, StateLayout::Legacy, t.prefix.len(), &bytes, &t.hash).is_err(), "{}", id.name());
                delivering += 1;
            }
        }
        assert!(delivering >= 6, "the delivery-block kinds: {delivering}");
    }

    #[test]
    fn the_table_loads_and_is_not_pinned() {
        self_check().unwrap();
        assert_eq!(retired().len(), 71);
        for r in retired() {
            // the span of the layout its retirement placed it with: the legacy one before the delivery block (a kind with
            // the block is longer today), today's for the pre-mainnet re-pin (TPLGEN), which kept every state
            let want = r.retirement.layout(r.kind);
            assert_eq!(r.layout, want, "{}", r.label());
            assert_eq!(layout_of_len(r.kind, r.state_len), Some(want), "{}", r.label());
            match (crate::state::delivery_block(r.kind), r.retirement) {
                (_, Retirement::PreMainnet) => assert_eq!(r.state_len, template(r.kind).state_len, "{}", r.label()),
                (Some(b), _) => assert!(r.state_len == b.legacy_len && r.state_len < template(r.kind).state_len, "{}", r.label()),
                (None, _) => assert_eq!(r.state_len, template(r.kind).state_len, "{}", r.label()),
            }
            // a retired template is never one this build pins, of any kind
            assert!(crate::artifacts::template_by_hash(&r.hash).is_none(), "{}", r.label());
            assert_eq!(retired_by_hash(&r.hash).unwrap().hash, r.hash);
            assert!(retired_for(&r.hash, r.kind).is_ok());
            let other = if r.kind == TemplateId::KobBid { TemplateId::KobAsk } else { TemplateId::KobBid };
            assert!(retired_for(&r.hash, other).is_err(), "{}", r.label());
        }
        // the bid of the bid re-version and its KRON twins, as template-retirement.md names them
        for label in ["KobBid b995661f", "KobBidKron 6ec1a3dd", "KobBidKronD0 436fbbbb", "KobCondBid 26042aff", "KobCondAsk 02a34664"]
        {
            assert!(retired().iter().any(|r| r.label() == label), "{label}");
        }
        // the bid delivery re-pin: every kind that changed, the pair order included
        for label in [
            "KobBid dfcd78f1",
            "KobCondAsk 23431719",
            "KobCondBid 07952223",
            "KobIfdBid 956f81e8",
            "KobIfdAsk 2328ea8f",
            "KobPair c95c9234",
            "KobCondPair 0d9918cc",
            "KobIfdPair 43c721c5",
        ] {
            assert!(retired().iter().any(|r| r.label() == label), "{label}");
        }
        // the minimum-fill re-pin: every KRON kind of TN10 generation 0506de663e7837a3 that changed (KobAskKron did not)
        for label in [
            "KobBidKron 7cb4f6ab",
            "KobCondAskKron e9930d36",
            "KobCondBidKron 8aa82c81",
            "KobIfdBidKron 644eeb9c",
            "KobIfdAskKron 2557e020",
            "KobBidKronD0 bdf2d399",
            "KobCondAskKronD0 b66e1ca0",
            "KobCondBidKronD0 9e2bac11",
            "KobIfdBidKronD0 c4742e78",
            "KobIfdAskKronD0 7ae3d8b7",
        ] {
            let r = retired().iter().find(|r| r.label() == label).unwrap_or_else(|| panic!("{label}"));
            assert_eq!(r.retirement, Retirement::BidMinFill, "{label}");
        }
    }

    #[test]
    fn redeem_and_state_round_trip() {
        for r in retired() {
            let state = vec![0x51u8; r.state_len];
            let redeem = r.redeem(&state).unwrap();
            assert_eq!(r.state_of(&redeem), Some(state.as_slice()));
            assert!(r.redeem(&state[1..]).is_err());
            // a retired script is never today's script of the same state (a delivering kind: today's span is longer)
            let today = template(r.kind);
            if today.state_len == r.state_len {
                assert_ne!(r.spk(&state).unwrap(), today.spk(&state), "{}", r.label());
            } else {
                assert!(today.state_len > r.state_len && (r.prefix != today.prefix || r.suffix != today.suffix), "{}", r.label());
            }
        }
    }

    #[test]
    fn cancel_sigscript_is_sig_tag_redeem() {
        let r = retired_by_hash(&hex32(HASHES[0]).unwrap()).unwrap();
        let state = vec![0u8; r.state_len];
        let sig = [7u8; 65];
        let s = r.cancel_sigscript(&state, &sig).unwrap();
        let want = [
            crate::script::push_data(&sig),
            crate::script::push_data(&CANCEL_TAG),
            crate::script::push_data(&r.redeem(&state).unwrap()),
        ]
        .concat();
        assert_eq!(s, want);
    }

    /// Audit 2026-11-23 A3-2: every order template hash any deployment record listed (`data/retired/record-history.tsv`,
    /// `scripts/record-history.mjs`) is exactly one of: pinned, in the cancel-only table, or classified by hand with its
    /// evidence (`never-deployed`, or `pre-f1`: the October generations, which the table leaves out by decision). A re-pin
    /// or a retirement that the file does not list, and a hash nobody classified, fail here.
    #[test]
    fn every_recorded_template_is_pinned_cancel_only_or_classified() {
        use std::collections::BTreeMap;
        let pinned: Vec<&str> = crate::artifacts::PINNED.iter().map(|(_, h)| *h).collect();
        let mut classes: BTreeMap<&str, usize> = BTreeMap::new();
        let mut listed = std::collections::BTreeSet::new();
        for l in include_str!("../data/retired/record-history.tsv").lines().filter(|l| !l.starts_with('#')) {
            let f: Vec<&str> = l.split('\t').collect();
            assert_eq!(f.len(), 6, "{l}");
            let (hash, class, reason) = (f[0], f[4], f[5]);
            assert!(listed.insert(hash), "{hash} listed twice");
            let (is_pinned, is_retired) = (pinned.contains(&hash), HASHES.contains(&hash));
            match class {
                "pinned" => assert!(is_pinned && !is_retired, "{hash}: not pinned"),
                "cancel-only" => assert!(is_retired && !is_pinned, "{hash}: not in the cancel-only table"),
                "never-deployed" | "pre-f1" => {
                    assert!(!is_pinned && !is_retired, "{hash}: {class}, yet pinned or cancel-only");
                    assert!(reason.len() > 40, "{hash}: {class} needs its evidence");
                }
                other => panic!("{hash}: class {other}: classify it (scripts/record-history.mjs)"),
            }
            *classes.entry(class).or_default() += 1;
        }
        for h in pinned.iter().chain(HASHES.iter()) {
            assert!(listed.contains(h), "{h} is pinned or cancel-only but not in record-history.tsv: run scripts/record-history.mjs");
        }
        // The hand-classified sets do not grow unnoticed: a new one is a decision, not a line.
        assert_eq!((classes["never-deployed"], classes["pre-f1"]), (22, 35));
    }
}
