//! Plain KAS payments from P2PK coins: the transactions `kob send KAS` and `kob-executor withdraw` build.
//!
//! A version-1 transaction whose inputs are the builders' P2PK inputs (budget role `p2pk`). [`kas_payment`] pays an amount and
//! sends the change back to the sender; [`kas_sweep`] pays everything the coins hold minus the fee in one output. The fee is
//! the node's relay floor (or the priority rate) at the requested rate, so `fee` equals `min_fee` of the [`FeeReport`] unless a
//! change too small to be an output went to the fee.

use kaspa_consensus_core::subnets::SUBNETWORK_ID_NATIVE;
use kaspa_consensus_core::tx::{ScriptPublicKey, Transaction, TransactionInput, TransactionOutput, UtxoEntry};

use crate::script::p2pk_spk;
use crate::tx::{masses, sighash, target_fee, BuiltTx, FeeOptions, FeeReport, KeyUtxo, SigPlan, SignRequest, TxJson};
use crate::tx::{DUST_OUTPUT_MIN, MIN_FEE_RATE};

/// The inputs of a payment and the transaction around any outputs.
struct Frame<'a> {
    coins: &'a [KeyUtxo],
    entries: Vec<UtxoEntry>,
    budget: u16,
}

impl<'a> Frame<'a> {
    fn new(coins: &'a [KeyUtxo]) -> Result<Self, String> {
        if coins.is_empty() {
            return Err("insufficient funds: no coins".into());
        }
        let budget = crate::budget::lookup("p2pk").map_err(|e| e.to_string())?;
        let entries =
            coins.iter().map(|k| UtxoEntry::new(k.utxo.amount, p2pk_spk(&k.pubkey), k.utxo.block_daa_score, false, None)).collect();
        Ok(Frame { coins, entries, budget })
    }

    /// The transaction with `outs`, every signature script `sig`, its storage mass set.
    fn tx_of(&self, outs: Vec<TransactionOutput>, sig: &[u8]) -> Transaction {
        let ins = self
            .coins
            .iter()
            .map(|k| {
                TransactionInput::new_with_compute_budget(k.utxo.outpoint(), sig.to_vec(), crate::tx::SEQUENCE_NONFINAL, self.budget)
            })
            .collect();
        let tx = Transaction::new(1, ins, outs, 0, SUBNETWORK_ID_NATIVE, 0, vec![]);
        let m = masses(&tx, &self.entries);
        tx.set_storage_mass(m.storage);
        tx
    }

    /// The fee the transaction with `outs` must pay (with a signature-sized placeholder in every input).
    fn need(&self, outs: &[TransactionOutput], fee: &FeeOptions) -> (u64, bool) {
        let t = self.tx_of(outs.to_vec(), &placeholder());
        let report = masses(&t, &self.entries);
        (target_fee(&report, rate_of(fee), fee.fee_mode), report.within_block_limits())
    }

    fn built(&self, outs: Vec<TransactionOutput>, paid: u64, change_output: Option<u32>, fee: &FeeOptions) -> BuiltTx {
        let sized = self.tx_of(outs.clone(), &placeholder());
        let report = masses(&sized, &self.entries);
        let tx = self.tx_of(outs, &[]);
        let sign = (0..self.coins.len())
            .map(|i| SignRequest {
                input_index: i,
                pubkey: self.coins[i].pubkey,
                sighash_type: 1,
                sighash: sighash(&tx, &self.entries, i),
                redeem_script: None,
            })
            .collect();
        let rate = rate_of(fee);
        BuiltTx {
            tx: TxJson::from_tx(&tx, &self.entries),
            plans: self.coins.iter().map(|k| SigPlan::P2pk { pubkey: k.pubkey }).collect(),
            roles: self.coins.iter().map(|_| "p2pk".to_string()).collect(),
            sign,
            fee: FeeReport {
                fee: paid,
                min_fee: target_fee(&report, rate, fee.fee_mode),
                fee_rate: rate,
                fee_mode: fee.fee_mode,
                mass: report,
                change_output,
            },
            covenants: vec![],
        }
    }
}

fn rate_of(fee: &FeeOptions) -> u64 {
    fee.fee_rate.unwrap_or(MIN_FEE_RATE)
}

/// A Schnorr signature plus its sighash byte: the size of the signature script the fee is priced for.
fn placeholder() -> Vec<u8> {
    let mut v = vec![0x41u8];
    v.extend([0u8; 65]);
    v
}

/// A plain KAS payment from P2PK coins of `me` (a version-1 transaction, the builders' P2PK inputs): the payment and
/// the change back to `me` (no change when it would be dust); the fee is the relay floor at the requested rate.
pub fn kas_payment(coins: &[KeyUtxo], amount: u64, to: &ScriptPublicKey, me: &[u8; 32], fee: &FeeOptions) -> Result<BuiltTx, String> {
    let frame = Frame::new(coins)?;
    let total: u64 = coins.iter().map(|k| k.utxo.amount).sum();
    let mine = p2pk_spk(me);
    let pay = TransactionOutput::new(amount, to.clone());
    // with change: the fee is a fixed point (the change's value does not change the relay floor)
    let mut chosen: Option<(Vec<TransactionOutput>, u64, Option<u32>)> = None;
    let mut f = 0u64;
    // whether what is left over is too small to be its own output (only then may it go to the fee)
    let mut change_is_dust = false;
    for _ in 0..16 {
        let Some(change) = total.checked_sub(amount).and_then(|x| x.checked_sub(f)) else {
            change_is_dust = true;
            break;
        };
        if change < DUST_OUTPUT_MIN {
            change_is_dust = true;
            break;
        }
        let outs = vec![pay.clone(), TransactionOutput::new(change, mine.clone())];
        let (need, within) = frame.need(&outs, fee);
        if need <= f {
            if within {
                chosen = Some((outs, f, Some(1)));
            }
            break;
        }
        f = need;
    }
    if chosen.is_none() {
        let outs = vec![pay.clone()];
        let (need, _) = frame.need(&outs, fee);
        if total < amount.saturating_add(need) {
            return Err(format!("insufficient funds: need {} sompi, have {total} sompi", amount.saturating_add(need)));
        }
        // a change that is not dust but did not fit (the transaction with it exceeds a block mass limit, e.g. the KIP-9
        // storage mass of a payment just above the dust bound plus the change) is never paid to the network as fee
        let surplus = total - amount - need;
        if !change_is_dust && surplus >= DUST_OUTPUT_MIN {
            return Err(format!(
                "the change of {surplus} sompi does not fit as its own output (the transaction with it exceeds a block mass \
                 limit) and would otherwise be paid as network fee: send a larger amount"
            ));
        }
        chosen = Some((outs, total - amount, None));
    }
    let (outs, paid, change_output) = chosen.expect("set above");
    Ok(frame.built(outs, paid, change_output, fee))
}

/// Everything the coins hold, minus the fee, to `to` in one output and no change. Refused when the rest would be dust or
/// the transaction exceeds a block mass limit (fewer coins then).
pub fn kas_sweep(coins: &[KeyUtxo], to: &ScriptPublicKey, fee: &FeeOptions) -> Result<BuiltTx, String> {
    let frame = Frame::new(coins)?;
    let total: u64 = coins.iter().map(|k| k.utxo.amount).sum();
    // the fee is a fixed point again: a smaller output has a larger KIP-9 storage mass, which can raise the priced fee
    let mut f = 0u64;
    for _ in 0..16 {
        let Some(value) = total.checked_sub(f).filter(|v| *v >= DUST_OUTPUT_MIN) else {
            return Err(format!(
                "insufficient funds: {total} sompi do not cover the fee and an output of at least {DUST_OUTPUT_MIN} sompi"
            ));
        };
        let outs = vec![TransactionOutput::new(value, to.clone())];
        let (need, within) = frame.need(&outs, fee);
        if need <= f {
            if !within {
                return Err("the transaction exceeds a block mass limit: sweep fewer coins".into());
            }
            return Ok(frame.built(outs, f, None, fee));
        }
        f = need;
    }
    Err("the fee did not settle".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tx::Utxo;

    const KAS: u64 = 100_000_000;

    fn coin(n: u8, amount: u64) -> KeyUtxo {
        KeyUtxo {
            utxo: Utxo { transaction_id: [n; 32], index: 0, amount, block_daa_score: 500, covenant_id: None },
            pubkey: crate::tx::pubkey_of(&[0x11; 32]).unwrap(),
        }
    }

    #[test]
    fn a_sweep_pays_everything_but_the_fee_in_one_output() {
        let to = p2pk_spk(&[7; 32]);
        let coins = [coin(1, 3 * KAS), coin(2, 2 * KAS)];
        let b = kas_sweep(&coins, &to, &FeeOptions::default()).unwrap();
        assert_eq!((b.tx.outputs.len(), b.tx.inputs.len(), b.fee.change_output), (1, 2, None));
        assert_eq!(b.fee.fee, b.fee.min_fee);
        let out: u64 = b.tx.outputs.iter().map(|o| o.value).sum();
        assert_eq!(out + b.fee.fee, 5 * KAS);
        // a higher rate pays more, still all of the rest
        let hi = kas_sweep(&coins, &to, &FeeOptions::rate(1_000)).unwrap();
        assert!(hi.fee.fee > b.fee.fee);
        assert_eq!(hi.tx.outputs[0].value + hi.fee.fee, 5 * KAS);
    }

    #[test]
    fn a_sweep_of_dust_or_nothing_is_refused() {
        let to = p2pk_spk(&[7; 32]);
        assert!(kas_sweep(&[], &to, &FeeOptions::default()).is_err());
        let e = kas_sweep(&[coin(1, DUST_OUTPUT_MIN)], &to, &FeeOptions::default()).unwrap_err();
        assert!(e.contains("insufficient funds"), "{e}");
    }
}
