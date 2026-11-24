//! Keeper job: the release of an automatic creator lock (`docs/spec/creator-lock-auto.md`).
//!
//! After its time anyone may release an automatic lock: the tokens go whole to the creator's key and
//! the genesis's lock coin back to the creator, less the fee and a tip of at most
//! [`lock_auto::MAX_TAKE`] (the lock script refuses more). The keeper adds one of its own KAS
//! inputs, which comes back on output 2 with [`lock_auto::RELEASE_TIP`]. The fee comes out of the
//! coin.
//!
//! A job is built only when it stays within the keeper's limits: the fee rate is the keeper's floor,
//! the fee at most [`lock_auto::RELEASE_FEE_MAX`] (else the coin could not pay the fee and the tip)
//! and at most the keeper's `max_tx_fee`. The operator never pays anything: its input comes back
//! whole with the tip, and the job is signed for that input only, then validated in the engine.

use std::collections::BTreeSet;

use kob_protocol::build::TokenRef;
use kob_protocol::creator_lock::LockKind;
use kob_protocol::kcc23::VerifiedDeclaration;
use kob_protocol::lock_auto::{self, build_release, ReleaseAutoLock};
use kob_protocol::state::TokenState;
use kob_protocol::tx::{finalize, BuiltTx, FeeOptions, FinalizeOptions, KeyUtxo, SignedTx, TokenUtxo, Utxo};
use kob_protocol::verify::validate_signed;

use crate::matcher::book::{outpoint, CovId, Outpoint};
use crate::matcher::lower::budgets;
use crate::matcher::wallet::Signer;

/// How long after a lock's time a keeper first sends its release, ms: the node takes it only once
/// its past median time (a minute or two behind the clock) passes the lock time.
pub const RELEASE_DELAY_MS: u64 = 180_000;

/// An automatic lock due for release, read from the indexer's verified declaration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DueLock {
    /// The token's covenant id.
    pub token: CovId,
    pub request: ReleaseAutoLock,
}

impl DueLock {
    /// The release of a verified declaration's automatic lock (without a keeper input), or `None`
    /// when it names none.
    pub fn of(d: &VerifiedDeclaration, fee_rate: u64) -> Option<DueLock> {
        let l = d.lock.as_ref().filter(|l| l.kind == LockKind::Auto)?;
        let auto = l.auto()?;
        let coin = l.coin.as_ref()?;
        let (index, state) = l.states().into_iter().next()?;
        let out = l.outputs.iter().find(|o| o.index == index)?;
        let tokens = TokenUtxo {
            utxo: Utxo {
                transaction_id: d.genesis_txid,
                index,
                amount: out.value,
                block_daa_score: 0,
                covenant_id: Some(d.covenant_id),
            },
            state: TokenState::Kcc20(state),
        };
        let coin =
            Utxo { transaction_id: d.genesis_txid, index: coin.index, amount: coin.value, block_daa_score: 0, covenant_id: None };
        Some(DueLock {
            token: d.covenant_id,
            request: ReleaseAutoLock {
                token: TokenRef { covenant_id: d.covenant_id, program: l.program },
                lock: auto.terms(),
                tokens,
                coin,
                keeper: None,
                tip: 0,
                fee: FeeOptions::rate(fee_rate),
            },
        })
    }

    /// The lock's time, unix ms.
    pub fn until(&self) -> u64 {
        self.request.lock.until
    }

    /// The outpoints the release spends besides the keeper's input.
    pub fn outpoints(&self) -> [Outpoint; 2] {
        [outpoint(&self.request.coin), outpoint(&self.request.tokens.utxo)]
    }
}

/// A signed, validated release.
#[derive(Clone, Debug)]
pub struct LockJob {
    pub token: CovId,
    pub built: BuiltTx,
    pub signed: SignedTx,
    /// The operator's profit: the tip.
    pub profit: i64,
    pub spends: BTreeSet<Outpoint>,
}

/// Limits of the release jobs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReleaseLimits {
    /// Sompi per gram.
    pub fee_rate: u64,
    /// The most fee of one transaction (0: no cap but the lock's).
    pub max_tx_fee: u64,
    /// The most releases per tick.
    pub max_jobs: usize,
}

/// The releases of `due` (each due at `now_ms` with [`RELEASE_DELAY_MS`]) that pay the keeper its
/// tip within `limits`, each with its own funding UTXO of the operator. `excluded` are outpoints
/// spent or reserved by the process. Returns the jobs and the locks skipped, with why.
pub fn release_jobs(
    due: &[DueLock],
    funding: &[KeyUtxo],
    now_ms: u64,
    limits: ReleaseLimits,
    excluded: &BTreeSet<Outpoint>,
    signer: &dyn Signer,
) -> (Vec<LockJob>, Vec<(CovId, String)>) {
    let operator = signer.pubkey();
    let mut free: Vec<&KeyUtxo> = funding
        .iter()
        .filter(|k| k.pubkey == operator && k.utxo.covenant_id.is_none() && !excluded.contains(&outpoint(&k.utxo)))
        .collect();
    // the smallest coins first: the input only carries the tip back
    free.sort_by_key(|k| k.utxo.amount);
    let mut jobs = vec![];
    let mut skipped = vec![];
    for d in due {
        if jobs.len() >= limits.max_jobs {
            break;
        }
        if now_ms < d.until().saturating_add(RELEASE_DELAY_MS) {
            continue;
        }
        if d.outpoints().iter().any(|o| excluded.contains(o)) {
            continue;
        }
        let Some(k) = free.first().copied() else {
            skipped.push((d.token, "no funding UTXO of the operator for the keeper input".into()));
            break;
        };
        match build_one(d, k, limits, signer) {
            Ok(j) => {
                free.remove(0);
                jobs.push(j);
            }
            Err(e) => skipped.push((d.token, e)),
        }
    }
    (jobs, skipped)
}

fn build_one(d: &DueLock, k: &KeyUtxo, limits: ReleaseLimits, signer: &dyn Signer) -> Result<LockJob, String> {
    let mut r = d.request.clone();
    r.keeper = Some(k.clone());
    r.tip = lock_auto::RELEASE_TIP;
    r.fee = FeeOptions::rate(limits.fee_rate);
    let built = build_release(&r, &budgets).map_err(|e| e.to_string())?;
    if built.fee.fee > lock_auto::RELEASE_FEE_MAX {
        return Err(format!("fee {} above the {} a release with a tip may take", built.fee.fee, lock_auto::RELEASE_FEE_MAX));
    }
    if limits.max_tx_fee > 0 && built.fee.fee > limits.max_tx_fee {
        return Err(format!("fee {} above the cap {}", built.fee.fee, limits.max_tx_fee));
    }
    // The operator signs its own input only, and gets it back with the tip.
    if built.sign.iter().any(|s| s.pubkey != signer.pubkey()) {
        return Err("the release asks a signature of another key".into());
    }
    // Output 2 counts only when it pays the operator's own P2PK script with no covenant: a release whose
    // output 2 pays anyone else returns nothing to the operator and is skipped (R8F3-3).
    let own = crate::matcher::wallet::p2pk_spk_string(&signer.pubkey());
    let back = built.tx.outputs.get(2).filter(|o| o.script_public_key == own && o.covenant.is_none()).map(|o| o.value).unwrap_or(0);
    let profit = back as i64 - k.utxo.amount as i64;
    if profit < 0 {
        return Err(format!("the release would cost the operator {}", -profit));
    }
    let sigs = signer.sign(&built)?;
    let signed = finalize(&built, &sigs, FinalizeOptions::default()).map_err(|e| e.to_string())?;
    validate_signed(&signed).map_err(|e| format!("engine rejected: {e}"))?;
    let spends = built.tx.inputs.iter().map(|i| (i.transaction_id, i.index)).collect();
    Ok(LockJob { token: d.token, built, signed, profit, spends })
}

#[cfg(test)]
mod tests {
    use super::*;
    use kob_protocol::creator_lock::CreatorLock;
    use kob_protocol::issue::{build_creation, FundingUtxo, Holder, IssueProgram, IssueSpec};
    use kob_protocol::lock_auto::AutoLock;
    use kob_protocol::tx::pubkey_of;

    use crate::matcher::wallet::HotKey;

    const KAS: u64 = 100_000_000;
    const CREATOR: [u8; 32] = [0x41; 32];
    const OPERATOR: [u8; 32] = [0x43; 32];
    const UNTIL: u64 = 1_798_761_600_000;

    /// A declaration of an automatic lock of 200 of 1,000 tokens, verified.
    fn declaration() -> VerifiedDeclaration {
        let key = pubkey_of(&CREATOR).unwrap();
        let lock = CreatorLock::new(UNTIL, key).unwrap();
        let auto = AutoLock::new(lock, IssueProgram::PublicMint.template_id()).unwrap();
        let funding = FundingUtxo {
            outpoint: kaspa_consensus_core::tx::TransactionOutpoint {
                transaction_id: kaspa_consensus_core::tx::TransactionId::from_bytes([7; 32]),
                index: 0,
            },
            amount: 500 * KAS,
            owner_pubkey: key,
        };
        let holders = vec![Holder::new(key, 0, 800), Holder::new(auto.authority(), 0x03, 200)];
        let mut spec = IssueSpec::new("Auto", "AUT", 0, 1000, holders, vec![funding]);
        spec.lock = Some(lock);
        spec.lock_kind = LockKind::Auto;
        let mut plan = build_creation(&spec).unwrap();
        plan.sign(&secp256k1::SecretKey::from_slice(&CREATOR).unwrap()).unwrap();
        plan.launch_proof().verify().unwrap()
    }

    fn operator() -> HotKey {
        HotKey::from_secret(OPERATOR).unwrap()
    }

    fn coin(amount: u64, n: u8) -> KeyUtxo {
        KeyUtxo {
            utxo: Utxo { transaction_id: [n; 32], index: 0, amount, block_daa_score: 1, covenant_id: None },
            pubkey: pubkey_of(&OPERATOR).unwrap(),
        }
    }

    const LIMITS: ReleaseLimits = ReleaseLimits { fee_rate: 100, max_tx_fee: 0, max_jobs: 8 };

    #[test]
    fn a_due_lock_is_released_for_the_tip_and_the_creator_gets_the_rest() {
        let d = declaration();
        let due = DueLock::of(&d, 100).expect("an automatic lock");
        let creator = pubkey_of(&CREATOR).unwrap();
        // not yet: before the time, and within the delay after it
        for now in [UNTIL - 1, UNTIL + RELEASE_DELAY_MS - 1] {
            let (jobs, _) = release_jobs(std::slice::from_ref(&due), &[coin(5 * KAS, 9)], now, LIMITS, &BTreeSet::new(), &operator());
            assert!(jobs.is_empty(), "{now}");
        }
        let (jobs, skipped) = release_jobs(
            std::slice::from_ref(&due),
            &[coin(5 * KAS, 9)],
            UNTIL + RELEASE_DELAY_MS,
            LIMITS,
            &BTreeSet::new(),
            &operator(),
        );
        assert!(skipped.is_empty(), "{skipped:?}");
        let j = &jobs[0];
        assert_eq!(j.profit, lock_auto::RELEASE_TIP as i64);
        assert_eq!(j.built.tx.lock_time, UNTIL);
        let out = &j.built.tx.outputs;
        assert_eq!(out[1].script_public_key, kob_protocol::tx::spk_to_string(&kob_protocol::script::p2pk_spk(&creator)));
        assert_eq!(out[1].value, due.request.coin.amount - lock_auto::RELEASE_TIP - j.built.fee.fee);
        assert_eq!(j.built.sign.len(), 1, "the operator signs only its own input");
        assert!(j.spends.contains(&outpoint(&due.request.coin)) && j.spends.contains(&outpoint(&due.request.tokens.utxo)));
    }

    #[test]
    fn the_keeper_stays_within_its_limits() {
        let d = declaration();
        let due = DueLock::of(&d, 100).unwrap();
        let now = UNTIL + RELEASE_DELAY_MS;
        // a fee cap below the release's fee
        let capped = ReleaseLimits { max_tx_fee: 1_000, ..LIMITS };
        let (jobs, skipped) =
            release_jobs(std::slice::from_ref(&due), &[coin(5 * KAS, 9)], now, capped, &BTreeSet::new(), &operator());
        assert!(jobs.is_empty() && skipped[0].1.contains("cap"), "{skipped:?}");
        // a fee rate whose fee the coin cannot pay with the tip
        let dear = ReleaseLimits { fee_rate: 10_000, ..LIMITS };
        let (jobs, skipped) = release_jobs(std::slice::from_ref(&due), &[coin(5 * KAS, 9)], now, dear, &BTreeSet::new(), &operator());
        assert!(jobs.is_empty() && !skipped.is_empty(), "{skipped:?}");
        // no funding, or the lock's outpoints already spent by the process: nothing
        let (jobs, skipped) = release_jobs(std::slice::from_ref(&due), &[], now, LIMITS, &BTreeSet::new(), &operator());
        assert!(jobs.is_empty() && skipped[0].1.contains("funding"));
        let busy: BTreeSet<Outpoint> = due.outpoints().into_iter().collect();
        let (jobs, _) = release_jobs(std::slice::from_ref(&due), &[coin(5 * KAS, 9)], now, LIMITS, &busy, &operator());
        assert!(jobs.is_empty());
        // one funding UTXO funds one release
        let two = [due.clone(), DueLock { token: [1; 32], ..due.clone() }];
        let (jobs, _) = release_jobs(&two, &[coin(5 * KAS, 9)], now, LIMITS, &BTreeSet::new(), &operator());
        assert_eq!(jobs.len(), 1);
        // a manual lock is no job
        let mut manual = d.clone();
        manual.lock.as_mut().unwrap().kind = LockKind::Manual;
        assert!(DueLock::of(&manual, 100).is_none());
    }
}
