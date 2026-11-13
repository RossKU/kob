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
//! Every retired template here shares the state layout and the `cancel(sig)` entry of today's kind
//! of the same name. That is checked on first use: the state span length, and the ABI and dispatch
//! tag of the cancel entry of today's artifact. So its terms decode with today's codec
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
}

impl Retirement {
    /// The commit that retired the templates.
    pub fn retired_by(self) -> &'static str {
        match self {
            Retirement::BidRefund => "acf39cf",
            Retirement::F1 => "da35841",
        }
    }

    /// The last commit that carries the templates (their artifacts and sources).
    pub fn last_release(self) -> &'static str {
        match self {
            Retirement::BidRefund => "59d9ea30",
            Retirement::F1 => "60a4d11",
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
    /// Length of the state span (that of `kind`).
    pub state_len: usize,
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
];

/// The full template hashes of [`TABLE`], in its order: the hashes the retired orders were placed
/// under. Each is recomputed from its bytes on load.
const HASHES: [&str; 31] = [
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
        // today's kind: the same state span at the same place (the order kinds have no
        // compiler-owned context fields)
        let (offset, len) = (cur.prefix.len(), cur.state_len);
        if bytes.len() < offset + len {
            return Err(format!("{name}: {} bytes cannot hold a state span {offset}+{len}", bytes.len()));
        }
        let prefix = bytes[..offset].to_vec();
        let suffix = bytes[offset + len..].to_vec();
        let computed = silverscript_abi::template_hash(&prefix, &suffix);
        if computed != hash {
            return Err(format!("{name}: the bytes hash to {}, not {full}", to_hex(&computed)));
        }
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
        out.push(RetiredTemplate { kind: *kind, hash, retirement: *retirement, prefix, suffix, state_len: len });
    }
    Ok(out)
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

    #[test]
    fn the_table_loads_and_is_not_pinned() {
        self_check().unwrap();
        assert_eq!(retired().len(), 31);
        for r in retired() {
            assert_eq!(r.state_len, template(r.kind).state_len, "{}", r.label());
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
    }

    #[test]
    fn redeem_and_state_round_trip() {
        for r in retired() {
            let state = vec![0x51u8; r.state_len];
            let redeem = r.redeem(&state).unwrap();
            assert_eq!(r.state_of(&redeem), Some(state.as_slice()));
            assert!(r.redeem(&state[1..]).is_err());
            // a retired script is never today's script of the same state
            assert_ne!(r.spk(&state).unwrap(), template(r.kind).spk(&state), "{}", r.label());
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
}
