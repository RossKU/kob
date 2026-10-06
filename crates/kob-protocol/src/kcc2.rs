//! KCC-2 (Authority Schemes): the scheme registry, authority values and the minimum approval checks.
//!
//! Pinned to kaspanet/kccs `main` at `411b41bc14b3fda8f3a0548242c555f3597cad1a` (KCC-2 in Last Call since 2026-10-01;
//! #30 made the P2PKH authorities the UNKEYED `Hash(pubkey)` and split the registry into a standard and a custom range).
//! The official vectors are vendored in `crates/kob-tests/vectors/kcc2/` and executed by
//! `crates/kob-tests/tests/kcc2_conformance_tests.rs` (pure checks below) and by
//! `crates/kob-tests/tests/kcc20_conformance_tests.rs` (the same cases executed in KOB's KCC-20 programs).
//!
//! KCC-20 (KOB's token programs) uses every assigned scheme `0x00`-`0x04` as its `owner_scheme` and no custom scheme.
//! The functions here are the off-chain mirror of what the programs check; the programs themselves are the authority.

use kaspa_consensus_core::tx::ScriptPublicKey;
use kaspa_txscript::pay_to_script_hash_script;

use crate::kcc1;

/// `p2pk-schnorr/v1`: the 32-byte Schnorr public key itself (`pubkey`).
pub const P2PK_SCHNORR: u8 = 0x00;
/// `p2pkh-schnorr/v1`: `byte[32]` containing `Hash(pubkey)` of a 32-byte Schnorr key.
pub const P2PKH_SCHNORR: u8 = 0x01;
/// `p2pkh-ecdsa/v1`: `byte[32]` containing `Hash(ecdsa_pubkey)` of a compressed 33-byte ECDSA key.
pub const P2PKH_ECDSA: u8 = 0x02;
/// `p2sh/v1`: `byte[32]` containing the KCC-1 P2SH commitment `Blake2b(R)`.
pub const P2SH: u8 = 0x03;
/// `covenant-id/v1`: `byte[32]` containing a KIP-20 Covenant ID.
pub const COVENANT_ID: u8 = 0x04;
/// Every assigned standard scheme.
pub const ASSIGNED: [u8; 5] = [P2PK_SCHNORR, P2PKH_SCHNORR, P2PKH_ECDSA, P2SH, COVENANT_ID];
/// First byte of the custom range (`0x80`-`0xff`, schemes defined outside KCC-2).
pub const CUSTOM_RANGE_START: u8 = 0x80;

/// Where a scheme byte sits in the registry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SchemeClass {
    /// `0x00`-`0x04`: an assigned standard scheme.
    Assigned,
    /// `0x05`-`0x7f`: reserved for future standard schemes; implementations MUST reject it and MUST NOT give it a local
    /// meaning.
    Reserved,
    /// `0x80`-`0xff`: a custom scheme, meaningful only under a convention that defines it.
    Custom,
}

/// The registry range of a scheme byte.
pub fn classify(scheme: u8) -> SchemeClass {
    match scheme {
        P2PK_SCHNORR..=COVENANT_ID => SchemeClass::Assigned,
        0x05..=0x7f => SchemeClass::Reserved,
        _ => SchemeClass::Custom,
    }
}

/// The scheme's canonical name (`None` outside the assigned range).
pub fn scheme_name(scheme: u8) -> Option<&'static str> {
    Some(match scheme {
        P2PK_SCHNORR => "p2pk-schnorr/v1",
        P2PKH_SCHNORR => "p2pkh-schnorr/v1",
        P2PKH_ECDSA => "p2pkh-ecdsa/v1",
        P2SH => "p2sh/v1",
        COVENANT_ID => "covenant-id/v1",
        _ => return None,
    })
}

/// Length of the public key an assigned signature scheme verifies with (`None` for P2SH and covenant id).
pub fn public_key_len(scheme: u8) -> Option<usize> {
    match scheme {
        P2PK_SCHNORR | P2PKH_SCHNORR => Some(32),
        P2PKH_ECDSA => Some(33),
        _ => None,
    }
}

/// The authority value a P2PKH scheme stores: `Hash(pubkey)`, unkeyed BLAKE3 over the raw key bytes. `None` when the key
/// has the wrong length for the scheme (32 bytes for Schnorr, compressed 33 bytes starting `02`/`03` for ECDSA).
pub fn p2pkh_authority(scheme: u8, public_key: &[u8]) -> Option<[u8; 32]> {
    match (scheme, public_key.len()) {
        (P2PKH_SCHNORR, 32) => Some(kcc1::hash(public_key)),
        (P2PKH_ECDSA, 33) if matches!(public_key[0], 0x02 | 0x03) => Some(kcc1::hash(public_key)),
        _ => None,
    }
}

/// The `p2sh/v1` authority value of a redeem script `R`: `Blake2b(R)`, as committed by the version-0 P2SH envelope.
pub fn p2sh_authority(redeem_script: &[u8]) -> [u8; 32] {
    pay_to_script_hash_script(redeem_script).script()[2..34].try_into().expect("P2SH script carries a 32-byte hash")
}

/// The version-0 P2SH script public key a `p2sh/v1` authority requires among the spent inputs.
pub fn p2sh_envelope(authority: &[u8; 32]) -> ScriptPublicKey {
    let mut s = Vec::with_capacity(35);
    s.push(0xaa); // OP_BLAKE2B
    s.push(0x20); // OP_DATA_32
    s.extend_from_slice(authority);
    s.push(0x87); // OP_EQUAL
    ScriptPublicKey::new(0, s.into())
}

/// Minimum approval check of a signature scheme (KCC-2 section 2.2): `verification_key` is the key the signature was
/// checked against and `signature_valid` the result. P2PK: the key is the stored authority. P2PKH: the revealed key has
/// the scheme's exact length and `Hash(key)` equals the authority.
pub fn signature_approval(scheme: u8, authority: &[u8], verification_key: &[u8], signature_valid: bool) -> bool {
    signature_valid
        && match scheme {
            P2PK_SCHNORR => authority.len() == 32 && verification_key == authority,
            P2PKH_SCHNORR | P2PKH_ECDSA => p2pkh_authority(scheme, verification_key).is_some_and(|h| h.as_slice() == authority),
            _ => false,
        }
}

/// Minimum approval check of `p2sh/v1` (KCC-2 section 2.3.1): some spent input's script public key is exactly the
/// version-0 P2SH envelope of the authority.
pub fn p2sh_approval(authority: &[u8; 32], input_script_public_keys: &[ScriptPublicKey]) -> bool {
    let want = p2sh_envelope(authority);
    input_script_public_keys.contains(&want)
}

/// Minimum approval check of `covenant-id/v1` (KCC-2 section 2.3.2): some spent input (the active one included) belongs
/// to the lineage. Outputs carrying the id do not count.
pub fn covenant_approval(authority: &[u8; 32], input_covenant_ids: &[Option<[u8; 32]>]) -> bool {
    input_covenant_ids.iter().any(|c| c.as_ref() == Some(authority))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_ranges() {
        assert!(ASSIGNED.iter().all(|s| classify(*s) == SchemeClass::Assigned && scheme_name(*s).is_some()));
        assert_eq!(classify(0x05), SchemeClass::Reserved);
        assert_eq!(classify(0x7f), SchemeClass::Reserved);
        assert_eq!(classify(0x80), SchemeClass::Custom);
        assert_eq!(classify(0xff), SchemeClass::Custom);
    }
}
