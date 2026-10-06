//! Background balance reads for intermittent mining, on their own node connection, so that a slow full UTXO listing (~30 s for the
//! soak bank's 78k coinbase UTXOs over the internet) never stalls the nonce search.
//!
//! The full listing (exact spendable) is only needed while our own coinbase may still be immature: at the first read, while mining,
//! and for a while after (`recent`, set by the mining loop). Otherwise the cheap total (getBalanceByAddress) is the spendable
//! balance, as long as nothing else pays coinbase to the watched address.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::anyhow;
use tokio::sync::{Notify, watch};
use tokio::task::JoinHandle;

use crate::rpc::{Balance, Rpc};

#[derive(Clone, Debug)]
pub struct PollCtl {
    pub addrs: Vec<String>,
    pub maturity_daa: u64,
    pub poll: Duration,
    /// mining now or recently: list the UTXOs instead of reading the total
    pub recent: bool,
}

pub struct Poller {
    ctl: Arc<Mutex<PollCtl>>,
    kick: Arc<Notify>,
    rx: watch::Receiver<Option<Balance>>,
    task: JoinHandle<()>,
}

impl Poller {
    pub fn spawn(node: String, ctl: PollCtl) -> Self {
        let ctl = Arc::new(Mutex::new(ctl));
        let kick = Arc::new(Notify::new());
        let (tx, rx) = watch::channel(None);
        let (c, k) = (ctl.clone(), kick.clone());
        let task = tokio::spawn(async move {
            let mut rpc: Option<Rpc> = None;
            let mut listed_once = false;
            loop {
                let PollCtl { addrs, maturity_daa, poll, recent } = c.lock().unwrap().clone();
                let list = !listed_once || recent;
                if rpc.is_none() {
                    rpc = Rpc::connect(&node).await.ok();
                }
                let res = match rpc.as_mut() {
                    None => Err(anyhow!("cannot connect to {node}")),
                    Some(r) if list => r.spendable(&addrs, maturity_daa).await,
                    Some(r) => r.total(&addrs).await.map(|t| Balance { spendable: t, ..Default::default() }),
                };
                match res {
                    Ok(b) => {
                        listed_once |= list;
                        if tx.send(Some(b)).is_err() {
                            return;
                        }
                    }
                    Err(e) => {
                        eprintln!("[miner] balance read failed: {e:#}");
                        rpc = None;
                    }
                }
                tokio::select! {
                    _ = tokio::time::sleep(poll) => {}
                    _ = k.notified() => {}
                }
            }
        });
        Self { ctl, kick, rx, task }
    }

    pub fn update(&self, f: impl FnOnce(&mut PollCtl)) {
        f(&mut self.ctl.lock().unwrap());
    }

    /// Read again now (e.g. after a settings change).
    pub fn kick(&self) {
        self.kick.notify_one();
    }

    /// The newest reading if one arrived since the last call.
    pub fn latest(&mut self) -> Option<Balance> {
        if self.rx.has_changed().unwrap_or(false) { *self.rx.borrow_and_update() } else { None }
    }
}

impl Drop for Poller {
    fn drop(&mut self) {
        self.task.abort();
    }
}
