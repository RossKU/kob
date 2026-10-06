//! KRON token helpers shared by the KRON-family v2 test suites: the 46-byte state codec, the pinned
//! real token programs, and the token sigscript (KRON SDK `transferSigScript` layout).
//!
//! The KRON token program has ONE entry and no leader/delegator split: every token input of a
//! transaction carries the same `owners | types | amounts | isMinters` columns (the next states of the
//! whole covenant group), a shared signature column (only id_type 0 uses it) and a witness column with
//! one byte per token input (in input order) that points at the input which authorises that token
//! input: for id_type 2 (covenant id) an input whose covenant id equals the owner, for id_type 3
//! (address presence) an input whose script public key is P2PK(owner), for id_type 1 a P2SH input.
#![allow(dead_code)]

use kaspa_consensus_core::tx::ScriptPublicKey;
use kaspa_txscript::pay_to_script_hash_script;
use silverscript_lang::template::template_hash;

use super::{kron_template, push_redeem_script};

/// KRON id_type values.
pub const T_PUBKEY: u8 = 0;
pub const T_SCRIPT_HASH: u8 = 1;
pub const T_COVID: u8 = 2;
pub const T_ADDR: u8 = 3;
/// Length of the KRON token state span.
pub const STATE_LEN: usize = 46;
/// The two pinned template files (`contracts/adapters/kron/templates/`).
pub const TPL_2433: &str = "kron_token_2433.bin";
pub const TPL_2732: &str = "kron_token_2732.bin";

/// One KRON token state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KS {
    pub owner: [u8; 32],
    pub typ: u8,
    pub amount: i64,
    pub minter: u8,
}

/// Non-minter state.
pub fn ks<O: AsRef<[u8]>>(owner: O, typ: u8, amount: i64) -> KS {
    let mut o = [0u8; 32];
    o.copy_from_slice(owner.as_ref());
    KS { owner: o, typ, amount, minter: 0 }
}

impl KS {
    /// The 46-byte state region: 0x20 owner | 0x01 type | 0x08 amount LE | 0x01 minter.
    pub fn bytes(&self) -> Vec<u8> {
        let mut v = vec![0x20];
        v.extend_from_slice(&self.owner);
        v.extend_from_slice(&[0x01, self.typ, 0x08]);
        v.extend_from_slice(&self.amount.to_le_bytes());
        v.extend_from_slice(&[0x01, self.minter]);
        assert_eq!(v.len(), STATE_LEN);
        v
    }
}

/// A pinned KRON token program (state prefix = 46 B at offset 0, template prefix length 0).
#[derive(Clone)]
pub struct Kron {
    pub suffix: Vec<u8>,
    /// `silverscript_lang::template::template_hash(&[], &suffix)`.
    pub hash: Vec<u8>,
}

impl Kron {
    /// Load `contracts/adapters/kron/templates/<file>` (e.g. [`TPL_2433`]).
    pub fn load(file: &str) -> Self {
        let b = kron_template(file);
        let suffix = b[STATE_LEN..].to_vec();
        let hash = template_hash(&[], &suffix).to_vec();
        Kron { suffix, hash }
    }
    /// Full redeem script of a token UTXO in state `s`.
    pub fn redeem(&self, s: &KS) -> Vec<u8> {
        [s.bytes(), self.suffix.clone()].concat()
    }
    pub fn spk(&self, s: &KS) -> ScriptPublicKey {
        pay_to_script_hash_script(&self.redeem(s))
    }
}

/// KRON token sigscript: owners | types | amounts | minters | sigs | witnesses | redeem, each column one
/// concatenated push; `next` are the output states of the covenant group.
pub fn kron_ss(redeem: &[u8], next: &[KS], sigs: &[u8], wit: &[u8]) -> Vec<u8> {
    let p = push_redeem_script;
    let mut s = vec![];
    s.extend(p(&next.iter().flat_map(|x| x.owner.to_vec()).collect::<Vec<u8>>()));
    s.extend(p(&next.iter().map(|x| x.typ).collect::<Vec<u8>>()));
    s.extend(p(&next.iter().flat_map(|x| x.amount.to_le_bytes().to_vec()).collect::<Vec<u8>>()));
    s.extend(p(&next.iter().map(|x| x.minter).collect::<Vec<u8>>()));
    s.extend(p(sigs));
    s.extend(p(wit));
    s.extend(p(redeem));
    s
}
