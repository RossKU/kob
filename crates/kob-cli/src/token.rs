//! `kob token issue`: fixed-supply KCC-20 issuance (8/8 slots, owner scheme 0x04 enabled, borrow disabled).

use std::path::{Path, PathBuf};
use std::time::Duration;

use kaspa_addresses::{Address, Version};
use kaspa_consensus_core::tx::{TransactionId, TransactionOutpoint};
use kob_protocol::issue::{
    build_genesis, hex, hex_array, p2pk_script, FundingUtxo, GenesisPlan, Holder, IssueSpec, DEFAULT_CARRIER, DEFAULT_FEE_RATE,
    EXTENSION_FIXED_SUPPLY,
};
use secp256k1::{Keypair, Secp256k1, SecretKey};
use serde_json::Value;

use crate::wrpc;

/// Usage text of `kob token issue`.
pub const ISSUE_USAGE: &str = "\
kob token issue: issue a fixed-supply KCC-20 token (reference program, 8 token inputs / 8 outputs per transfer)

USAGE
  kob token issue --name <NAME> --ticker <TICKER> --decimals <N> --fund-utxo <TXID:INDEX:AMOUNT:OWNER>...
                  [--supply <UNITS>] [--holder <OWNER:SCHEME:AMOUNT[:BORROW_SCHEME:GUARD]>...]
                  [--key <HEX | env:VAR | file:PATH>] [--out-dir <DIR>] [--dry-run] [--node ws://HOST:PORT]

REQUIRED
  --name, --ticker      display name (1..=64 chars) and ticker (2..=12 of A-Z 0-9)
  --decimals            0..=18 (informational; every amount is in base units)
  --fund-utxo           funding UTXO, repeatable; OWNER = 64-hex x-only pubkey or a kaspa address (P2PK Schnorr).
                        Input 0 authorises the genesis group. AMOUNT is in sompi.

SUPPLY
  --holder              initial holder, repeatable, one token output each. OWNER = 64-hex | `self` (the funding
                        key) | kaspa address (scheme 0 only). SCHEME = 0 p2pk-schnorr, 1 p2pkh-schnorr,
                        2 p2pkh-ecdsa, 3 p2sh, 4 covenant id. AMOUNT in base units.
                        Scheme 4 states are always issued with borrow disabled (borrow guard rules cannot be set).
  --supply              total supply in base units; must equal the sum of the holders. Without --holder the
                        whole supply goes to `self` (scheme 0).
  --extension-commitment 64-hex (default: 32 zero bytes)
  --allow-borrow        allow BORROW_SCHEME 1..3 on non-0x04 holders (default: borrow disabled everywhere)

TRANSACTION
  --carrier             sompi on each token output (default 1000000000 = 10 KAS)
  --fee                 explicit fee in sompi (default: fee-rate x mass)
  --fee-rate            sompi per mass unit (default 100)
  --change-to           address or 64-hex pubkey for the change (default: the funding key)
  --key                 signing secret (64 hex). `env:NAME` and `file:PATH` keep it out of shell history.

OUTPUT
  --out-dir             write genesis_tx.json, submit_request.json, supply.json, metadata.json, metadata.payload.hex,
                        registry_entry.json here
  --description --icon --website --network   recorded in metadata/supply
  --dry-run             build and verify everything offline (signs and executes the funding inputs when --key is
                        given, else the transaction stays unsigned); never submits
  --node ws://HOST:PORT submit the signed transaction via wRPC JSON (`submitTransaction`; JSON port 18110 mainnet,
                        18210 testnet-10). Required unless --dry-run.
";

/// Where the signing key comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeySource {
    /// Hex on the command line.
    Hex(String),
    /// Environment variable.
    Env(String),
    /// File containing the hex.
    File(PathBuf),
}

/// A parsed `--holder`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HolderArg {
    /// `None` = `self` (the funding key).
    pub owner: Option<[u8; 32]>,
    /// Owner scheme.
    pub scheme: u8,
    /// Amount in base units.
    pub amount: u64,
    /// Borrow scheme.
    pub borrow_scheme: u8,
    /// Borrow guard.
    pub borrow_guard: [u8; 32],
}

/// A parsed `--fund-utxo`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FundArg {
    /// Outpoint.
    pub outpoint: TransactionOutpoint,
    /// Sompi.
    pub amount: u64,
    /// x-only pubkey.
    pub owner: [u8; 32],
}

/// Parsed arguments of `kob token issue`.
#[derive(Debug, Clone)]
pub struct IssueArgs {
    /// Name.
    pub name: String,
    /// Ticker.
    pub ticker: String,
    /// Decimals.
    pub decimals: u8,
    /// Declared supply.
    pub supply: Option<u64>,
    /// Holders.
    pub holders: Vec<HolderArg>,
    /// Funding UTXOs.
    pub fund: Vec<FundArg>,
    /// Key source.
    pub key: Option<KeySource>,
    /// Output directory.
    pub out_dir: Option<PathBuf>,
    /// Carrier.
    pub carrier: u64,
    /// Fee.
    pub fee: Option<u64>,
    /// Fee rate.
    pub fee_rate: u64,
    /// Extension commitment.
    pub extension_commitment: [u8; 32],
    /// Allow borrow.
    pub allow_borrow: bool,
    /// Change destination.
    pub change_to: Option<[u8; 32]>,
    /// Description.
    pub description: Option<String>,
    /// Icon.
    pub icon: Option<String>,
    /// Website.
    pub website: Option<String>,
    /// Network label.
    pub network: String,
    /// Dry run.
    pub dry_run: bool,
    /// Node URL.
    pub node: Option<String>,
}

const ADDRESS_PREFIXES: [&str; 4] = ["kaspa", "kaspatest", "kaspasim", "kaspadev"];

/// Parse a 64-hex x-only pubkey or a P2PK Schnorr kaspa address.
pub fn parse_pubkey(s: &str) -> Result<[u8; 32], String> {
    if let Some((prefix, _)) = s.split_once(':') {
        if ADDRESS_PREFIXES.contains(&prefix) {
            let a = Address::try_from(s).map_err(|e| format!("bad address `{s}`: {e}"))?;
            if a.version != Version::PubKey {
                return Err(format!("address `{s}` is not a P2PK Schnorr address"));
            }
            return a.payload.as_slice().try_into().map_err(|_| format!("address `{s}` has an unexpected payload"));
        }
    }
    hex_array::<32>(s).map_err(|e| e.to_string())
}

fn parse_u64(s: &str, what: &str) -> Result<u64, String> {
    s.replace('_', "").parse::<u64>().map_err(|_| format!("{what} must be a non-negative integer, got `{s}`"))
}

fn parse_scheme(s: &str, what: &str) -> Result<u8, String> {
    let v = match s.strip_prefix("0x") {
        Some(h) => u8::from_str_radix(h, 16),
        None => s.parse::<u8>(),
    };
    v.map_err(|_| format!("{what} must be a small integer, got `{s}`"))
}

/// Split `a:b:c:...` where a leading kaspa address prefix keeps its own colon.
fn split_fields(s: &str) -> Vec<String> {
    let parts: Vec<&str> = s.split(':').collect();
    let mut out = vec![];
    let mut i = 0;
    while i < parts.len() {
        if i == 0 && ADDRESS_PREFIXES.contains(&parts[0]) && parts.len() > 1 {
            out.push(format!("{}:{}", parts[0], parts[1]));
            i += 2;
        } else {
            out.push(parts[i].to_string());
            i += 1;
        }
    }
    out
}

/// Parse `OWNER:SCHEME:AMOUNT[:BORROW_SCHEME:GUARD_HEX]`.
pub fn parse_holder(s: &str) -> Result<HolderArg, String> {
    let f = split_fields(s);
    if f.len() != 3 && f.len() != 5 {
        return Err(format!("--holder `{s}`: expected OWNER:SCHEME:AMOUNT[:BORROW_SCHEME:GUARD]"));
    }
    let owner = if f[0] == "self" { None } else { Some(parse_pubkey(&f[0]).map_err(|e| format!("--holder `{s}`: {e}"))?) };
    let scheme = parse_scheme(&f[1], "owner scheme")?;
    if scheme > 4 {
        return Err(format!("--holder `{s}`: owner scheme must be 0..=4"));
    }
    if scheme != 0 && owner.is_some() && f[0].contains(':') {
        return Err(format!("--holder `{s}`: an address is only valid for owner scheme 0"));
    }
    let amount = parse_u64(&f[2], "amount")?;
    let (borrow_scheme, borrow_guard) = if f.len() == 5 {
        (parse_scheme(&f[3], "borrow scheme")?, hex_array::<32>(&f[4]).map_err(|e| e.to_string())?)
    } else {
        (0, [0u8; 32])
    };
    if scheme != 0 && f[0] == "self" {
        return Err(format!("--holder `{s}`: `self` is the funding pubkey and only valid for owner scheme 0"));
    }
    Ok(HolderArg { owner, scheme, amount, borrow_scheme, borrow_guard })
}

/// Parse `TXID:INDEX:AMOUNT:OWNER`.
pub fn parse_fund(s: &str) -> Result<FundArg, String> {
    let (head, owner) = {
        let mut it = s.splitn(4, ':');
        let (a, b, c, d) = (it.next(), it.next(), it.next(), it.next());
        match (a, b, c, d) {
            (Some(a), Some(b), Some(c), Some(d)) => ((a, b, c), d),
            _ => return Err(format!("--fund-utxo `{s}`: expected TXID:INDEX:AMOUNT:OWNER")),
        }
    };
    let txid: [u8; 32] = hex_array(head.0).map_err(|e| format!("--fund-utxo `{s}`: txid: {e}"))?;
    let index = head.1.parse::<u32>().map_err(|_| format!("--fund-utxo `{s}`: bad index"))?;
    Ok(FundArg {
        outpoint: TransactionOutpoint { transaction_id: TransactionId::from_bytes(txid), index },
        amount: parse_u64(head.2, "funding amount")?,
        owner: parse_pubkey(owner).map_err(|e| format!("--fund-utxo `{s}`: {e}"))?,
    })
}

pub(crate) fn parse_key_source(s: &str) -> KeySource {
    if let Some(v) = s.strip_prefix("env:") {
        KeySource::Env(v.to_string())
    } else if let Some(p) = s.strip_prefix("file:") {
        KeySource::File(PathBuf::from(p))
    } else {
        KeySource::Hex(s.to_string())
    }
}

/// Parse the arguments after `kob token issue`.
pub fn parse_issue_args(args: &[String]) -> Result<IssueArgs, String> {
    let mut a = IssueArgs {
        name: String::new(),
        ticker: String::new(),
        decimals: 0,
        supply: None,
        holders: vec![],
        fund: vec![],
        key: None,
        out_dir: None,
        carrier: DEFAULT_CARRIER,
        fee: None,
        fee_rate: DEFAULT_FEE_RATE,
        extension_commitment: EXTENSION_FIXED_SUPPLY,
        allow_borrow: false,
        change_to: None,
        description: None,
        icon: None,
        website: None,
        network: "testnet-10".into(),
        dry_run: false,
        node: None,
    };
    let (mut have_name, mut have_ticker, mut have_decimals) = (false, false, false);
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        let mut val = |what: &str| -> Result<String, String> { it.next().cloned().ok_or_else(|| format!("{what} needs a value")) };
        match flag.as_str() {
            "--name" => {
                a.name = val("--name")?;
                have_name = true;
            }
            "--ticker" => {
                a.ticker = val("--ticker")?;
                have_ticker = true;
            }
            "--decimals" => {
                let v = val("--decimals")?;
                a.decimals = v.parse::<u8>().map_err(|_| format!("--decimals must be 0..=18, got `{v}`"))?;
                have_decimals = true;
            }
            "--supply" => a.supply = Some(parse_u64(&val("--supply")?, "--supply")?),
            "--holder" => a.holders.push(parse_holder(&val("--holder")?)?),
            "--fund-utxo" => a.fund.push(parse_fund(&val("--fund-utxo")?)?),
            "--key" => a.key = Some(parse_key_source(&val("--key")?)),
            "--out-dir" => a.out_dir = Some(PathBuf::from(val("--out-dir")?)),
            "--carrier" => a.carrier = parse_u64(&val("--carrier")?, "--carrier")?,
            "--fee" => a.fee = Some(parse_u64(&val("--fee")?, "--fee")?),
            "--fee-rate" => a.fee_rate = parse_u64(&val("--fee-rate")?, "--fee-rate")?,
            "--extension-commitment" => {
                a.extension_commitment = hex_array(&val("--extension-commitment")?).map_err(|e| e.to_string())?
            }
            "--allow-borrow" => a.allow_borrow = true,
            "--change-to" => a.change_to = Some(parse_pubkey(&val("--change-to")?)?),
            "--description" => a.description = Some(val("--description")?),
            "--icon" => a.icon = Some(val("--icon")?),
            "--website" => a.website = Some(val("--website")?),
            "--network" => a.network = val("--network")?,
            "--dry-run" => a.dry_run = true,
            "--node" => a.node = Some(val("--node")?),
            other => return Err(format!("unknown argument `{other}` (see `kob token issue --help`)")),
        }
    }
    if !have_name || !have_ticker || !have_decimals {
        return Err("--name, --ticker and --decimals are required".into());
    }
    if a.fund.is_empty() {
        return Err("at least one --fund-utxo is required".into());
    }
    if a.holders.is_empty() && a.supply.is_none() {
        return Err("give --supply and/or at least one --holder".into());
    }
    if !a.dry_run && a.node.is_none() {
        return Err("--node is required unless --dry-run".into());
    }
    if !a.dry_run && a.key.is_none() {
        return Err("--key is required unless --dry-run (an unsigned transaction cannot be submitted)".into());
    }
    Ok(a)
}

pub(crate) fn read_key(src: &KeySource) -> Result<SecretKey, String> {
    let raw = match src {
        KeySource::Hex(h) => h.clone(),
        KeySource::Env(v) => std::env::var(v).map_err(|_| format!("environment variable {v} is not set"))?,
        KeySource::File(p) => std::fs::read_to_string(p).map_err(|e| format!("cannot read key file {}: {e}", p.display()))?,
    };
    let bytes: [u8; 32] = hex_array(raw.trim()).map_err(|e| format!("signing key: {e}"))?;
    SecretKey::from_slice(&bytes).map_err(|e| format!("signing key: {e}"))
}

/// Build the library spec from parsed arguments (resolving `self` holders against the funding key).
pub fn to_spec(a: &IssueArgs) -> Result<IssueSpec, String> {
    let funder = a.fund[0].owner;
    if a.fund.iter().any(|f| f.owner != funder) {
        return Err("all --fund-utxo entries must belong to the same key".into());
    }
    let holders: Vec<Holder> = if a.holders.is_empty() {
        vec![Holder::new(funder, 0, a.supply.expect("checked"))]
    } else {
        a.holders
            .iter()
            .map(|h| Holder {
                owner: h.owner.unwrap_or(funder),
                owner_scheme: h.scheme,
                amount: h.amount,
                borrow_scheme: h.borrow_scheme,
                borrow_guard: h.borrow_guard,
            })
            .collect()
    };
    let supply = a.supply.unwrap_or_else(|| holders.iter().map(|h| h.amount).sum());
    let funding = a.fund.iter().map(|f| FundingUtxo { outpoint: f.outpoint, amount: f.amount, owner_pubkey: f.owner }).collect();
    let mut spec = IssueSpec::new(&a.name, &a.ticker, a.decimals, supply, holders, funding);
    spec.extension_commitment = a.extension_commitment;
    spec.carrier = a.carrier;
    spec.fee = a.fee;
    spec.fee_rate = a.fee_rate;
    spec.allow_borrow = a.allow_borrow;
    spec.change_spk = a.change_to.map(|pk| p2pk_script(&pk));
    spec.description = a.description.clone();
    spec.icon = a.icon.clone();
    spec.website = a.website.clone();
    spec.network = a.network.clone();
    Ok(spec)
}

fn write_json(dir: &Path, name: &str, v: &Value) -> Result<(), String> {
    let mut s = serde_json::to_string_pretty(v).map_err(|e| e.to_string())?;
    s.push('\n');
    std::fs::write(dir.join(name), s).map_err(|e| format!("write {name}: {e}"))
}

/// Write every output document of a plan into `dir`.
pub fn write_outputs(dir: &Path, plan: &GenesisPlan) -> Result<Vec<String>, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let tx = plan.rpc_transaction_json();
    write_json(dir, "genesis_tx.json", &tx)?;
    write_json(dir, "submit_request.json", &wrpc::submit_request(1, &tx, false))?;
    write_json(dir, "supply.json", &plan.supply_json())?;
    write_json(dir, "metadata.json", &plan.metadata_json())?;
    write_json(dir, "registry_entry.json", &plan.registry_entry())?;
    std::fs::write(dir.join("metadata.payload.hex"), format!("{}\n", hex(&plan.metadata_payload())))
        .map_err(|e| format!("write metadata.payload.hex: {e}"))?;
    Ok(["genesis_tx.json", "submit_request.json", "supply.json", "metadata.json", "metadata.payload.hex", "registry_entry.json"]
        .iter()
        .map(|s| s.to_string())
        .collect())
}

/// Run `kob token issue`; returns the process exit code.
pub fn run_issue(args: &[String]) -> i32 {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print!("{ISSUE_USAGE}");
        return 0;
    }
    match issue(args) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("error: {e}");
            1
        }
    }
}

fn issue(args: &[String]) -> Result<(), String> {
    let a = parse_issue_args(args)?;
    let spec = to_spec(&a)?;
    let mut plan = build_genesis(&spec).map_err(|e| e.to_string())?;
    if let Some(src) = &a.key {
        let sk = read_key(src)?;
        let pk = Keypair::from_secret_key(&Secp256k1::new(), &sk).x_only_public_key().0.serialize();
        if pk != spec.funding[0].owner_pubkey {
            return Err("the signing key does not own the funding UTXO".into());
        }
        plan.sign(&sk).map_err(|e| e.to_string())?;
    }
    let report = plan.verify().map_err(|e| e.to_string())?;

    println!(
        "token        {} ({}), {} decimals, fixed supply {} base units",
        plan.spec.name, plan.spec.ticker, plan.spec.decimals, plan.spec.supply
    );
    println!("covenant id  {}", hex(plan.covenant_id.as_bytes().as_slice()));
    println!(
        "template     {}  (prefix {} B, suffix {} B, {}/{} slots)",
        hex(&plan.program.template_hash),
        plan.program.prefix.len(),
        plan.program.suffix.len(),
        plan.program.max_token_inputs,
        plan.program.max_token_outputs
    );
    println!(
        "genesis tx   {}  ({} token outputs, fee {} sompi, mass compute {} storage {}, {} B)",
        plan.txid(),
        plan.states.len(),
        plan.fee,
        report.compute_mass,
        report.storage_mass,
        report.size
    );
    println!(
        "verified     covenant group, exact scripts, supply, mass, owner scheme 0x04 self-test{}",
        if report.scripts_executed {
            ", funding inputs executed in the script engine"
        } else {
            " (UNSIGNED: script execution skipped, pass --key)"
        }
    );
    for w in &plan.warnings {
        println!("warning      {w}");
    }
    if let Some(dir) = &a.out_dir {
        let files = write_outputs(dir, &plan)?;
        println!("wrote        {} to {}", files.join(", "), dir.display());
    } else if a.dry_run {
        println!("(no --out-dir: nothing written)");
    }
    if a.dry_run {
        println!("dry run: nothing submitted");
        return Ok(());
    }
    let node = a.node.as_deref().expect("checked");
    let txid =
        wrpc::submit_transaction(node, &plan.rpc_transaction_json(), false, Duration::from_secs(30)).map_err(|e| e.to_string())?;
    println!("submitted    {txid}");
    if txid != plan.txid().to_string() {
        eprintln!("warning: the node reported transaction id {txid}, expected {}", plan.txid());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_string).collect()
    }
    const TX: &str = "0707070707070707070707070707070707070707070707070707070707070707";
    const PK: &str = "aa2c17c5f5e5d5a3b6c0d2f1e4b7a9c8d7e6f5a4b3c2d1e0f9a8b7c6d5e4f3a2";

    fn base() -> String {
        format!("--name Test --ticker TEST --decimals 8 --fund-utxo {TX}:1:5000000000:{PK} --dry-run")
    }

    #[test]
    fn parses_a_full_command() {
        let a = parse_issue_args(&v(&format!(
            "{} --supply 1000 --holder self:0:600 --holder {PK}:4:400 --out-dir out --allow-borrow",
            base()
        )))
        .unwrap();
        assert_eq!(a.holders.len(), 2);
        assert_eq!(a.holders[0].owner, None);
        assert_eq!(a.holders[1].scheme, 4);
        assert_eq!(a.fund[0].amount, 5_000_000_000);
        assert_eq!(a.fund[0].outpoint.index, 1);
        assert!(a.dry_run && a.allow_borrow);
        let spec = to_spec(&a).unwrap();
        assert_eq!(spec.holders[0].owner, a.fund[0].owner);
        assert_eq!(spec.supply, 1000);
    }

    #[test]
    fn supply_alone_goes_to_self() {
        let a = parse_issue_args(&v(&format!("{} --supply 42", base()))).unwrap();
        let s = to_spec(&a).unwrap();
        assert_eq!(s.holders, vec![Holder::new(a.fund[0].owner, 0, 42)]);
    }

    #[test]
    fn requires_the_essentials() {
        assert!(parse_issue_args(&v("--name X --ticker X --decimals 1")).is_err());
        assert!(
            parse_issue_args(&v(&format!("--name X --ticker X --decimals 1 --fund-utxo {TX}:0:1:{PK} --supply 1"))).is_err(),
            "needs --node or --dry-run"
        );
        assert!(
            parse_issue_args(&v(&format!("--name X --ticker X --decimals 1 --fund-utxo {TX}:0:1:{PK} --supply 1 --node ws://x:1")))
                .is_err(),
            "needs --key"
        );
        assert!(parse_issue_args(&v(&format!("--ticker X --decimals 1 --fund-utxo {TX}:0:1:{PK} --supply 1 --dry-run"))).is_err());
        assert!(parse_issue_args(&v(&format!("{} --bogus", base()))).is_err());
        assert!(parse_issue_args(&v(&format!("{} --supply 1 --holder {PK}:9:1", base()))).is_err());
        assert!(parse_issue_args(&v(&format!("{} --supply 1 --holder self:4:1", base()))).is_err());
    }

    #[test]
    fn holder_borrow_fields_and_addresses() {
        let h = parse_holder(&format!("{PK}:1:10:1:{}", "11".repeat(32))).unwrap();
        assert_eq!((h.scheme, h.borrow_scheme, h.borrow_guard[0]), (1, 1, 0x11));
        assert!(parse_holder(&format!("{PK}:1:10:1")).is_err());
        assert!(parse_holder("nothex:0:1").is_err());
        assert!(parse_pubkey("kaspa:notanaddress").is_err());
    }

    #[test]
    fn dry_run_end_to_end_signed_and_unsigned() {
        let sk = "3333333333333333333333333333333333333333333333333333333333333333";
        let pk = Keypair::from_secret_key(&Secp256k1::new(), &SecretKey::from_slice(&hex_array::<32>(sk).unwrap()).unwrap())
            .x_only_public_key()
            .0
            .serialize();
        let dir = std::env::temp_dir().join(format!("kob-issue-test-{}", std::process::id()));
        let cmd = format!(
            "--name Test --ticker TEST --decimals 8 --fund-utxo {TX}:0:5000000000:{} --supply 1000 --holder self:0:900 --holder {PK}:4:100 --dry-run --key {sk} --out-dir {}",
            hex(&pk),
            dir.display()
        );
        assert_eq!(run_issue(&v(&cmd)), 0);
        let reg: Value = serde_json::from_str(&std::fs::read_to_string(dir.join("registry_entry.json")).unwrap()).unwrap();
        assert_eq!(reg["ticker"], "TEST");
        assert_eq!(reg["template_id"], "kcc20-ref-8x8");
        // the emitted entry is a valid registry token against the shipped templates
        let mut registry = kob_protocol::registry::Registry::default_registry();
        registry.tokens.push(serde_json::from_value(reg.clone()).expect("registry entry parses as a registry token"));
        registry.validate().expect("issued token validates in the registry");
        assert!(registry.tokens.last().unwrap().extension_commitment.is_some());
        let sup: Value = serde_json::from_str(&std::fs::read_to_string(dir.join("supply.json")).unwrap()).unwrap();
        assert_eq!(sup["total_supply"], "1000");
        let tx: Value = serde_json::from_str(&std::fs::read_to_string(dir.join("genesis_tx.json")).unwrap()).unwrap();
        assert_eq!(tx["outputs"].as_array().unwrap().len(), 3);
        assert_ne!(tx["inputs"][0]["signatureScript"], "");
        let req: Value = serde_json::from_str(&std::fs::read_to_string(dir.join("submit_request.json")).unwrap()).unwrap();
        assert_eq!(req["method"], "submitTransaction");
        // unsigned dry run needs no key
        let cmd2 = format!("--name Test --ticker TEST --decimals 8 --fund-utxo {TX}:0:5000000000:{} --supply 5 --dry-run", hex(&pk));
        assert_eq!(run_issue(&v(&cmd2)), 0);
        // wrong key refused
        let cmd3 = format!("{cmd2} --key 5555555555555555555555555555555555555555555555555555555555555555");
        assert_eq!(run_issue(&v(&cmd3)), 1);
        // supply mismatch refused
        let cmd4 = format!("{cmd2} --holder self:0:4");
        assert_eq!(run_issue(&v(&cmd4)), 1);
        let _ = std::fs::remove_dir_all(dir);
    }
}
