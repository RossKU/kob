//! kUSDT, a KCC-20 token whose issuance is a federated bridge.
//!
//! The token is described in `contracts/third-party/kusdt/UPSTREAM.md`. It runs the `KCC20` program
//! of the published public-mint app (KOB's `KCC20PublicMint`, the same bytes). Its genesis group
//! holds a `BridgeMint` and a `TokenSeed` of the app. The `BridgeMint` is the only issuer: it mints
//! against USDT locked in EVM vaults when 3 of its 5 signers sign. A redemption sends kUSDT to a
//! burn owner that names the payout on the EVM side.
//!
//! KOB reads two things of it, from the chain only.
//!
//! - **The `BridgeMint` companion.** It is pinned by its template hash over the KCC-1 actor-type
//!   cut: the 34-byte prefix with the compiler-owned `gen__kcc20_template` context, which names
//!   the program it mints (the app's `KCC20`). The pins are the `bridge-minter` companions of
//!   `KCC20PublicMint` in `data/programs.json`, one per build of the issuer
//!   ([`BRIDGE_MINT_STATE_LENS`]). KOB does not review the bridge. The pin only
//!   recognises the covenant, so a genesis holding it verifies and is reported as having a mint
//!   authority. The apps name the issuance ("federated bridge") instead of counting a public
//!   mint. Its state is not read, except for the extension commitment its mints carry
//!   ([`bridge_mint_extension`]), which the genesis check holds to the token's one class.
//! - **Burned cells.** A burn owner is the `p2sh/v1` authority (owner scheme 3) of a 40-byte script
//!   `R` that starts with `OP_RETURN` ([`burn_script`]). No input can ever satisfy it, so the
//!   tokens can never move again. A redemption carries the preimages in its payload
//!   ([`redemption_entries`]). An output whose state hashes to one of them is burned
//!   ([`is_burned`]), and the indexer keeps it out of the holdings. Without a payload a burn is
//!   not recognised, because its owner is just a script hash.

use crate::state::TokenState;

/// Tag in the burn script `R` (mixed case, exact bytes).
pub const REDEEM_TAG: &[u8; 15] = b"kUSDT/redeem/v1";
/// Magic of a redemption payload (upper case, exact bytes).
pub const PAYLOAD_MAGIC: &[u8; 5] = b"KUSDT";
/// Payload kind of a redemption.
pub const PAYLOAD_REDEMPTION: u8 = 0x01;
/// Length of the state span after the 34-byte prefix of the first `BridgeMint` build KOB pins
/// (testnet-10 covenant `6711958e...`, superseded): eight 8-byte integers, a bool and three
/// 32-byte fields, each with its push opcode. Its extension commitment is the last field.
pub const BRIDGE_MINT_STATE_LEN_V1: usize = 173;
/// Length of the state span of the `BridgeMint` build with the signer-recovery path (testnet-10
/// covenant `a1d9fd13...`): seven 8-byte integers, a bool, three 32-byte fields (the extension
/// commitment is the third), then the recovery fields: two integers, a bool and two 32-byte fields.
pub const BRIDGE_MINT_STATE_LEN_RECOVERY: usize = 250;
/// The state span lengths of the `BridgeMint` builds KOB pins. A `bridge-minter` companion has one
/// of them.
pub const BRIDGE_MINT_STATE_LENS: [usize; 2] = [BRIDGE_MINT_STATE_LEN_V1, BRIDGE_MINT_STATE_LEN_RECOVERY];

/// The push sizes of the fields of a `BridgeMint` state span, per build (in the order of
/// [`BRIDGE_MINT_STATE_LENS`]), and the index of the extension commitment among them.
const BRIDGE_MINT_FIELDS: [(&[usize], usize); 2] =
    [(&[8, 8, 8, 8, 8, 8, 8, 8, 1, 32, 32, 32], 11), (&[8, 8, 8, 8, 8, 8, 8, 1, 32, 32, 32, 8, 8, 1, 32, 32], 10)];

/// The burn script `R(chain, address)`, 40 bytes.
///
/// It is `6a 0f "kUSDT/redeem/v1" 01 chain 14 address[20]`, where `6a` is `OP_RETURN`.
pub fn burn_script(chain: u8, address: &[u8; 20]) -> [u8; 40] {
    let mut r = [0u8; 40];
    r[0] = 0x6a;
    r[1] = 0x0f;
    r[2..17].copy_from_slice(REDEEM_TAG);
    r[17] = 0x01;
    r[18] = chain;
    r[19] = 0x14;
    r[20..].copy_from_slice(address);
    r
}

/// The owner of a burn state (owner scheme 3): `Blake2b(R)`, the `p2sh/v1` authority of
/// [`burn_script`].
pub fn burn_owner(chain: u8, address: &[u8; 20]) -> [u8; 32] {
    crate::kcc2::p2sh_authority(&burn_script(chain, address))
}

/// One burn output a redemption payload describes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RedemptionEntry {
    /// Output index in the transaction.
    pub output: u8,
    /// Payout chain code (`01` Ethereum, `02` BNB Chain).
    pub chain: u8,
    /// The EVM address to pay.
    pub address: [u8; 20],
}

impl RedemptionEntry {
    /// The burn owner the entry names.
    pub fn owner(&self) -> [u8; 32] {
        burn_owner(self.chain, &self.address)
    }
}

/// The entries of a redemption payload, `None` for any other payload.
///
/// The payload is `"KUSDT" 01 count (output_index chain address[20]){count}`, with `count` 1 to 255
/// and nothing after the entries.
pub fn redemption_entries(payload: &[u8]) -> Option<Vec<RedemptionEntry>> {
    let rest = payload.strip_prefix(PAYLOAD_MAGIC.as_slice())?;
    let (&kind, rest) = rest.split_first()?;
    let (&count, rest) = rest.split_first()?;
    if kind != PAYLOAD_REDEMPTION || count == 0 || rest.len() != 22 * count as usize {
        return None;
    }
    Some(
        rest.chunks_exact(22)
            .map(|e| RedemptionEntry { output: e[0], chain: e[1], address: e[2..22].try_into().expect("20") })
            .collect(),
    )
}

/// True when `state`, the proven state of output `output`, is a burn.
///
/// A burn is a KCC-20 state of owner scheme 3 whose owner is the burn owner that the entry for that
/// output names. `entries` are the redemption entries of the transaction.
pub fn is_burned(entries: &[RedemptionEntry], output: u32, state: &TokenState) -> bool {
    let Some(k) = state.as_kcc20() else { return false };
    k.owner_scheme == crate::kcc2::P2SH && entries.iter().any(|e| u32::from(e.output) == output && e.owner() == k.owner)
}

/// The extension commitment that a `BridgeMint` state span gives every state it mints.
///
/// The span is the state after the 34-byte prefix. Its length names the build
/// ([`BRIDGE_MINT_STATE_LENS`]): the commitment is the last field of a 173-byte span and the
/// eleventh (bytes 131..164: `0x20` and 32 bytes) of a 250-byte one. `None` (fail closed) for any
/// other length, or when the span is not exactly that build's fields, each a direct push of its
/// size.
pub fn bridge_mint_extension(state: &[u8]) -> Option<[u8; 32]> {
    let (sizes, ext) =
        BRIDGE_MINT_FIELDS.iter().zip(BRIDGE_MINT_STATE_LENS).find_map(|(f, len)| (state.len() == len).then_some(*f))?;
    let mut at = 0;
    let mut found = None;
    for (i, &n) in sizes.iter().enumerate() {
        let field = state.get(at + 1..at + 1 + n).filter(|_| usize::from(state[at]) == n)?;
        if i == ext {
            found = Some(field.try_into().ok()?);
        }
        at += 1 + n;
    }
    found.filter(|_| at == state.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json::to_hex;
    use crate::state::Kcc20State;

    #[test]
    fn the_burn_owner_of_the_issuers_worked_example() {
        // kusdt-docs protocol/burn-owner-and-payload: chain 01, address 0x1111...
        let a = [0x11; 20];
        assert_eq!(to_hex(&burn_script(1, &a)), "6a0f6b555344542f72656465656d2f76310101141111111111111111111111111111111111111111");
        assert_eq!(to_hex(&burn_owner(1, &a)), "09d6acad2a8e131ae14825cbf7362cc85042f7355baed747aed5599f76afe87e");
    }

    #[test]
    fn redemption_payloads_and_burned_states() {
        // the issuer's example: one burn at output 0 to 0x1111... on Ethereum
        let p = crate::json::from_hex("4b55534454010100011111111111111111111111111111111111111111").unwrap();
        let e = redemption_entries(&p).unwrap();
        assert_eq!(e, vec![RedemptionEntry { output: 0, chain: 1, address: [0x11; 20] }]);
        for bad in [&p[..p.len() - 1], &[p.as_slice(), &[0]].concat()[..], b"KUSDT\x02\x01", b"KUSDT\x01\x00", b"KOB1\x01", b""] {
            assert_eq!(redemption_entries(bad), None, "{}", to_hex(bad));
        }
        let burned = TokenState::Kcc20(Kcc20State { owner_scheme: 3, ..Kcc20State::p2pk(5, e[0].owner(), [0; 32]) });
        assert!(is_burned(&e, 0, &burned));
        // another output, another scheme with the same 32 bytes, another address: not a burn
        assert!(!is_burned(&e, 1, &burned));
        assert!(!is_burned(&e, 0, &TokenState::Kcc20(Kcc20State::p2pk(5, e[0].owner(), [0; 32]))));
        let other = TokenState::Kcc20(Kcc20State { owner_scheme: 3, ..Kcc20State::p2pk(5, burn_owner(2, &[0x11; 20]), [0; 32]) });
        assert!(!is_burned(&e, 0, &other));
    }

    /// A state span of the given push sizes, each field filled with its index + 1.
    fn span(sizes: &[usize]) -> Vec<u8> {
        sizes.iter().enumerate().flat_map(|(i, &n)| std::iter::once(n as u8).chain(std::iter::repeat_n(i as u8 + 1, n))).collect()
    }

    #[test]
    fn the_field_tables_are_the_pinned_lengths() {
        for ((sizes, ext), len) in BRIDGE_MINT_FIELDS.iter().zip(BRIDGE_MINT_STATE_LENS) {
            assert_eq!(sizes.iter().map(|n| n + 1).sum::<usize>(), len);
            assert_eq!(sizes[*ext], 32);
        }
    }

    #[test]
    fn the_extension_commitment_of_a_first_build_state() {
        let mut s = span(BRIDGE_MINT_FIELDS[0].0);
        assert_eq!(s.len(), BRIDGE_MINT_STATE_LEN_V1);
        // the last field
        assert_eq!(s[140], 0x20);
        s[141..].copy_from_slice(&[7; 32]);
        assert_eq!(bridge_mint_extension(&s), Some([7; 32]));
        s[140] = 0x21;
        assert_eq!(bridge_mint_extension(&s), None);
        assert_eq!(bridge_mint_extension(&s[1..]), None);
    }

    #[test]
    fn the_extension_commitment_of_a_recovery_build_state() {
        let mut s = span(BRIDGE_MINT_FIELDS[1].0);
        assert_eq!(s.len(), BRIDGE_MINT_STATE_LEN_RECOVERY);
        // bytes 131..164, not the last field (the proposed recovery guardian)
        assert_eq!(s[131], 0x20);
        s[132..164].copy_from_slice(&[7; 32]);
        assert_eq!(bridge_mint_extension(&s), Some([7; 32]));
        // a push of another size anywhere moves the fields after it: refused
        for at in [0, 63, 65, 131, 164, 182, 217] {
            let mut t = s.clone();
            t[at] ^= 1;
            assert_eq!(bridge_mint_extension(&t), None, "{at}");
        }
    }

    #[test]
    fn any_other_state_length_fails_closed() {
        for len in [0, 33, 112, 172, 174, 206, 249, 251, 283] {
            assert_eq!(bridge_mint_extension(&vec![0x20; len]), None, "{len}");
        }
        // a 250-byte span laid out as the first build's fields plus padding is no recovery-build state
        let mut s = span(BRIDGE_MINT_FIELDS[0].0);
        s.resize(BRIDGE_MINT_STATE_LEN_RECOVERY, 0);
        assert_eq!(bridge_mint_extension(&s), None);
    }
}
