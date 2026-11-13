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
//!   the program it mints (the app's `KCC20`). The pin is the `bridge-minter` companion of
//!   `KCC20PublicMint` in `data/programs.json`. KOB does not review the bridge. The pin only
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
/// Length of the `BridgeMint` state span after the 34-byte prefix.
///
/// The span is eight 8-byte integers, a bool and three 32-byte fields, each with its push opcode.
/// Its extension commitment is the last field.
pub const BRIDGE_MINT_STATE_LEN: usize = 173;

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
/// The span is the state after the 34-byte prefix. The commitment is its last field (`0x20` and 32
/// bytes). `None` when the span is not that length or the field is not pushed that way.
pub fn bridge_mint_extension(state: &[u8]) -> Option<[u8; 32]> {
    let tail = state.len().checked_sub(33).filter(|_| state.len() == BRIDGE_MINT_STATE_LEN).map(|i| &state[i..])?;
    (tail[0] == 0x20).then(|| tail[1..].try_into().expect("32"))
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

    #[test]
    fn the_extension_commitment_of_a_bridge_mint_state() {
        let mut s = vec![0u8; BRIDGE_MINT_STATE_LEN];
        s[140] = 0x20;
        s[141..].copy_from_slice(&[7; 32]);
        assert_eq!(bridge_mint_extension(&s), Some([7; 32]));
        s[140] = 0x21;
        assert_eq!(bridge_mint_extension(&s), None);
        assert_eq!(bridge_mint_extension(&s[1..]), None);
    }
}
