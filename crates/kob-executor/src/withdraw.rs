//! `kob-executor withdraw`: sends KAS out of the operator's key, over the node the configuration names.
//!
//! The default operator has no node of their own: the configuration says `rpc_url = "resolver"` (two public nodes found
//! through the Kaspa resolvers, wss), which `kob-cli` cannot reach. The executor already speaks that, so the withdrawal is a
//! command of the executor: it reads the configuration and the key file exactly as `address-balance` does, lists the plain
//! (covenant-free) UTXOs of the key's address at the configured node, builds one version-1 payment (`kob_protocol::payment`),
//! signs it with the operator key, and submits it to that node.
//!
//! Safe next to the executor by refusing it: the command takes the data directory's single-writer lock
//! (`indexer::lock`), the same one `run` holds for as long as it runs. A running executor makes it fail with that lock's
//! message ("stop it first"), so no matcher transaction is being built from the coins it spends, and two withdrawals cannot
//! overlap either. A transaction the stopped executor sent just before the stop may still sit in the node's mempool: a coin it
//! spent is then refused by the node as a double spend, and nothing is lost.
//!
//! Network safety is that of every command: the network is the configuration's, resolved against the build
//! ([`crate::config::resolve_network`]: a deployment binary refuses the other network), the node must report it, and the
//! destination must be an address of it.
//!
//! The secret never leaves the key file. Only plain KAS moves: the operator's token UTXOs and the orders' coins are not
//! touched.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use kob_protocol::payment::{kas_payment, kas_sweep};
use kob_protocol::tx::{finalize, FeeOptions, FinalizeOptions, KeyUtxo, Utxo, DUST_OUTPUT_MIN, MIN_FEE_RATE};
use serde_json::{json, Value};

use crate::config::{default_wrpc_url, IndexerConfig};
use crate::indexer::lock::WriterLock;
use crate::matcher::node::{classify_submit_error, AddressUtxo, NodeApi, RpcError, ServerInfo, SubmitOutcome, WrpcConfig, WrpcNode};
use crate::matcher::run::kas_text;
use crate::matcher::wallet::{p2pk_spk_string, HotKey, Signer};
use crate::setup::{Console, CONFIG_FILE};

/// Most coins one withdrawal spends (one transaction, the largest first); a key spread over more coins needs a second run.
pub const MAX_INPUTS: usize = 50;

/// `kob-executor withdraw`.
#[derive(clap::Args, Debug, Clone)]
pub struct WithdrawArgs {
    /// The address to send to (an address of the executor's network).
    #[arg(long)]
    pub to: String,
    /// KAS to send (`2.5`, `2.5KAS`), or `all`: everything the key holds, minus the fee.
    #[arg(long)]
    pub amount: String,
    /// Directory of `kob-executor.toml` and the key file. Default: `$KOB_EXECUTOR_DIR` or the executable's own directory.
    #[arg(long, conflicts_with = "config")]
    pub dir: Option<PathBuf>,
    /// The configuration file, instead of `--dir` (its directory is the base of its relative paths).
    #[arg(long)]
    pub config: Option<PathBuf>,
    /// The key file, instead of the configuration's (else the environment: `KOB_OPERATOR_KEY_FILE`, a systemd credential).
    #[arg(long)]
    pub key_file: Option<PathBuf>,
    /// Fee rate, sompi per gram of mass (at least 100, the node's relay floor).
    #[arg(long, default_value_t = MIN_FEE_RATE)]
    pub fee_rate: u64,
    /// Send without asking. Without it the command asks to confirm, and refuses to run without a terminal.
    #[arg(long, short = 'y')]
    pub yes: bool,
    /// Build and show the withdrawal, send nothing.
    #[arg(long)]
    pub dry_run: bool,
}

/// How much to send.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Amount {
    /// Everything the spent coins hold, minus the fee.
    All,
    Sompi(u64),
}

/// `all`, or KAS with up to eight decimals and an optional `KAS` suffix (`2`, `2.5`, `2.5KAS`).
pub fn parse_amount(text: &str) -> Result<Amount> {
    let t = text.trim();
    if t.eq_ignore_ascii_case("all") {
        return Ok(Amount::All);
    }
    let digits = match t.len().checked_sub(3).filter(|i| t.is_char_boundary(*i)) {
        Some(i) if t[i..].eq_ignore_ascii_case("kas") => t[..i].trim_end(),
        _ => t,
    };
    let bad = || anyhow!("--amount `{text}` is not `all` or an amount of KAS such as 2.5");
    let (int, frac) = digits.split_once('.').unwrap_or((digits, ""));
    if int.is_empty() && frac.is_empty() || !int.chars().all(|c| c.is_ascii_digit()) || !frac.chars().all(|c| c.is_ascii_digit()) {
        return Err(bad());
    }
    if frac.len() > 8 {
        bail!("--amount `{text}` has more than eight decimals (a sompi is 0.00000001 KAS)");
    }
    let whole: u64 = if int.is_empty() { 0 } else { int.parse().map_err(|_| bad())? };
    let part: u64 = if frac.is_empty() { 0 } else { format!("{frac:0<8}").parse().map_err(|_| bad())? };
    let sompi = whole.checked_mul(100_000_000).and_then(|w| w.checked_add(part)).ok_or_else(bad)?;
    if sompi == 0 {
        bail!("--amount is zero");
    }
    if sompi < DUST_OUTPUT_MIN {
        bail!("--amount is below {} KAS, the smallest output the network takes", kas_text(DUST_OUTPUT_MIN as i64));
    }
    Ok(Amount::Sompi(sompi))
}

/// The node calls of a withdrawal.
pub trait WithdrawNode {
    fn server_info(&mut self, url: &str) -> Result<ServerInfo, RpcError>;
    /// The UTXOs of `address` at the node's UTXO index.
    fn utxos(&mut self, url: &str, address: &str) -> Result<Vec<AddressUtxo>, RpcError>;
    /// `submitTransaction`: the transaction id, or the node's refusal.
    fn submit(&mut self, url: &str, tx: Value) -> Result<[u8; 32], RpcError>;
}

/// The real node: one connection for the whole command (a resolver entry keeps the public node it found), on the caller's
/// runtime.
pub struct WrpcWithdrawNode<'a> {
    pub rt: &'a tokio::runtime::Runtime,
    node: Option<WrpcNode>,
}

impl<'a> WrpcWithdrawNode<'a> {
    pub fn new(rt: &'a tokio::runtime::Runtime) -> Self {
        WrpcWithdrawNode { rt, node: None }
    }

    fn node(&mut self, url: &str) -> &WrpcNode {
        self.node.get_or_insert_with(|| {
            let mut c = WrpcConfig::new(url);
            c.connect_timeout = std::time::Duration::from_secs(10);
            c.request_timeout = std::time::Duration::from_secs(30);
            WrpcNode::new(c)
        })
    }
}

impl WithdrawNode for WrpcWithdrawNode<'_> {
    fn server_info(&mut self, url: &str) -> Result<ServerInfo, RpcError> {
        let rt = self.rt;
        rt.block_on(self.node(url).server_info())
    }
    fn utxos(&mut self, url: &str, address: &str) -> Result<Vec<AddressUtxo>, RpcError> {
        let rt = self.rt;
        rt.block_on(self.node(url).utxos_by_addresses(&[address.to_string()]))
    }
    fn submit(&mut self, url: &str, tx: Value) -> Result<[u8; 32], RpcError> {
        let rt = self.rt;
        rt.block_on(self.node(url).submit(tx))
    }
}

/// What the command is for (`withdraw`'s arguments, resolved).
#[derive(Debug, Clone)]
pub struct Request {
    pub to: String,
    pub amount: Amount,
    pub key_file: Option<PathBuf>,
    pub fee_rate: u64,
    pub yes: bool,
    /// Whether a person can answer the confirmation (standard input is a terminal).
    pub interactive: bool,
    pub dry_run: bool,
}

/// The configuration file and its base directory for the arguments.
pub fn config_location(a: &WithdrawArgs) -> Result<(PathBuf, PathBuf)> {
    if let Some(c) = &a.config {
        let dir = c.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new(".")).to_path_buf();
        return Ok((c.clone(), dir));
    }
    let dir = match &a.dir {
        Some(d) => d.clone(),
        None => crate::setup::home_dir()?,
    };
    Ok((dir.join(CONFIG_FILE), dir))
}

fn under(dir: &Path, p: &Path) -> PathBuf {
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        dir.join(p)
    }
}

/// The withdrawal: builds, shows, confirms and sends. Returns `{txid, to, amount_sompi, fee_sompi, inputs, remaining_coins, ...}`
/// (`txid` null for a dry run). Never prints or returns the secret.
pub fn withdraw(cfg_path: &Path, base: &Path, req: &Request, node: &mut dyn WithdrawNode, io: &mut dyn Console) -> Result<Value> {
    let mut cfg = IndexerConfig::load(cfg_path)?;
    let launch_key = cfg.launch.as_ref().and_then(|l| l.key_file.clone());
    if cfg.launch.as_ref().is_some_and(|l| l.key_file.is_none()) && req.key_file.is_none() {
        bail!("{} has no operator key (index-only role): there is nothing to withdraw", cfg_path.display());
    }
    let network = crate::config::resolve_network(Some(&cfg.network)).map_err(|e| anyhow!(e))?;
    let key = match (&req.key_file, launch_key) {
        (Some(p), _) => HotKey::load(Some(p))?,
        (None, Some(p)) => HotKey::load(Some(&under(base, &p)))?,
        (None, None) => HotKey::load(None)?,
    };
    let pubkey = key.pubkey();
    let from = key.address(&network).to_string();

    // the destination: an address of this network, not the key's own
    let dest = kaspa_addresses::Address::try_from(req.to.as_str()).map_err(|e| anyhow!("--to `{}` is not an address: {e}", req.to))?;
    let want = crate::matcher::wallet::address_of(&pubkey, &network).prefix;
    if dest.prefix != want {
        bail!("--to `{}` is an address of another network than {network} (expected `{}:...`)", req.to, want);
    }
    if dest.to_string() == from {
        bail!("--to is the executor's own address");
    }
    if req.fee_rate < MIN_FEE_RATE {
        bail!("--fee-rate must be at least {MIN_FEE_RATE} sompi per gram");
    }
    let fee = FeeOptions::rate(req.fee_rate);

    // the executor and a withdrawal never write at once: the data directory's single-writer lock, held until the end
    cfg.data_dir = under(base, &cfg.data_dir);
    if let Some(r) = cfg.records.dir.take() {
        cfg.records.dir = Some(under(base, &r));
    }
    if cfg.lean {
        cfg.records.enabled = false;
    }
    let _lock = WriterLock::acquire(&cfg).map_err(|e| {
        anyhow!("{e}\nThe executor holds this directory while it runs: stop it (Ctrl+C, or stop the service), then withdraw.")
    })?;

    let url = match cfg.primary_url().trim() {
        "" => default_wrpc_url(&network)
            .ok_or_else(|| anyhow!("no node endpoint for {network}: set rpc_url in {}", cfg_path.display()))?,
        u => u.to_string(),
    };
    if crate::rpc::resolver::is_resolver(&url) {
        crate::rpc::resolver::set_network(&network);
    }
    let shown = crate::rpc::redact_url(&url);
    let info = node.server_info(&url).map_err(|e| anyhow!("cannot reach the node {shown}: {e}"))?;
    if info.network_id != network {
        bail!("the node {shown} is on {}, this is the {network} executor: nothing was sent", info.network_id);
    }
    if !info.is_synced {
        bail!("the node {shown} is not synced yet, so its view of the coins is not current: try again in a few minutes");
    }
    if !info.has_utxo_index {
        bail!("the node {shown} runs without a UTXO index (--utxoindex), so it cannot list the executor's coins");
    }

    let spk = p2pk_spk_string(&pubkey);
    let mut coins: Vec<KeyUtxo> = node
        .utxos(&url, &from)
        .map_err(|e| anyhow!("cannot list the coins of {from} at {shown}: {e}"))?
        .into_iter()
        .filter(|u| u.covenant_id.is_none() && u.script_public_key == spk)
        .map(|u| KeyUtxo {
            utxo: Utxo {
                transaction_id: u.transaction_id,
                index: u.index,
                amount: u.amount,
                block_daa_score: u.block_daa_score,
                covenant_id: None,
            },
            pubkey,
        })
        .collect();
    coins.sort_by(|a, b| b.utxo.amount.cmp(&a.utxo.amount));
    let held: u64 = coins.iter().map(|c| c.utxo.amount).sum();
    if coins.is_empty() {
        bail!("{from} holds no plain KAS at {shown}: nothing to withdraw");
    }
    let dest_spk = kaspa_txscript::pay_to_address_script(&dest);
    let (built, used) = match req.amount {
        Amount::All => {
            let used = coins.len().min(MAX_INPUTS);
            (kas_sweep(&coins[..used], &dest_spk, &fee).map_err(|e| anyhow!(e))?, used)
        }
        Amount::Sompi(sompi) => {
            let mut last = String::new();
            let mut found = None;
            for n in 1..=coins.len().min(MAX_INPUTS) {
                match kas_payment(&coins[..n], sompi, &dest_spk, &pubkey, &fee) {
                    Ok(b) => {
                        found = Some((b, n));
                        break;
                    }
                    Err(e) => last = e,
                }
            }
            found.ok_or_else(|| {
                anyhow!(
                    "cannot send {} KAS: {last} (the key holds {} KAS in {} coin(s); a transaction spends at most {MAX_INPUTS})",
                    kas_text(sompi as i64),
                    kas_text(held as i64),
                    coins.len()
                )
            })?
        }
    };
    let sent: u64 = built.tx.outputs.first().map(|o| o.value).unwrap_or(0);
    let change: u64 = built.tx.outputs.get(1).map(|o| o.value).unwrap_or(0);
    let rest = coins.len() - used;

    io.say("Withdraw from the executor key");
    io.say(&format!("  network:   {network}"));
    io.say(&format!("  from:      {from}"));
    io.say(&format!("  to:        {}", req.to));
    io.say(&format!("  amount:    {} KAS", kas_text(sent as i64)));
    io.say(&format!("  fee:       {} KAS ({} sompi per gram)", kas_text(built.fee.fee as i64), req.fee_rate));
    io.say(&format!(
        "  spends:    {used} of {} coin(s), {} KAS",
        coins.len(),
        kas_text((held - coins[used..].iter().map(|c| c.utxo.amount).sum::<u64>()) as i64)
    ));
    io.say(&format!("  stays:     {} KAS on the key", kas_text((held - sent - built.fee.fee) as i64)));
    io.say(&format!("  node:      {shown}"));
    if rest > 0 && req.amount == Amount::All {
        io.say(&format!(
            "  note:      {rest} smaller coin(s) are left; run the command again once this one is accepted to move them too"
        ));
    }
    if change == 0 && matches!(req.amount, Amount::Sompi(_)) {
        io.say("  note:      what was left over was too small to be an output and goes to the fee");
    }
    let mut out = json!({
        "txid": null,
        "network": network,
        "from": from,
        "to": req.to,
        "amount_sompi": sent,
        "fee_sompi": built.fee.fee,
        "inputs": used,
        "remaining_coins": rest,
    });
    if req.dry_run {
        io.say("Dry run: nothing was sent.");
        return Ok(out);
    }
    if !req.yes {
        if !req.interactive {
            bail!(
                "nothing was sent: this needs a confirmation and there is no terminal to ask on (pass --yes to send without asking)"
            );
        }
        io.say("Check the address: a transaction cannot be taken back. The executor must be stopped; it will warn about low funds when it starts.");
        let a = io.ask("Send it? Type yes to confirm: ").ok_or_else(|| anyhow!("the input ended: nothing was sent"))?;
        if !matches!(a.to_ascii_lowercase().as_str(), "y" | "yes") {
            bail!("not confirmed: nothing was sent");
        }
    }

    let sigs = key.sign(&built).map_err(|e| anyhow!(e))?;
    let signed = finalize(&built, &sigs, FinalizeOptions::default()).context("signing the withdrawal")?;
    let (tx, _) = signed.tx.to_tx()?;
    let rpc = kob_protocol::issue::rpc_transaction_json(&tx);
    let txid = submit(node, &url, &rpc, signed.tx.id, &shown)?;
    let txid_hex = kob_protocol::json::to_hex(&txid);
    io.say(&format!("Sent: transaction {txid_hex}"));
    io.say("The node has it; it is final within seconds. `kob-executor address-balance` shows what the key holds then.");
    out["txid"] = json!(txid_hex);
    Ok(out)
}

/// `submitTransaction`, again after a failure on the wire (the same signed transaction: an answer that it is already known counts
/// as sent).
fn submit(node: &mut dyn WithdrawNode, url: &str, rpc: &Value, id: [u8; 32], shown: &str) -> Result<[u8; 32]> {
    let mut last = String::new();
    for attempt in 0..3 {
        match node.submit(url, rpc.clone()) {
            Ok(sent) => return Ok(sent),
            Err(RpcError::Node(msg)) => match classify_submit_error(&msg) {
                SubmitOutcome::AlreadyKnown => return Ok(id),
                SubmitOutcome::DoubleSpend | SubmitOutcome::MissingInput => bail!(
                    "the node {shown} refused the transaction: a coin of the key is already spent (by a transaction the executor sent before it \
                     stopped, still on its way?). Nothing was lost; wait a minute and run the command again. The node said: {msg}"
                ),
                _ => bail!("the node {shown} refused the transaction: {msg}"),
            },
            Err(e) => {
                last = e.to_string();
                if attempt < 2 {
                    std::thread::sleep(std::time::Duration::from_secs(2));
                }
            }
        }
    }
    bail!(
        "the transaction may or may not have reached {shown} ({last}). Check `kob-executor address-balance` before running the command \
         again: a second run would spend the same coins and be refused if the first went through"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::setup::Poll;
    use std::collections::VecDeque;
    use std::time::Duration;

    #[derive(Default)]
    struct Script {
        said: Vec<String>,
        answers: VecDeque<String>,
        asked: usize,
    }

    impl Console for Script {
        fn say(&mut self, line: &str) {
            self.said.push(line.to_string());
        }
        fn ask(&mut self, _prompt: &str) -> Option<String> {
            self.asked += 1;
            self.answers.pop_front()
        }
        fn poll(&mut self, _wait: Duration) -> Poll {
            Poll::Closed
        }
    }

    struct Fake {
        info: ServerInfo,
        coins: Vec<AddressUtxo>,
        submitted: Vec<Value>,
        submit_result: Vec<Result<[u8; 32], RpcError>>,
    }

    impl WithdrawNode for Fake {
        fn server_info(&mut self, _url: &str) -> Result<ServerInfo, RpcError> {
            Ok(self.info.clone())
        }
        fn utxos(&mut self, _url: &str, _address: &str) -> Result<Vec<AddressUtxo>, RpcError> {
            Ok(self.coins.clone())
        }
        fn submit(&mut self, _url: &str, tx: Value) -> Result<[u8; 32], RpcError> {
            self.submitted.push(tx);
            if self.submit_result.is_empty() {
                Ok([9; 32])
            } else {
                self.submit_result.remove(0)
            }
        }
    }

    const KAS: u64 = 100_000_000;

    struct Fx {
        dir: tempfile::TempDir,
        pubkey: [u8; 32],
        network: String,
    }

    fn fixture() -> Fx {
        let dir = tempfile::tempdir().unwrap();
        let network = kob_protocol::artifacts::deployment_network().unwrap_or("testnet-10").to_string();
        let o = crate::app::setup_non_interactive(
            &crate::setup::SetupOptions {
                dir: dir.path().to_path_buf(),
                build_network: Some(network.clone()),
                pinned_registry: true,
                poll: Duration::ZERO,
                program: "kob-executor".into(),
            },
            &crate::app::AppSetup { node: "resolver".into(), role: crate::app::AppRole::Match, new_key: true, network: None },
        )
        .unwrap();
        assert!(o["address"].is_string());
        let key = HotKey::from_file(&dir.path().join(crate::setup::KEY_FILE)).unwrap();
        Fx { pubkey: key.pubkey(), dir, network }
    }

    impl Fx {
        fn coin(&self, n: u8, amount: u64) -> AddressUtxo {
            AddressUtxo {
                transaction_id: [n; 32],
                index: 0,
                amount,
                script_public_key: p2pk_spk_string(&self.pubkey),
                block_daa_score: 1,
                covenant_id: None,
            }
        }
        fn node(&self, coins: Vec<AddressUtxo>) -> Fake {
            Fake {
                info: ServerInfo {
                    server_version: "2.1.0".into(),
                    network_id: self.network.clone(),
                    is_synced: true,
                    has_utxo_index: true,
                    virtual_daa_score: 10,
                },
                coins,
                submitted: vec![],
                submit_result: vec![],
            }
        }
        fn dest(&self) -> String {
            crate::matcher::wallet::address_of(&[5; 32], &self.network).to_string()
        }
        fn req(&self, amount: Amount) -> Request {
            Request { to: self.dest(), amount, key_file: None, fee_rate: MIN_FEE_RATE, yes: true, interactive: false, dry_run: false }
        }
        fn run(&self, req: &Request, node: &mut Fake, io: &mut Script) -> Result<Value> {
            withdraw(&self.dir.path().join(CONFIG_FILE), self.dir.path(), req, node, io)
        }
    }

    #[test]
    fn amounts_parse_as_kas() {
        assert_eq!(parse_amount("all").unwrap(), Amount::All);
        assert_eq!(parse_amount(" ALL ").unwrap(), Amount::All);
        assert_eq!(parse_amount("2.5").unwrap(), Amount::Sompi(250_000_000));
        assert_eq!(parse_amount("2.5KAS").unwrap(), Amount::Sompi(250_000_000));
        assert_eq!(parse_amount("2 kas").unwrap(), Amount::Sompi(200_000_000));
        assert_eq!(parse_amount("0.02").unwrap(), Amount::Sompi(2_000_000));
        assert_eq!(parse_amount(".5").unwrap(), Amount::Sompi(50_000_000));
        for bad in ["", "kas", "x", "-1", "1e3", "0", "0.00000001", "0.019", "1.123456789", "1,5", "99999999999999999999"] {
            assert!(parse_amount(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_withdrawal_spends_the_largest_coins_and_sends_the_exact_amount() {
        let fx = fixture();
        let mut node = fx.node(vec![fx.coin(1, 3 * KAS), fx.coin(2, 10 * KAS), fx.coin(3, KAS)]);
        let mut io = Script::default();
        let out = fx.run(&fx.req(Amount::Sompi(4 * KAS)), &mut node, &mut io).unwrap();
        assert_eq!(out["txid"], kob_protocol::json::to_hex(&[9; 32]));
        assert_eq!(out["amount_sompi"], 4 * KAS);
        assert_eq!(out["inputs"], 1, "the 10 KAS coin alone covers it");
        assert_eq!(node.submitted.len(), 1);
        let tx = &node.submitted[0];
        assert_eq!(tx["version"], 1);
        let outputs = tx["outputs"].as_array().unwrap();
        assert_eq!(outputs.len(), 2, "the payment and the change");
        assert_eq!(outputs[0]["value"], 4 * KAS);
        let fee = out["fee_sompi"].as_u64().unwrap();
        assert_eq!(outputs[1]["value"].as_u64().unwrap() + 4 * KAS + fee, 10 * KAS);
        // the input is signed (a 65-byte Schnorr signature push) and is the coin picked
        assert_eq!(tx["inputs"].as_array().unwrap().len(), 1);
        assert!(!tx["inputs"][0]["signatureScript"].as_str().unwrap().is_empty());
        // the secret is in no line printed or returned
        let secret = std::fs::read_to_string(fx.dir.path().join(crate::setup::KEY_FILE)).unwrap();
        assert!(!io.said.join("\n").contains(secret.trim()) && !out.to_string().contains(secret.trim()));
        assert!(io.said.iter().any(|l| l.contains(&fx.dest())));
    }

    #[test]
    fn all_sends_everything_but_the_fee_and_reports_coins_left_over() {
        let fx = fixture();
        let coins: Vec<_> = (1..=(MAX_INPUTS as u8 + 3)).map(|n| fx.coin(n, 2 * KAS)).collect();
        let total_used = MAX_INPUTS as u64 * 2 * KAS;
        let mut node = fx.node(coins);
        let mut io = Script::default();
        let out = fx.run(&fx.req(Amount::All), &mut node, &mut io).unwrap();
        assert_eq!((out["inputs"].as_u64(), out["remaining_coins"].as_u64()), (Some(MAX_INPUTS as u64), Some(3)));
        let outputs = node.submitted[0]["outputs"].as_array().unwrap();
        assert_eq!(outputs.len(), 1, "no change");
        assert_eq!(outputs[0]["value"].as_u64().unwrap() + out["fee_sompi"].as_u64().unwrap(), total_used);
        assert!(io.said.iter().any(|l| l.contains("3 smaller coin(s) are left")), "{:?}", io.said);
    }

    #[test]
    fn token_coins_and_foreign_scripts_are_never_spent() {
        let fx = fixture();
        let mut token = fx.coin(1, 50 * KAS);
        token.covenant_id = Some([7; 32]);
        let mut foreign = fx.coin(2, 50 * KAS);
        foreign.script_public_key = "0000ab".into();
        let mut node = fx.node(vec![token.clone(), foreign.clone()]);
        let e = fx.run(&fx.req(Amount::All), &mut node, &mut Script::default()).unwrap_err();
        assert!(e.to_string().contains("no plain KAS"), "{e}");
        let mut node = fx.node(vec![token, foreign, fx.coin(3, 5 * KAS)]);
        let out = fx.run(&fx.req(Amount::All), &mut node, &mut Script::default()).unwrap();
        assert_eq!(out["inputs"], 1);
        assert!(out["amount_sompi"].as_u64().unwrap() < 5 * KAS);
    }

    #[test]
    fn it_refuses_the_other_network_a_wrong_address_and_a_node_that_cannot_answer() {
        let fx = fixture();
        let other = if fx.network == "mainnet" { "testnet-10" } else { "mainnet" };
        let mut io = Script::default();
        // a destination of the other network
        let mut r = fx.req(Amount::All);
        r.to = crate::matcher::wallet::address_of(&[5; 32], other).to_string();
        let mut node = fx.node(vec![fx.coin(1, 5 * KAS)]);
        let e = fx.run(&r, &mut node, &mut io).unwrap_err();
        assert!(e.to_string().contains("another network"), "{e}");
        // not an address; the key's own address
        r.to = "kaspa:nonsense".into();
        assert!(fx.run(&r, &mut node, &mut io).is_err());
        r.to = crate::matcher::wallet::address_of(&fx.pubkey, &fx.network).to_string();
        assert!(fx.run(&r, &mut node, &mut io).unwrap_err().to_string().contains("own address"));
        // a node of the other network, an unsynced node, a node without a UTXO index
        let mut node = fx.node(vec![fx.coin(1, 5 * KAS)]);
        node.info.network_id = other.into();
        assert!(fx.run(&fx.req(Amount::All), &mut node, &mut io).unwrap_err().to_string().contains(&format!("is on {other}")));
        node.info.network_id = fx.network.clone();
        node.info.is_synced = false;
        assert!(fx.run(&fx.req(Amount::All), &mut node, &mut io).unwrap_err().to_string().contains("not synced"));
        node.info.is_synced = true;
        node.info.has_utxo_index = false;
        assert!(fx.run(&fx.req(Amount::All), &mut node, &mut io).unwrap_err().to_string().contains("UTXO index"));
        assert!(node.submitted.is_empty(), "nothing was sent by any of these");
        // a configuration of the other network than this binary's deployment network is refused as every command does
        if let Some(b) = kob_protocol::artifacts::deployment_network() {
            let p = fx.dir.path().join(CONFIG_FILE);
            let text = std::fs::read_to_string(&p).unwrap().replace(&format!("\"{b}\""), &format!("\"{other}\""));
            std::fs::write(&p, text).unwrap();
            let mut node = fx.node(vec![fx.coin(1, 5 * KAS)]);
            let e = fx.run(&fx.req(Amount::All), &mut node, &mut io).unwrap_err();
            assert!(e.to_string().contains("deployment build"), "{e}");
            assert!(node.submitted.is_empty());
        }
    }

    #[test]
    fn it_asks_unless_told_not_to_and_never_sends_without_an_answer() {
        let fx = fixture();
        let coins = vec![fx.coin(1, 5 * KAS)];
        // no --yes and no terminal: refused
        let mut r = fx.req(Amount::All);
        r.yes = false;
        let mut node = fx.node(coins.clone());
        let mut io = Script::default();
        let e = fx.run(&r, &mut node, &mut io).unwrap_err();
        assert!(e.to_string().contains("--yes"), "{e}");
        assert_eq!((node.submitted.len(), io.asked), (0, 0));
        // a terminal: asked; anything but yes sends nothing
        r.interactive = true;
        for answer in ["", "n", "no", "yess", "send"] {
            let mut io = Script { answers: VecDeque::from([answer.to_string()]), ..Script::default() };
            assert!(fx.run(&r, &mut node, &mut io).is_err(), "{answer}");
            assert_eq!(io.asked, 1);
        }
        let mut io = Script::default();
        assert!(fx.run(&r, &mut node, &mut io).is_err(), "the input ended");
        assert!(node.submitted.is_empty());
        // yes sends
        for answer in ["yes", "Y"] {
            let mut node = fx.node(coins.clone());
            let mut io = Script { answers: VecDeque::from([answer.to_string()]), ..Script::default() };
            fx.run(&r, &mut node, &mut io).unwrap();
            assert_eq!(node.submitted.len(), 1, "{answer}");
        }
        // --yes does not ask
        let mut node = fx.node(coins.clone());
        let mut io = Script::default();
        fx.run(&fx.req(Amount::All), &mut node, &mut io).unwrap();
        assert_eq!((node.submitted.len(), io.asked), (1, 0));
        // a dry run builds and shows, sends nothing, asks nothing
        let mut d = fx.req(Amount::All);
        d.yes = false;
        d.dry_run = true;
        let mut node = fx.node(coins);
        let mut io = Script::default();
        let out = fx.run(&d, &mut node, &mut io).unwrap();
        assert!(out["txid"].is_null() && node.submitted.is_empty() && io.asked == 0);
    }

    #[test]
    fn a_running_executor_makes_it_refuse_and_a_stopped_one_does_not() {
        let fx = fixture();
        let mut cfg = IndexerConfig::load(&fx.dir.path().join(CONFIG_FILE)).unwrap();
        cfg.data_dir = fx.dir.path().join(&cfg.data_dir);
        cfg.records.enabled = false;
        let held = WriterLock::acquire(&cfg).unwrap();
        let mut node = fx.node(vec![fx.coin(1, 5 * KAS)]);
        let e = fx.run(&fx.req(Amount::All), &mut node, &mut Script::default()).unwrap_err();
        assert!(e.to_string().contains("in use by another kob-executor writer") && e.to_string().contains("stop it"), "{e}");
        assert!(node.submitted.is_empty());
        drop(held);
        fx.run(&fx.req(Amount::All), &mut node, &mut Script::default()).unwrap();
        assert_eq!(node.submitted.len(), 1);
        // the lock is released when the command ends
        assert!(WriterLock::acquire(&cfg).is_ok());
    }

    #[test]
    fn a_refused_or_lost_submission_says_what_to_do() {
        let fx = fixture();
        let coins = vec![fx.coin(1, 5 * KAS)];
        let run = |results: Vec<Result<[u8; 32], RpcError>>| {
            let mut node = fx.node(coins.clone());
            node.submit_result = results;
            let r = fx.run(&fx.req(Amount::All), &mut node, &mut Script::default());
            (r, node.submitted.len())
        };
        let (r, n) = run(vec![Err(RpcError::Node("Transaction x is not standard: output is dust".into()))]);
        assert!(r.unwrap_err().to_string().contains("refused the transaction"));
        assert_eq!(n, 1);
        let (r, _) = run(vec![Err(RpcError::Node("Transaction y is already spent by transaction z in the mempool".into()))]);
        let m = r.unwrap_err().to_string();
        assert!(m.contains("already spent") && m.contains("Nothing was lost"), "{m}");
        // a failure on the wire is retried with the same transaction, then reported as unknown
        let (r, n) = run(vec![Err(RpcError::Transport("reset".into())), Ok([4; 32])]);
        assert_eq!(r.unwrap()["txid"], kob_protocol::json::to_hex(&[4; 32]));
        assert_eq!(n, 2);
    }

    #[test]
    fn index_only_and_a_missing_configuration_are_errors() {
        let fx = fixture();
        crate::app::setup_non_interactive(
            &crate::setup::SetupOptions {
                dir: fx.dir.path().to_path_buf(),
                build_network: Some(fx.network.clone()),
                pinned_registry: true,
                poll: Duration::ZERO,
                program: "kob-executor".into(),
            },
            &crate::app::AppSetup { node: "resolver".into(), role: crate::app::AppRole::Index, new_key: false, network: None },
        )
        .unwrap();
        let mut node = fx.node(vec![fx.coin(1, 5 * KAS)]);
        let e = fx.run(&fx.req(Amount::All), &mut node, &mut Script::default()).unwrap_err();
        assert!(e.to_string().contains("no operator key"), "{e}");
        let empty = tempfile::tempdir().unwrap();
        assert!(
            withdraw(&empty.path().join(CONFIG_FILE), empty.path(), &fx.req(Amount::All), &mut node, &mut Script::default()).is_err()
        );
    }
}
