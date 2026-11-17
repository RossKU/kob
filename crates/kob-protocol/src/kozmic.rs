//! Kozmic, Kaspero Labs' USDC bridge (kzUSDC on mainnet), a KCC-20 token issued by a ZK bridge.
//!
//! The token is described in `contracts/third-party/kozmic/UPSTREAM.md`. Its holders run the
//! `KCC20` program of the published public-mint app (KOB's `KCC20PublicMint`, the same bytes). Its
//! genesis group holds one `BridgeMint`, the only issuer. A `BridgeMint` mints only in a
//! transaction that also spends the bridge covenant its state names, a RISC Zero light client of
//! Ethereum that decides the amounts and owners.
//!
//! KOB reads two things of it, from the chain only.
//!
//! - **The `BridgeMint` companion.** It is pinned by its template hash over the KCC-1 actor-type
//!   cut (the 34-byte prefix holds the push of `gen__kcc20_template`, the app's `KCC20`). It is the
//!   `zk-bridge-minter` companion of `KCC20PublicMint` in `data/programs.json`. Every instance of
//!   the build matches: the bridge is in the state, and KOB does not check it. Its state span
//!   ([`bridge_mint_state`]) gives the extension commitment of what it mints, which the genesis
//!   check holds to the token's one class, and the bridge covenant id, which the apps show.
//! - **Burned cells.** A burn is a transfer to owner scheme 3 at `Blake2b(6a)` ([`burn_owner`]),
//!   the `p2sh/v1` authority of the script `OP_RETURN` alone. Spending it means revealing a script
//!   that hashes to it, and the only one known always fails, so the tokens can never move again
//!   ([`is_burned`]). The indexer keeps such an output out of the holdings. No payload is needed.

use crate::state::TokenState;

/// Length of the state span of the `BridgeMint` after the 34-byte prefix: three 32-byte fields,
/// each with its push opcode (its own template, the bridge covenant id, the extension commitment).
pub const BRIDGE_MINT_STATE_LEN: usize = 99;

/// The fields of a `BridgeMint` state span.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BridgeMintState {
    /// `gen__bridge_mint_template`: the minter's own Sil template hash (not used by its code).
    pub template: [u8; 32],
    /// The bridge covenant whose input every mint needs. KOB does not check what it is.
    pub bridge_covenant_id: [u8; 32],
    /// The extension commitment of every state it mints.
    pub extension_commitment: [u8; 32],
}

/// The fields of a `BridgeMint` state span, `None` (fail closed) unless it is exactly three direct
/// 32-byte pushes.
pub fn bridge_mint_state(state: &[u8]) -> Option<BridgeMintState> {
    if state.len() != BRIDGE_MINT_STATE_LEN {
        return None;
    }
    let mut f = state.chunks_exact(33).map(|c| (c[0] == 0x20).then(|| <[u8; 32]>::try_from(&c[1..]).expect("32")));
    Some(BridgeMintState { template: f.next()??, bridge_covenant_id: f.next()??, extension_commitment: f.next()?? })
}

/// The extension commitment that a `BridgeMint` state span gives every state it mints (the last
/// field), `None` for any other span.
pub fn bridge_mint_extension(state: &[u8]) -> Option<[u8; 32]> {
    bridge_mint_state(state).map(|s| s.extension_commitment)
}

/// The burn script: `OP_RETURN` alone.
pub const BURN_SCRIPT: [u8; 1] = [0x6a];

/// The burn owner (owner scheme 3): `Blake2b(6a)`, `d379e50f...` (the constant `BURN` of the
/// issuer's `bin/minter-artifact.py`).
pub fn burn_owner() -> [u8; 32] {
    crate::kcc2::p2sh_authority(&BURN_SCRIPT)
}

/// True when `state` is a KCC-20 state of owner scheme 3 at [`burn_owner`]: it can never move.
pub fn is_burned(state: &TokenState) -> bool {
    state.as_kcc20().is_some_and(|k| k.owner_scheme == crate::kcc2::P2SH && k.owner == burn_owner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json::to_hex;
    use crate::state::Kcc20State;

    #[test]
    fn the_burn_owner_is_the_issuers_constant() {
        assert_eq!(to_hex(&burn_owner()), "d379e50f3e2203f9a92584916335bb7cd014b237ceaa8b36f1d3d80d0cc88b32");
        let burned = |scheme: u8, owner: [u8; 32]| {
            TokenState::Kcc20(Kcc20State { owner_scheme: scheme, ..Kcc20State::p2pk(5, owner, [0; 32]) })
        };
        assert!(is_burned(&burned(3, burn_owner())));
        // the same 32 bytes as a key (scheme 0), or another script hash: not a burn
        assert!(!is_burned(&burned(0, burn_owner())));
        assert!(!is_burned(&burned(3, crate::kcc2::p2sh_authority(&[0x6a, 0x00]))));
    }

    #[test]
    fn a_bridge_mint_state_is_three_pushes() {
        let mut s = [[0x20].as_slice(), &[1; 32], &[0x20], &[2; 32], &[0x20], &[3; 32]].concat();
        assert_eq!(
            bridge_mint_state(&s),
            Some(BridgeMintState { template: [1; 32], bridge_covenant_id: [2; 32], extension_commitment: [3; 32] })
        );
        assert_eq!(bridge_mint_extension(&s), Some([3; 32]));
        for at in [0, 33, 66] {
            let mut t = s.clone();
            t[at] = 0x21;
            assert_eq!(bridge_mint_state(&t), None, "{at}");
        }
        assert_eq!(bridge_mint_state(&s[1..]), None);
        s.push(0);
        assert_eq!(bridge_mint_state(&s), None);
        // the kUSDT builds' spans are not this one
        for len in crate::kusdt::BRIDGE_MINT_STATE_LENS {
            assert_eq!(bridge_mint_state(&vec![0x20; len]), None);
        }
    }
}
