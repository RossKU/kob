//! Shared pieces of the order commands.
//!
//! It holds the templates, the addresses, the node lookups, the funding of the maker and the
//! pipeline that signs, validates in the script engine, writes and submits a transaction.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use kaspa_addresses::Prefix;
use kaspa_consensus_core::tx::ScriptPublicKey;
use kob_protocol::artifacts::{template, template_by_hash, TemplateId};
use kob_protocol::build::pair_programs;
use kob_protocol::issue::rpc_transaction_json;
use kob_protocol::json::to_hex;
use kob_protocol::script::p2pk_spk;
use kob_protocol::state::AnyState;
use kob_protocol::tx::{finalize, pubkey_of, sign_locally, BuiltTx, FinalizeOptions, KeyUtxo, SignedTx, Utxo};
use kob_protocol::verify::validate_signed;
use serde_json::{json, Value};

use crate::wrpc::{self, NodeUtxo};

/// Timeout of one node request.
pub const NODE_TIMEOUT: Duration = Duration::from_secs(60);

/// Most funding UTXOs tried (smallest first) before giving up.
const FUNDING_TRIES: usize = 16;

/// The network a build uses when none is named: mainnet, like kaspad.
///
/// A test network is chosen at run time (`--testnet`, `--netsuffix=10` or `--network testnet-10`).
/// It is never an implicit testnet-10.
pub const DEFAULT_NETWORK: &str = "mainnet";

/// The network of a command.
///
/// It is `--network` when given (an empty value is not given), else the deployment network of the
/// build (`kob_protocol::artifacts::deployment_network`: `testnet-10` with the feature
/// `deploy-tn10`, `mainnet` with `deploy-mainnet`), else [`DEFAULT_NETWORK`]. A deployment build
/// refuses another network. Every plan and confirmation names the network, and a node of another
/// network is refused before anything is planned or signed.
pub fn resolve_network(given: &str) -> Result<String, String> {
    resolve_network_for(given, kob_protocol::artifacts::deployment_network())
}

/// [`resolve_network`] for a build whose deployment network is `build`.
pub fn resolve_network_for(given: &str, build: Option<&str>) -> Result<String, String> {
    let given = given.trim();
    let n = match (given.is_empty(), build) {
        (false, Some(b)) if given != b => {
            return Err(format!("this binary is the {b} deployment build but --network is {given}"));
        }
        (false, _) => given.to_string(),
        (true, Some(b)) => b.to_string(),
        (true, None) => DEFAULT_NETWORK.to_string(),
    };
    prefix_for_network(&n)?;
    Ok(n)
}

/// The arguments with the kaspad-style network switches turned into `--network`.
///
/// `--testnet` and `--netsuffix=10` (or `--netsuffix 10`) name `testnet-10`, the only test network
/// KOB runs on. They may stand anywhere. Naming another network too, such as `--network mainnet`,
/// is a contradiction.
pub fn network_switches(args: &[String]) -> Result<Vec<String>, String> {
    const TESTNET: &str = "testnet-10";
    let (mut out, mut testnet, mut named) = (Vec::with_capacity(args.len()), false, None::<String>);
    let mut it = args.iter().peekable();
    while let Some(a) = it.next() {
        let suffix = match a.as_str() {
            "--testnet" => {
                testnet = true;
                continue;
            }
            "--netsuffix" => Some(it.next().cloned().ok_or("--netsuffix needs a value (10)")?),
            s => s.strip_prefix("--netsuffix=").map(str::to_string),
        };
        if let Some(n) = suffix {
            if n != "10" {
                return Err(format!("--netsuffix {n}: KOB runs on testnet-10 only (--netsuffix=10)"));
            }
            testnet = true;
            continue;
        }
        if a == "--network" {
            named = it.peek().map(|v| v.to_string());
        } else if let Some(v) = a.strip_prefix("--network=") {
            named = Some(v.to_string());
        }
        out.push(a.clone());
    }
    match (testnet, named) {
        (true, Some(n)) if n != TESTNET => Err(format!("--testnet (or --netsuffix=10) and --network {n} name different networks")),
        (true, Some(_)) => Ok(out),
        (true, None) => {
            out.extend(["--network".to_string(), TESTNET.to_string()]);
            Ok(out)
        }
        (false, _) => Ok(out),
    }
}

/// Address prefix of a network name (`mainnet`, `testnet-10`, `simnet` or `devnet`).
pub fn prefix_for_network(network: &str) -> Result<Prefix, String> {
    if network.starts_with("mainnet") {
        Ok(Prefix::Mainnet)
    } else if network.starts_with("testnet") {
        Ok(Prefix::Testnet)
    } else if network.starts_with("simnet") {
        Ok(Prefix::Simnet)
    } else if network.starts_with("devnet") {
        Ok(Prefix::Devnet)
    } else {
        Err(format!("unknown network `{network}` (mainnet, testnet-10, simnet, devnet)"))
    }
}

/// The address of a script public key.
pub fn address_of(spk: &ScriptPublicKey, network: &str) -> Result<String, String> {
    kaspa_txscript::extract_script_pub_key_address(spk, prefix_for_network(network)?)
        .map(|a| a.to_string())
        .map_err(|e| format!("script has no address: {e}"))
}

/// `version (u16 BE) ‖ script`, the node's form.
pub fn spk_bytes(spk: &ScriptPublicKey) -> Vec<u8> {
    let mut b = spk.version().to_be_bytes().to_vec();
    b.extend_from_slice(spk.script());
    b
}

/// The order template with this hash, if this build pins it.
///
/// `None` means that this is not an order template that this build pins, so an order of any other
/// template is unknown here.
pub fn order_template(hash: &[u8; 32]) -> Option<TemplateId> {
    template_by_hash(hash).filter(|t| t.id.kind_code().is_some()).map(|t| t.id)
}

/// Decode a state span of the template `id` (canonical encodings only).
pub fn decode_state(id: TemplateId, span: &[u8]) -> Result<AnyState, String> {
    let s = AnyState::decode(id, span).map_err(|e| e.to_string())?;
    if s.try_encode().map_err(|e| e.to_string())? != span {
        return Err("state is not canonical for its template".into());
    }
    Ok(s)
}

/// The state span of a decoded state under the template `id`.
pub fn encode_state(id: TemplateId, s: &AnyState) -> Result<Vec<u8>, String> {
    if s.template_id() != id {
        return Err(format!("a {} state is not a {} state", s.template_id().name(), id.name()));
    }
    s.try_encode().map_err(|e| e.to_string())
}

/// The order's P2SH script public key for a state under the template `id`.
pub fn order_spk(id: TemplateId, s: &AnyState) -> Result<ScriptPublicKey, String> {
    Ok(template(id).spk(&encode_state(id, s)?))
}

/// The template an order sits under: one this build pins, or a retired one of its cancel-only list.
///
/// The retired templates are in `kob_protocol::retired`. For them only the cancel of the maker is
/// ever built.
#[derive(Clone, Copy, Debug)]
pub enum OrderScript {
    /// A pinned order kind.
    Pinned(TemplateId),
    /// A retired template. Its kind is the current kind of the same state layout.
    Retired(&'static kob_protocol::retired::RetiredTemplate),
}

impl OrderScript {
    /// The order kind its state decodes as.
    pub fn kind(self) -> TemplateId {
        match self {
            OrderScript::Pinned(t) => t,
            OrderScript::Retired(r) => r.kind,
        }
    }

    /// The retired template hash (`CancelOrder::retired`), `None` for a pinned template.
    pub fn retired(self) -> Option<[u8; 32]> {
        match self {
            OrderScript::Pinned(_) => None,
            OrderScript::Retired(r) => Some(r.hash),
        }
    }

    /// The order's P2SH script public key for a state of [`OrderScript::kind`].
    pub fn spk(self, s: &AnyState) -> Result<ScriptPublicKey, String> {
        match self {
            OrderScript::Pinned(t) => order_spk(t, s),
            OrderScript::Retired(r) => r.spk(&encode_state(r.kind, s)?).map_err(|e| e.to_string()),
        }
    }

    /// How a report names it: `pinned`, or `retired (cancel only): KobBid b995661f`.
    pub fn describe(self) -> String {
        match self {
            OrderScript::Pinned(_) => "pinned".into(),
            OrderScript::Retired(r) => format!("retired (cancel only): {}", r.label()),
        }
    }
}

/// The template of an order with this hash: [`order_template`], else a retired cancel-only one.
pub fn order_script(hash: &[u8; 32]) -> Option<OrderScript> {
    order_template(hash).map(OrderScript::Pinned).or_else(|| kob_protocol::retired::retired_by_hash(hash).map(OrderScript::Retired))
}

/// Base units still open (`amountLeft`). For a bid, its remaining buying power in an escrow of
/// `value` sompi.
pub fn amount_left_in(s: &AnyState, value: u64) -> Option<i64> {
    match s {
        AnyState::KobBid(b) | AnyState::KobBidKron(b) => Some(b.buying_power(i64::try_from(value).unwrap_or(i64::MAX))),
        other => other.amount_left(),
    }
}

/// The second token of an order and its program: the quote token B of a pair order.
///
/// It is `None` for the KAS kinds.
pub fn b_token(s: &AnyState) -> Result<Option<([u8; 32], TemplateId)>, String> {
    if !s.is_pair() {
        return Ok(None);
    }
    pair_programs(s).map(|(_, b)| Some(b)).map_err(|e| e.to_string())
}

/// The same state with another `amountLeft` in base units.
///
/// It is `None` for a bid, whose quantity is its escrow.
pub fn with_amount_left(s: &AnyState, n: i64) -> Option<AnyState> {
    let mut s = s.clone();
    match &mut s {
        AnyState::KobAsk(x) | AnyState::KobAskKron(x) => x.amount_left = n,
        AnyState::KobCondAsk(x) | AnyState::KobCondAskKron(x) => x.amount_left = n,
        AnyState::KobCondBid(x) | AnyState::KobCondBidKron(x) => x.amount_left = n,
        AnyState::KobIfdBid(x) | AnyState::KobIfdBidKron(x) => x.amount_left = n,
        AnyState::KobIfdAsk(x) | AnyState::KobIfdAskKron(x) => x.amount_left = n,
        // A pair ask, and a sell-first entry, holds exactly its amount left. The escrow of a bid
        // is not guessed.
        AnyState::KobPair(x) => {
            if x.is_ask() {
                x.custody = n;
            }
            x.amount_left = n;
        }
        AnyState::KobCondPair(x) => {
            if x.is_ask() {
                x.custody = n;
            }
            x.amount_left = n;
        }
        AnyState::KobIfdPair(x) => x.amount_left = n,
        AnyState::KobBid(_) | AnyState::KobBidKron(_) => return None,
    }
    Some(s)
}

/// Whether the script of a kind changes only in `amountLeft`, or never.
///
/// Its states differ in that one 8-byte window, and no other field needs guessing. The search over
/// amounts is still not exhaustive for such a kind, because any amount of at least `minFill` may
/// have been filled.
pub fn amount_is_the_only_mutable_field(id: TemplateId) -> bool {
    kob_protocol::state::mutable_windows(id).iter().all(|(name, ..)| *name == "amountLeft")
}

/// Whether the script of a state never changes after placement.
///
/// Then one candidate is the whole search. A bid is such a state: its quantity is its escrow.
pub fn script_is_fixed(s: &AnyState) -> bool {
    kob_protocol::state::mutable_windows(s.template_id()).is_empty()
}

/// A token amount in base units.
///
/// A plain integer is in base units. A decimal such as `1.5` is in whole tokens, and needs the
/// token's `scale` (`10^decimals`, base units per whole token) and at most `decimals` fractional
/// digits. The amount must be positive.
pub fn parse_amount(s: &str, scale: Option<i64>) -> Result<i64, String> {
    let bad = || format!("`{s}` is not an amount (base units, or a decimal amount of whole tokens)");
    if s.is_empty() || s.matches('.').count() > 1 || !s.chars().all(|c| c.is_ascii_digit() || c == '.') {
        return Err(bad());
    }
    let n: i128 = match s.split_once('.') {
        None => s.parse::<i128>().map_err(|_| bad())?,
        Some((int, frac)) => {
            if int.is_empty() && frac.is_empty() {
                return Err(bad());
            }
            let scale = scale.filter(|x| *x > 0).ok_or_else(|| {
                format!("`{s}`: a decimal amount needs the token's decimals, which are not known here: give base units")
            })?;
            let decimals = scale.to_string().len() - 1;
            if 10i128.pow(decimals as u32) != scale as i128 {
                return Err(format!("`{s}`: the scale {scale} is not a power of ten: give base units"));
            }
            if frac.len() > decimals {
                return Err(format!("`{s}` has more than the token's {decimals} decimals"));
            }
            let whole: i128 = if int.is_empty() { 0 } else { int.parse().map_err(|_| bad())? };
            let part: i128 = if frac.is_empty() { 0 } else { frac.parse().map_err(|_| bad())? };
            let part = part * 10i128.pow((decimals - frac.len()) as u32);
            whole.checked_mul(scale as i128).and_then(|w| w.checked_add(part)).ok_or_else(bad)?
        }
    };
    let n = i64::try_from(n).map_err(|_| format!("`{s}` is out of range"))?;
    if n <= 0 {
        return Err(format!("`{s}` must be positive"));
    }
    Ok(n)
}

/// A [`Utxo`] from a node entry.
pub fn utxo_of(u: &NodeUtxo) -> Utxo {
    Utxo { transaction_id: u.txid, index: u.index, amount: u.amount, block_daa_score: u.daa, covenant_id: u.covenant_id }
}

/// `txid:index`.
pub fn outpoint_str(txid: &[u8; 32], index: u32) -> String {
    format!("{}:{index}", to_hex(txid))
}

/// The unspent outputs of the node at a set of scripts, as a map from script bytes to entries.
pub fn lookup(node: &str, network: &str, spks: &[ScriptPublicKey]) -> Result<BTreeMap<Vec<u8>, Vec<NodeUtxo>>, String> {
    let mut addrs: BTreeSet<String> = BTreeSet::new();
    let mut wanted: BTreeSet<Vec<u8>> = BTreeSet::new();
    for spk in spks {
        addrs.insert(address_of(spk, network)?);
        wanted.insert(spk_bytes(spk));
    }
    let addrs: Vec<String> = addrs.into_iter().collect();
    let entries = wrpc::utxos_by_addresses(node, &addrs, NODE_TIMEOUT).map_err(|e| wrpc::node_failure(node, &e))?;
    let mut out: BTreeMap<Vec<u8>, Vec<NodeUtxo>> = BTreeMap::new();
    for e in entries {
        // Keyed by the script the node reports. An entry counts only for the exact script it has.
        if wanted.contains(&e.spk) {
            let v = out.entry(e.spk.clone()).or_default();
            if !v.iter().any(|x| x.txid == e.txid && x.index == e.index) {
                v.push(e);
            }
        }
    }
    Ok(out)
}

/// The plain KAS UTXOs of the maker (P2PK, no covenant id, not coinbase), smallest first.
pub fn maker_funding(node: &str, network: &str, maker: &[u8; 32]) -> Result<Vec<KeyUtxo>, String> {
    let spk = p2pk_spk(maker);
    let mut found = lookup(node, network, std::slice::from_ref(&spk))?.remove(&spk_bytes(&spk)).unwrap_or_default();
    found.retain(|u| u.covenant_id.is_none() && !u.coinbase);
    found.sort_by_key(|u| (u.amount, u.txid, u.index));
    Ok(found.iter().map(|u| KeyUtxo { utxo: utxo_of(u), pubkey: *maker }).collect())
}

/// How a transaction was funded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Funding {
    /// The order's own released KAS paid the fee.
    Own,
    /// One of the maker's P2PK UTXOs was added.
    Key(KeyUtxo),
}

/// Build the transaction, funded by the maker's UTXOs only if needed.
///
/// With `try_unfunded` the build is tried without funding first. When that fails for lack of
/// funds, or is not tried, the smallest P2PK UTXO of the maker that makes the build succeed is
/// added.
pub fn build_funded(
    try_unfunded: bool,
    node: &str,
    network: &str,
    maker: &[u8; 32],
    build: impl Fn(Vec<KeyUtxo>) -> kob_protocol::Result<BuiltTx>,
) -> Result<(BuiltTx, Funding), String> {
    if try_unfunded {
        match build(vec![]) {
            Ok(b) => return Ok((b, Funding::Own)),
            Err(kob_protocol::Error::InsufficientFunds { .. }) => {}
            Err(e) => return Err(e.to_string()),
        }
    }
    let utxos = maker_funding(node, network, maker)?;
    if utxos.is_empty() {
        return Err(format!(
            "needs funding and the node holds no plain KAS UTXO of the maker ({})",
            address_of(&p2pk_spk(maker), network)?
        ));
    }
    let mut last = String::new();
    for u in utxos.into_iter().take(FUNDING_TRIES) {
        match build(vec![u.clone()]) {
            Ok(b) => return Ok((b, Funding::Key(u))),
            Err(e @ kob_protocol::Error::InsufficientFunds { .. }) => last = e.to_string(),
            Err(e) => return Err(e.to_string()),
        }
    }
    Err(format!("none of the maker's KAS UTXOs is large enough: {last}"))
}

/// The signing key and its x-only public key.
pub struct Signer {
    /// Secret key bytes.
    pub secret: [u8; 32],
    /// x-only public key.
    pub pubkey: [u8; 32],
}

/// The debug output never prints the secret.
impl std::fmt::Debug for Signer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Signer").field("pubkey", &to_hex(&self.pubkey)).field("secret", &"<redacted>").finish()
    }
}

impl Signer {
    /// From a key source (`--key-file`, `--key-env`, or a deprecated `--key` form).
    pub fn load(src: &crate::token::KeySource) -> Result<Signer, String> {
        let sk = crate::token::read_key(src)?;
        let secret = sk.secret_bytes();
        let pubkey = pubkey_of(&secret).map_err(|e| e.to_string())?;
        Ok(Signer { secret, pubkey })
    }
}

/// What happened to one transaction.
#[derive(Debug, Clone)]
pub struct Sent {
    /// Transaction id.
    pub txid: String,
    /// Fee in sompi.
    pub fee: u64,
    /// The file written, if any.
    pub file: Option<PathBuf>,
    /// The id the node accepted (`None`: dry run).
    pub submitted: Option<String>,
}

/// Sign, validate, write and submit one transaction.
///
/// Every request of `built` is signed with `signer`, and a transaction that needs another key is
/// refused. The signed transaction is finalized and validated in the script engine, and nothing is
/// written or submitted unless that passes. It is written to `<out_dir>/<file_name>` and submitted
/// unless `dry_run`.
pub fn sign_validate_send(
    built: &BuiltTx,
    signer: &Signer,
    out_dir: Option<&Path>,
    file_name: &str,
    extra: Value,
    dry_run: bool,
    node: &str,
    network: &str,
) -> Result<Sent, String> {
    if let Some(r) = built.sign.iter().find(|r| r.pubkey != signer.pubkey) {
        return Err(format!("input {} needs the signature of {}, not of this key", r.input_index, to_hex(&r.pubkey)));
    }
    if !dry_run {
        // A signature commits to no network. Never sign for one network what a node of another
        // would accept.
        wrpc::check_node_network(node, network, NODE_TIMEOUT)?;
    }
    let keys = BTreeMap::from([(signer.pubkey, signer.secret)]);
    let sigs = sign_locally(built, &keys).map_err(|e| format!("sign: {e}"))?;
    let signed: SignedTx = finalize(built, &sigs, FinalizeOptions::default()).map_err(|e| format!("finalize: {e}"))?;
    validate_signed(&signed).map_err(|e| format!("engine validation failed (nothing submitted): {e}"))?;
    let (tx, _) = signed.tx.to_tx().map_err(|e| e.to_string())?;
    let rpc = rpc_transaction_json(&tx);
    let txid = tx.id().to_string();
    let mut file = None;
    if let Some(dir) = out_dir {
        std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
        let doc = json!({
            "txid": txid,
            "fee": signed.fee.fee.to_string(),
            "info": extra,
            "signed": serde_json::to_value(&signed).map_err(|e| e.to_string())?,
            "submit_request": wrpc::submit_request(1, &rpc, false),
        });
        let path = dir.join(file_name);
        let mut s = serde_json::to_string_pretty(&doc).map_err(|e| e.to_string())?;
        s.push('\n');
        std::fs::write(&path, s).map_err(|e| format!("write {}: {e}", path.display()))?;
        file = Some(path);
    }
    let submitted = if dry_run {
        None
    } else {
        let id = wrpc::submit_transaction(node, &rpc, false, NODE_TIMEOUT).map_err(|e| format!("submit: {e}"))?;
        if id != txid {
            eprintln!("warning: the node reported transaction id {id}, expected {txid}");
        }
        // Record the own token outputs of the key for the next command within the lag of the
        // indexer. They are confirmed on the node before use.
        crate::recent::record(network, &signer.pubkey, built, &txid);
        Some(id)
    };
    Ok(Sent { txid, fee: signed.fee.fee, file, submitted })
}

/// Read and parse a JSON file.
pub fn read_json(path: &Path) -> Result<Value, String> {
    let s = std::fs::read_to_string(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    serde_json::from_str(&s).map_err(|e| format!("{} is not JSON: {e}", path.display()))
}

/// Write a JSON value (pretty, trailing newline).
pub fn write_json(path: &Path, v: &Value) -> Result<(), String> {
    let mut s = serde_json::to_string_pretty(v).map_err(|e| e.to_string())?;
    s.push('\n');
    std::fs::write(path, s).map_err(|e| format!("write {}: {e}", path.display()))
}

/// Format sompi as KAS.
pub fn kas(sompi: u64) -> String {
    format!("{}.{:08}", sompi / 100_000_000, sompi % 100_000_000)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_network_is_named_or_the_builds_or_mainnet() {
        // No deployment: mainnet like kaspad, never an implicit testnet-10. A test network is
        // named.
        assert_eq!(resolve_network_for("", None).unwrap(), "mainnet");
        assert_eq!(resolve_network_for("  ", None).unwrap(), "mainnet");
        assert_eq!(resolve_network_for("mainnet", None).unwrap(), "mainnet");
        assert_eq!(resolve_network_for("testnet-10", None).unwrap(), "testnet-10");
        assert!(resolve_network_for("nonsense", None).is_err());
        // A deployment build implies its network and refuses another.
        assert_eq!(resolve_network_for("", Some("mainnet")).unwrap(), "mainnet");
        assert_eq!(resolve_network_for("", Some("testnet-10")).unwrap(), "testnet-10");
        let e = resolve_network_for("testnet-10", Some("mainnet")).unwrap_err();
        assert!(e.contains("mainnet deployment build"), "{e}");
        assert_eq!(resolve_network("").ok().as_deref(), Some(kob_protocol::artifacts::deployment_network().unwrap_or("mainnet")));
    }

    #[test]
    fn the_kaspad_style_network_switches() {
        let v = |s: &str| s.split_whitespace().map(str::to_string).collect::<Vec<_>>();
        let sw = |s: &str| network_switches(&v(s));
        assert_eq!(sw("book TUSD").unwrap(), v("book TUSD"));
        assert_eq!(sw("book TUSD --testnet").unwrap(), v("book TUSD --network testnet-10"));
        assert_eq!(sw("--testnet book TUSD").unwrap(), v("book TUSD --network testnet-10"));
        assert_eq!(sw("book TUSD --netsuffix=10").unwrap(), v("book TUSD --network testnet-10"));
        assert_eq!(sw("book TUSD --netsuffix 10").unwrap(), v("book TUSD --network testnet-10"));
        assert_eq!(sw("book --testnet --netsuffix=10 TUSD").unwrap(), v("book TUSD --network testnet-10"));
        // The long form stays, alone or together.
        assert_eq!(sw("book TUSD --network testnet-10").unwrap(), v("book TUSD --network testnet-10"));
        assert_eq!(sw("book TUSD --testnet --network testnet-10").unwrap(), v("book TUSD --network testnet-10"));
        assert_eq!(sw("book TUSD --testnet --network=testnet-10").unwrap(), v("book TUSD --network=testnet-10"));
        assert_eq!(sw("book TUSD --network mainnet").unwrap(), v("book TUSD --network mainnet"));
        // Contradictions and unknown suffixes.
        assert!(sw("book TUSD --testnet --network mainnet").unwrap_err().contains("different networks"));
        assert!(sw("book TUSD --network=mainnet --testnet").unwrap_err().contains("different networks"));
        assert!(sw("book TUSD --netsuffix=11").unwrap_err().contains("testnet-10 only"));
        assert!(sw("book TUSD --netsuffix").unwrap_err().contains("needs a value"));
    }

    #[test]
    fn addresses_follow_the_network() {
        let spk = p2pk_spk(&[0x11; 32]);
        assert!(address_of(&spk, "testnet-10").unwrap().starts_with("kaspatest:"));
        assert!(address_of(&spk, "mainnet").unwrap().starts_with("kaspa:"));
        assert!(address_of(&spk, "elsewhere").is_err());
        assert_eq!(spk_bytes(&spk)[..2], [0, 0]);
        assert_eq!(kas(123_456_789), "1.23456789");
    }

    #[test]
    fn amounts_are_base_units_or_decimals_of_the_tokens_decimals() {
        // A plain integer is base units, with or without a known scale.
        assert_eq!(parse_amount("150000000", None), Ok(150_000_000));
        assert_eq!(parse_amount("150000000", Some(100_000_000)), Ok(150_000_000));
        // A decimal is whole tokens: 8 decimals.
        assert_eq!(parse_amount("1.5", Some(100_000_000)), Ok(150_000_000));
        assert_eq!(parse_amount("0.00000001", Some(100_000_000)), Ok(1));
        assert_eq!(parse_amount(".5", Some(1_000)), Ok(500));
        assert_eq!(parse_amount("5.", Some(1_000)), Ok(5_000));
        assert_eq!(parse_amount("6.500", Some(1_000)), Ok(6_500));
        // More decimals than the token has, or no decimals known.
        assert!(parse_amount("1.0001", Some(1_000)).is_err());
        assert!(parse_amount("1.5", None).is_err());
        assert!(parse_amount("1.5", Some(1)).is_err(), "a token without decimals has no fractions");
        assert!(parse_amount("1.5", Some(1_500)).is_err(), "the scale must be a power of ten");
        // Not an amount.
        for bad in ["", ".", "-1", "+1", "1e3", "1.2.3", " 5", "0x10", "1,5", "abc"] {
            assert!(parse_amount(bad, Some(1_000)).is_err(), "`{bad}`");
        }
        // Positive only, and it must fit.
        assert!(parse_amount("0", None).is_err());
        assert!(parse_amount("0.000", Some(1_000)).is_err());
        assert_eq!(parse_amount("9223372036854775807", None), Ok(i64::MAX));
        assert!(parse_amount("9223372036854775808", None).is_err());
        assert_eq!(parse_amount("9223372036854775.807", Some(1_000)), Ok(i64::MAX));
        assert!(parse_amount("9223372036854775.808", Some(1_000)).is_err());
        assert!(parse_amount("99999999999999999999999999999999999999999", None).is_err());
    }
}
