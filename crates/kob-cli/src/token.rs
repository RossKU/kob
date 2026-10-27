//! `kob token issue`: fixed-supply KCC-20 issuance (the reference program in its standard 3/3 configuration, KOB's
//! standard, or the published public-mint build, also 3/3; owner scheme 0x04 enabled, borrow disabled), created under KCC-23:
//! a preparation transaction declares the metadata, the genesis spends its output (`kob_protocol::issue::build_creation`).
//! `kob token resume` sends a creation whose genesis did not go out; `kob token verify-declaration` checks a launch proof.

use std::path::{Path, PathBuf};
use std::time::Duration;

use kaspa_addresses::{Address, Version};
use kaspa_consensus_core::tx::{TransactionId, TransactionOutpoint};
use kob_protocol::issue::{
    build_creation, hex, hex_array, p2pk_script, rpc_transaction_json, CreationPlan, FundingUtxo, Holder, IssueProgram, IssueSpec,
    DEFAULT_CARRIER, DEFAULT_FEE_RATE, EXTENSION_FIXED_SUPPLY,
};
use kob_protocol::kcc23;
use secp256k1::{Keypair, Secp256k1, SecretKey};
use serde_json::{json, Value};

use crate::wrpc;

/// Usage text of `kob token issue`.
pub const ISSUE_USAGE: &str = "\
kob token issue: issue a fixed-supply KCC-20 token (the reference program in its standard configuration, 3 token
inputs / 3 outputs per transfer, or the published public-mint build of the reference, also 3 / 3)

A creation is two transactions (KCC-23): the PREPARATION spends the funding into one output of the funding key (the
genesis's carriers and fee) and the change, its payload declaring the token metadata (0x00 + JSON: name, symbol,
decimals, then description, logo, website, x, telegram when given); the GENESIS spends that output, so the token's
covenant id commits to the declaration. Both are signed at once; the genesis is sent right after the preparation. If it
does not go out, the preparation output stays an ordinary UTXO of the funding key, and `kob token resume <DIR>` sends
the saved genesis.

The genesis's payload names the states of its token outputs (KOBG, docs/spec/token-metadata.md 3): an indexer lists them to
their owners from the genesis on, so the holder's key finds and spends them from any machine and browser (`kob place`,
`kob send`, the web app) without any file of this run.

USAGE
  kob token issue --name <NAME> --ticker <TICKER> --decimals <N> --fund-utxo <TXID:INDEX:AMOUNT:OWNER>...
                  [--supply <UNITS>] [--holder <OWNER:SCHEME:AMOUNT[:BORROW_SCHEME:GUARD]>...]
                  [--key <HEX | env:VAR | file:PATH>] [--out-dir <DIR>] [--yes | --dry-run] [--node ws://HOST:PORT]

REQUIRED
  --name, --ticker      display name (1..=64 chars) and ticker (2..=12 of A-Z 0-9)
  --decimals            0..=18 (informational; every amount is in base units)
  --fund-utxo           funding UTXO of the preparation, repeatable, all of one key; OWNER = 64-hex x-only pubkey or
                        a kaspa address (P2PK Schnorr). AMOUNT is in sompi.

SUPPLY
  --holder              initial holder, repeatable, one token output each. OWNER = 64-hex | `self` (the funding
                        key) | kaspa address (scheme 0 only). SCHEME = 0 p2pk-schnorr, 1 p2pkh-schnorr,
                        2 p2pkh-ecdsa, 3 p2sh, 4 covenant id. AMOUNT in base units.
                        Scheme 4 states are always issued with borrow disabled (borrow guard rules cannot be set).
  --supply              total supply in base units; must equal the sum of the holders. Without --holder the
                        whole supply goes to `self` (scheme 0).
  --extension-commitment 64-hex (default: 32 zero bytes)
  --allow-borrow        allow BORROW_SCHEME 1..3 on non-0x04 holders (default: borrow disabled everywhere)
  --program             3x3 (default): KOB's standard, the reference program with its 3 / 3 slots (registry
                        kcc20-ref-3x3); public-mint: the KCC20 actor of upstream's published KCC20PublicMint app, 3 / 3
                        slots (registry kcc20-ref-public-mint). Its holders carry the app's context field; the genesis
                        holds only KCC20 holders (no PublicMint, no TokenSeed): a fixed supply. 8x8-prototype: the
                        8 / 8 slot variant, a prototype that is not KOB's issuance (the plan warns).
                        kasparocket-testnet: KaspaRocket's program (the public-mint actor at 10 / 10 slots,
                        KaspaRocketToken11458), issued on testnet-10 only, to test KOB against that launchpad.

TRANSACTION
  --carrier             sompi on each token output (default 1000000000 = 10 KAS)
  --fee                 explicit GENESIS fee in sompi (default: fee-rate x mass); the preparation pays fee-rate x mass
  --fee-rate            sompi per mass unit (default 100)
  --change-to           address or 64-hex pubkey for the change (default: the funding key)
  --key                 signing secret (64 hex). `env:NAME` and `file:PATH` keep it out of shell history.

OUTPUT
  --out-dir             write preparation_tx.json, genesis_tx.json, submit_preparation.json, submit_request.json (the
                        genesis), supply.json, metadata.json, metadata.payload.hex, launch_proof.json,
                        registry_entry.json here. A submitting run without it writes them to ./kob-create-<ID16>.
  --description --logo (or --icon) --website --x --telegram
                        declared on chain in the preparation payload (at most 1024 bytes with name, symbol and
                        decimals) and copied to the registry entry; --x and --telegram take a handle or the address
  --network             mainnet or testnet-10, recorded in metadata/supply; required unless the build is a deployment
                        build (deploy-mainnet, deploy-tn10), which implies its own
  --yes, -y             submit without asking. Without it a terminal is shown the plan and asked (type yes); without a
                        terminal nothing is written or submitted. Not needed with --dry-run.
  --dry-run             build and verify everything offline (signs and executes the funding inputs when --key is
                        given, else the transaction stays unsigned); never submits (excludes --yes)
  --node ws://HOST:PORT submit the signed transactions via wRPC JSON (`submitTransaction`; JSON port 18110 mainnet,
                        18210 testnet-10). Required unless --dry-run; default $KOB_NODE_WRPC (a URL with a password or
                        an API key belongs there: arguments are visible in `ps`).
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
    /// X account (canonical address).
    pub x: Option<String>,
    /// Telegram (canonical address).
    pub telegram: Option<String>,
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
    /// Submit without asking.
    pub yes: bool,
    /// Node URL.
    pub node: Option<String>,
    /// The issued program.
    pub program: IssueProgram,
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
        x: None,
        telegram: None,
        network: String::new(),
        dry_run: false,
        yes: false,
        node: None,
        program: IssueProgram::default(),
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
            "--icon" | "--logo" => a.icon = Some(val("--logo")?),
            "--x" => {
                let v = val("--x")?;
                a.x = Some(kcc23::x_url(&v).ok_or_else(|| format!("--x: `{v}` is not an X handle or https://x.com/ address"))?);
            }
            "--telegram" => {
                let v = val("--telegram")?;
                a.telegram = Some(
                    kcc23::telegram_url(&v)
                        .ok_or_else(|| format!("--telegram: `{v}` is not a Telegram handle or https://t.me/ address"))?,
                );
            }
            "--website" => a.website = Some(val("--website")?),
            "--network" => a.network = val("--network")?,
            "--dry-run" => a.dry_run = true,
            "--yes" | "-y" => a.yes = true,
            "--node" => a.node = Some(val("--node")?),
            "--program" => a.program = IssueProgram::parse(&val("--program")?).map_err(|e| e.to_string())?,
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
    if a.yes && a.dry_run {
        return Err("--yes and --dry-run exclude each other".into());
    }
    a.node = crate::wrpc::node_arg(a.node.take());
    if !a.dry_run && a.node.is_none() {
        return Err(format!("--node (or ${}) is required unless --dry-run", crate::wrpc::NODE_ENV));
    }
    if !a.dry_run && a.key.is_none() {
        return Err("--key is required unless --dry-run (an unsigned transaction cannot be submitted)".into());
    }
    a.network = crate::common::resolve_network(&a.network)?;
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
    spec.x = a.x.clone();
    spec.telegram = a.telegram.clone();
    spec.network = a.network.clone();
    spec.program = a.program;
    Ok(spec)
}

/// Every output document of a creation, as `(file name, content)`.
pub fn output_documents(plan: &CreationPlan) -> Vec<(&'static str, String)> {
    let pretty = |v: &Value| format!("{}\n", serde_json::to_string_pretty(v).expect("json"));
    let prep = rpc_transaction_json(&plan.prep.tx);
    let genesis = plan.genesis.rpc_transaction_json();
    vec![
        ("preparation_tx.json", pretty(&prep)),
        ("genesis_tx.json", pretty(&genesis)),
        ("submit_preparation.json", pretty(&wrpc::submit_request(1, &prep, false))),
        ("submit_request.json", pretty(&wrpc::submit_request(2, &genesis, false))),
        ("supply.json", pretty(&plan.supply_json())),
        ("metadata.json", pretty(&plan.genesis.metadata_json())),
        ("metadata.payload.hex", format!("{}\n", hex(&plan.prep.tx.payload))),
        ("launch_proof.json", pretty(&serde_json::to_value(plan.launch_proof()).expect("proof json"))),
        ("registry_entry.json", pretty(&plan.genesis.registry_entry())),
        (
            "creation.json",
            pretty(&json!({
                "preparation_txid": plan.prep.txid().to_string(),
                "genesis_txid": plan.genesis.txid().to_string(),
                "covenant_id": hex(plan.covenant_id().as_bytes().as_slice()),
                "signed": plan.signed(),
            })),
        ),
    ]
}

/// Write every output document of a creation into `dir`.
pub fn write_outputs(dir: &Path, plan: &CreationPlan) -> Result<Vec<String>, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let docs = output_documents(plan);
    for (name, content) in &docs {
        std::fs::write(dir.join(name), content).map_err(|e| format!("write {name}: {e}"))?;
    }
    Ok(docs.iter().map(|(n, _)| n.to_string()).collect())
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
    let mut plan = build_creation(&spec).map_err(|e| e.to_string())?;
    if let Some(src) = &a.key {
        let sk = read_key(src)?;
        let pk = Keypair::from_secret_key(&Secp256k1::new(), &sk).x_only_public_key().0.serialize();
        if pk != spec.funding[0].owner_pubkey {
            return Err("the signing key does not own the funding UTXO".into());
        }
        plan.sign(&sk).map_err(|e| e.to_string())?;
    }
    let report = plan.verify().map_err(|e| e.to_string())?;
    let g = &plan.genesis;

    println!(
        "token        {} ({}), {} decimals, fixed supply {} base units",
        g.spec.name, g.spec.ticker, g.spec.decimals, g.spec.supply
    );
    println!("covenant id  {}", hex(g.covenant_id.as_bytes().as_slice()));
    println!(
        "template     {} {}  (prefix {} B, suffix {} B, {}/{} slots)",
        g.program.registry_template_id,
        hex(&g.program.template_hash),
        g.program.prefix.len(),
        g.program.suffix.len(),
        g.program.max_token_inputs,
        g.program.max_token_outputs
    );
    println!("declaration  {}", plan.prep.metadata.json());
    println!(
        "preparation  {}  (KCC-23 payload {} B, output 0 = {} sompi to the funding key, fee {} sompi, mass compute {} storage {})",
        plan.prep.txid(),
        plan.prep.tx.payload.len(),
        plan.prep.value,
        plan.prep.fee,
        plan.prep.compute_mass,
        plan.prep.storage_mass
    );
    println!(
        "genesis tx   {}  ({} token outputs, fee {} sompi, mass compute {} storage {}, {} B)",
        g.txid(),
        g.states.len(),
        g.fee,
        report.compute_mass,
        report.storage_mass,
        report.size
    );
    println!("total fee    {} sompi", plan.total_fee());
    println!(
        "verified     KCC-23 binding, covenant group, exact scripts, supply, mass, owner scheme 0x04 self-test{}",
        if report.scripts_executed {
            ", every input executed in the script engine"
        } else {
            " (UNSIGNED: script execution skipped, pass --key)"
        }
    );
    for w in plan.warnings() {
        println!("warning      {w}");
    }
    // a submitting run asks first (the same rule as every other submitting command: `--yes`, or a terminal; else nothing is
    // written or sent), then always keeps the signed transactions: they are what `kob token resume` sends
    if !a.dry_run {
        crate::trade::opts::confirm_with(
            a.yes,
            &a.network,
            &format!("the creation of {} (the preparation, then the genesis {})", g.spec.ticker, g.txid()),
        )?;
    }
    let dir = match (&a.out_dir, a.dry_run) {
        (Some(d), _) => Some(d.clone()),
        (None, false) => Some(PathBuf::from(format!("kob-create-{}", &hex(g.covenant_id.as_bytes().as_slice())[..16]))),
        (None, true) => None,
    };
    if let Some(dir) = &dir {
        let files = write_outputs(dir, &plan)?;
        println!("wrote        {} to {}", files.join(", "), dir.display());
    } else {
        println!("(no --out-dir: nothing written)");
    }
    if a.dry_run {
        println!("dry run: nothing submitted");
        return Ok(());
    }
    let dir = dir.expect("a submitting run has a directory");
    let node = a.node.as_deref().expect("checked");
    wrpc::check_node_network(node, &a.network, Duration::from_secs(30))?;
    let ids = (plan.prep.txid().to_string(), g.txid().to_string());
    submit_creation(node, (&rpc_transaction_json(&plan.prep.tx), &ids.0), (&g.rpc_transaction_json(), &ids.1), &dir, false)?;
    // the genesis outputs the funding key holds (scheme 0): recorded so that `kob send`, `kob place` and `kob merge` can
    // spend them before the indexer has the genesis. The genesis names the states of its outputs in its payload, so an indexer of
    // this build lists them to the owner on any machine (`GET /v1/token-utxos`); this record only bridges the indexer's lag
    // and an indexer that has not been upgraded.
    let funder = a.fund[0].owner;
    let token: [u8; 32] = g.covenant_id.as_bytes();
    let own: Vec<([u8; 32], u32, kob_protocol::state::TokenState)> = g
        .states
        .iter()
        .enumerate()
        .filter(|(_, s)| s.owner_scheme == 0 && s.owner == funder)
        .map(|(i, s)| {
            let k = kob_protocol::state::Kcc20State {
                amount: s.amount as i64,
                owner: s.owner,
                owner_scheme: s.owner_scheme,
                borrow_scheme: s.borrow_scheme,
                borrow_guard: s.borrow_guard,
                extension_commitment: s.extension_commitment,
            };
            (token, i as u32, kob_protocol::state::TokenState::Kcc20(k))
        })
        .collect();
    crate::recent::record_outputs(&a.network, &funder, own, &g.txid().to_string());
    Ok(())
}

/// How many times the genesis is offered to the node, and the pause between: the node normally takes it at once (its input is the
/// preparation it just accepted into its mempool); a node that has not yet seen the preparation gets a few seconds.
const GENESIS_ATTEMPTS: u32 = 6;
const GENESIS_RETRY: Duration = Duration::from_secs(2);

/// Sends the preparation, then the genesis (retried a few times). `resuming`: a node error on the preparation is reported and
/// the genesis is still tried (the preparation may be on chain already). On failure the error says how to resume from `dir`.
fn submit_creation(node: &str, prep: (&Value, &str), genesis: (&Value, &str), dir: &Path, resuming: bool) -> Result<(), String> {
    let resume_hint = format!(
        "the preparation output stays an ordinary UTXO of the funding key; send the saved genesis with `kob token resume {} --node <URL>`",
        dir.display()
    );
    match wrpc::submit_transaction(node, prep.0, false, Duration::from_secs(30)) {
        Ok(txid) => {
            println!("submitted    preparation {txid}");
            if txid != prep.1 {
                eprintln!("warning: the node reported transaction id {txid}, expected {}", prep.1);
            }
        }
        Err(e) if resuming => println!("preparation  not accepted again ({e}): it may be on chain already; sending the genesis"),
        Err(e) => return Err(format!("the preparation was not accepted: {e} (nothing was spent)")),
    }
    let mut last = String::new();
    for attempt in 1..=GENESIS_ATTEMPTS {
        match wrpc::submit_transaction(node, genesis.0, false, Duration::from_secs(30)) {
            Ok(txid) => {
                println!("submitted    genesis {txid}");
                if txid != genesis.1 {
                    eprintln!("warning: the node reported transaction id {txid}, expected {}", genesis.1);
                }
                return Ok(());
            }
            Err(e) => {
                last = e.to_string();
                if attempt < GENESIS_ATTEMPTS {
                    std::thread::sleep(GENESIS_RETRY);
                }
            }
        }
    }
    Err(format!("the genesis was not accepted ({last}): {resume_hint}"))
}

/// Usage text of `kob token resume`.
pub const RESUME_USAGE: &str = "\
kob token resume: send a creation whose genesis did not go out

USAGE
  kob token resume <DIR> [--node ws://HOST:PORT]

DIR is the directory `kob token issue` wrote (creation.json, and preparation_tx.json and genesis_tx.json, both signed). The preparation is sent
again (a node that already has it refuses it: that is fine) and then the genesis. Nothing is signed: the saved transactions
are sent as they are. If the preparation output was spent elsewhere meanwhile, the genesis cannot be sent; that KAS was
the funding key's own.
";

/// Run `kob token resume`; returns the process exit code.
pub fn run_resume(args: &[String]) -> i32 {
    if args.is_empty() || args.iter().any(|a| a == "--help" || a == "-h") {
        print!("{RESUME_USAGE}");
        return i32::from(args.is_empty()) * 2;
    }
    let run = || -> Result<(), String> {
        let mut dir = None;
        let mut node = None;
        let mut it = args.iter();
        while let Some(a) = it.next() {
            match a.as_str() {
                "--node" => node = Some(it.next().cloned().ok_or("--node needs a value")?),
                other if dir.is_none() && !other.starts_with("--") => dir = Some(PathBuf::from(other)),
                other => return Err(format!("unknown argument `{other}` (see `kob token resume --help`)")),
            }
        }
        let dir = dir.ok_or("give the directory `kob token issue` wrote")?;
        let node = wrpc::node_arg(node).ok_or_else(|| format!("--node (or ${}) is required", wrpc::NODE_ENV))?;
        let read = |name: &str| -> Result<Value, String> {
            let text = std::fs::read_to_string(dir.join(name)).map_err(|e| format!("read {name}: {e}"))?;
            serde_json::from_str(&text).map_err(|e| format!("{name}: {e}"))
        };
        let (prep, genesis, ids) = (read("preparation_tx.json")?, read("genesis_tx.json")?, read("creation.json")?);
        let (prep_id, genesis_id) = (ids["preparation_txid"].as_str().unwrap_or(""), ids["genesis_txid"].as_str().unwrap_or(""));
        let signed = |v: &Value| {
            v["inputs"]
                .as_array()
                .is_some_and(|i| !i.is_empty() && i.iter().all(|x| x["signatureScript"].as_str().is_some_and(|s| !s.is_empty())))
        };
        if !signed(&prep) || !signed(&genesis) {
            return Err("the saved transactions are not signed (a dry run without --key): issue again".into());
        }
        let g = &genesis["inputs"][0]["previousOutpoint"];
        if genesis["inputs"].as_array().map(Vec::len) != Some(1) || g["transactionId"].as_str() != Some(prep_id) || g["index"] != 0 {
            return Err("genesis_tx.json does not spend output 0 of preparation_tx.json".into());
        }
        submit_creation(&node, (&prep, prep_id), (&genesis, genesis_id), &dir, true)
    };
    match run() {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("error: {e}");
            1
        }
    }
}

/// Usage text of `kob token verify-declaration`.
pub const VERIFY_USAGE: &str = "\
kob token verify-declaration: check a KCC-23 launch proof

USAGE
  kob token verify-declaration <launch_proof.json>

The proof (kob-kcc23-proof/1: the preparation and the genesis in the fields their ids commit to, and the covenant id) may
come from anyone: an indexer (GET /v1/tokens/<id>/declaration), a community list, `kob token issue`. Both transaction ids and
the covenant id are recomputed; the declaration is printed only when it is bound to that covenant id.
";

/// Run `kob token verify-declaration`; returns the process exit code.
pub fn run_verify_declaration(args: &[String]) -> i32 {
    let [path] = args else {
        print!("{VERIFY_USAGE}");
        return 2;
    };
    if path == "--help" || path == "-h" {
        print!("{VERIFY_USAGE}");
        return 0;
    }
    let run = || -> Result<(), String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("read {path}: {e}"))?;
        let proof: kcc23::LaunchProof = serde_json::from_str(&text).map_err(|e| format!("{path}: {e}"))?;
        let v = proof.verify()?;
        println!("covenant id  {}", hex(&v.covenant_id));
        println!("preparation  {}", hex(&v.preparation_txid));
        println!("genesis tx   {}", hex(&v.genesis_txid));
        println!("declaration  {}", v.json);
        let t = &v.token;
        println!(
            "token fields name {:?}, symbol {:?}, decimals {}",
            t.name.as_deref().unwrap_or("-"),
            t.symbol.as_deref().unwrap_or("-"),
            t.decimals.map_or("undeclared".to_string(), |d| d.to_string())
        );
        for i in &t.issues {
            println!("ignored      {i}");
        }
        Ok(())
    };
    match run() {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("error: {e}");
            1
        }
    }
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
        format!("--name Test --ticker TEST --decimals 8 --fund-utxo {TX}:1:5000000000:{PK} --dry-run --network testnet-10")
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
            "--name Test --ticker TEST --decimals 8 --fund-utxo {TX}:0:5000000000:{} --supply 1000 --holder self:0:900 --holder {PK}:4:100 --dry-run --network testnet-10 --key {sk} --out-dir {}",
            hex(&pk),
            dir.display()
        );
        assert_eq!(run_issue(&v(&cmd)), 0);
        let reg: Value = serde_json::from_str(&std::fs::read_to_string(dir.join("registry_entry.json")).unwrap()).unwrap();
        assert_eq!(reg["ticker"], "TEST");
        assert_eq!(reg["template_id"], "kcc20-ref-3x3");
        // the emitted entry is a valid registry token against the shipped templates
        let mut registry = kob_protocol::registry::Registry::default_registry();
        registry.tokens.push(serde_json::from_value(reg.clone()).expect("registry entry parses as a registry token"));
        registry.validate().expect("issued token validates in the registry");
        assert!(registry.tokens.last().unwrap().extension_commitment.is_some());
        let sup: Value = serde_json::from_str(&std::fs::read_to_string(dir.join("supply.json")).unwrap()).unwrap();
        assert_eq!(sup["total_supply"], "1000");
        let tx: Value = serde_json::from_str(&std::fs::read_to_string(dir.join("genesis_tx.json")).unwrap()).unwrap();
        assert_eq!(
            tx["outputs"].as_array().unwrap().len(),
            2,
            "two token outputs, no change: the preparation output funds it exactly"
        );
        assert_ne!(tx["inputs"][0]["signatureScript"], "");
        // the genesis names the states of its two outputs (KOBG), so the issuer's key finds them from any machine through an indexer
        let named = kob_protocol::genesis_states::decode(&kob_protocol::json::from_hex(tx["payload"].as_str().unwrap()).unwrap())
            .expect("a genesis-states payload");
        assert_eq!(named.len(), 2);
        assert!(named.iter().all(|span| span.len() == 112));
        assert_eq!(hex(&named[0][10..42]), hex(&pk), "the first output's owner is the key (self)");
        assert_eq!(&named[0][1..9], &900i64.to_le_bytes(), "and its amount");
        assert_eq!(&named[1][1..9], &100i64.to_le_bytes());
        let prep: Value = serde_json::from_str(&std::fs::read_to_string(dir.join("preparation_tx.json")).unwrap()).unwrap();
        assert!(prep["payload"]
            .as_str()
            .unwrap()
            .starts_with("007b226e616d65223a2254657374222c2273796d626f6c223a2254455354222c22646563696d616c73223a38"));
        let ids: Value = serde_json::from_str(&std::fs::read_to_string(dir.join("creation.json")).unwrap()).unwrap();
        assert_eq!(tx["inputs"][0]["previousOutpoint"]["transactionId"], ids["preparation_txid"]);
        assert_eq!(tx["inputs"][0]["previousOutpoint"]["index"], 0);
        assert_eq!(sup["kcc23"]["preparation_txid"], ids["preparation_txid"]);
        // the launch proof verifies from the file, as anyone would check it
        assert_eq!(run_verify_declaration(&[dir.join("launch_proof.json").display().to_string()]), 0);
        let mut proof: Value = serde_json::from_str(&std::fs::read_to_string(dir.join("launch_proof.json")).unwrap()).unwrap();
        proof["covenantId"] = Value::String("11".repeat(32));
        let forged = dir.join("forged_proof.json");
        std::fs::write(&forged, proof.to_string()).unwrap();
        assert_eq!(run_verify_declaration(&[forged.display().to_string()]), 1);
        let req: Value = serde_json::from_str(&std::fs::read_to_string(dir.join("submit_request.json")).unwrap()).unwrap();
        assert_eq!(req["method"], "submitTransaction");
        // unsigned dry run needs no key
        let cmd2 = format!(
            "--name Test --ticker TEST --decimals 8 --fund-utxo {TX}:0:5000000000:{} --supply 5 --dry-run --network testnet-10",
            hex(&pk)
        );
        assert_eq!(run_issue(&v(&cmd2)), 0);
        // wrong key refused
        let cmd3 = format!("{cmd2} --key 5555555555555555555555555555555555555555555555555555555555555555");
        assert_eq!(run_issue(&v(&cmd3)), 1);
        // supply mismatch refused
        let cmd4 = format!("{cmd2} --holder self:0:4");
        assert_eq!(run_issue(&v(&cmd4)), 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn x_and_telegram_take_handles() {
        let a = parse_issue_args(&v(&format!(
            "{} --supply 1 --x @kaspa --telegram https://telegram.me/kaspa_chat --logo ipfs://cid",
            base()
        )))
        .unwrap();
        assert_eq!(
            (a.x.as_deref(), a.telegram.as_deref(), a.icon.as_deref()),
            (Some("https://x.com/kaspa"), Some("https://t.me/kaspa_chat"), Some("ipfs://cid"))
        );
        assert!(parse_issue_args(&v(&format!("{} --supply 1 --x https://evil.example/kaspa", base()))).is_err());
        let s = to_spec(&a).unwrap();
        assert!(s
            .token_metadata()
            .json()
            .ends_with(r#""logo":"ipfs://cid","x":"https://x.com/kaspa","telegram":"https://t.me/kaspa_chat"}"#));
    }

    /// `--yes` and `--dry-run` exclude each other, as in every submitting command.
    #[test]
    fn yes_and_dry_run_exclude_each_other() {
        let base =
            format!("--name Test --ticker TEST --decimals 8 --fund-utxo {TX}:1:5000000000:{PK} --supply 1 --network testnet-10");
        assert!(parse_issue_args(&v(&format!("{base} --dry-run --yes"))).unwrap_err().contains("exclude each other"));
        assert!(parse_issue_args(&v(&format!("{base} --dry-run -y"))).is_err());
        let a = parse_issue_args(&v(&format!("{base} --node ws://x:1 --key {PK} --yes"))).unwrap();
        assert!(a.yes && !a.dry_run);
    }

    /// A submitting run sends the preparation, then the genesis spending it; `kob token resume` sends the saved pair again.
    #[test]
    fn submit_sends_the_preparation_then_the_genesis_and_resume_resends_them() {
        let sk = "3333333333333333333333333333333333333333333333333333333333333333";
        let pk = Keypair::from_secret_key(&Secp256k1::new(), &SecretKey::from_slice(&hex_array::<32>(sk).unwrap()).unwrap())
            .x_only_public_key()
            .0
            .serialize();
        let (url, state) = crate::wrpc::mock::start(vec![]);
        let dir = std::env::temp_dir().join(format!("kob-create-test-{}", std::process::id()));
        let cmd = format!(
            "--name Test --ticker TEST --decimals 8 --fund-utxo {TX}:0:5000000000:{} --supply 1000 --network testnet-10 --key {sk} --node {url} --out-dir {}",
            hex(&pk),
            dir.display()
        );
        // without --yes and without a terminal nothing is written and nothing is sent
        if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
            assert_eq!(run_issue(&v(&cmd)), 1, "refused without --yes");
            assert!(state.lock().unwrap().submitted.is_empty(), "nothing submitted");
            assert!(!dir.exists(), "nothing written");
        }
        assert_eq!(run_issue(&v(&format!("{cmd} --yes"))), 0);
        let ids: Value = serde_json::from_str(&std::fs::read_to_string(dir.join("creation.json")).unwrap()).unwrap();
        let sent = state.lock().unwrap().submitted.clone();
        assert_eq!(sent.len(), 2, "preparation, then genesis");
        assert!(sent[0]["payload"].as_str().unwrap().starts_with("007b"));
        assert_eq!(sent[1]["inputs"][0]["previousOutpoint"]["transactionId"], ids["preparation_txid"]);
        // the genesis names the state of its one token output (KOBG | 1 | 1 | 112 | the state span)
        assert!(sent[1]["payload"].as_str().unwrap().starts_with("4b4f4247010170"), "{}", sent[1]["payload"]);
        assert_eq!(run_resume(&v(&format!("{} --node {url}", dir.display()))), 0);
        let sent = state.lock().unwrap().submitted.clone();
        assert_eq!(sent.len(), 4);
        assert_eq!((&sent[2], &sent[3]), (&sent[0], &sent[1]), "resume sends the saved, signed pair unchanged");
        // a dry run without a key writes unsigned transactions: resume refuses them
        let dry = std::env::temp_dir().join(format!("kob-create-dry-{}", std::process::id()));
        let cmd = format!(
            "--name Test --ticker TEST --decimals 8 --fund-utxo {TX}:0:5000000000:{} --supply 5 --dry-run --network testnet-10 --out-dir {}",
            hex(&pk),
            dry.display()
        );
        assert_eq!(run_issue(&v(&cmd)), 0);
        assert_eq!(run_resume(&v(&format!("{} --node {url}", dry.display()))), 1);
        let _ = std::fs::remove_dir_all(dir);
        let _ = std::fs::remove_dir_all(dry);
    }
}
