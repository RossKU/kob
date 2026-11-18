//! `kob token create`, `kob token resume` and `kob token verify-declaration`.
//!
//! `create` issues a fixed-supply KCC-20 token under KCC-23. By default it issues the published
//! public-mint build of the reference program, the standard KCC-20 program with 3 token inputs and
//! 3 outputs. `--program 3x3` names the standalone 3/3 reference program instead. Owner scheme
//! 0x04 is enabled, and borrow is disabled unless `--allow-borrow` is given.
//!
//! A creation is two transactions. The preparation declares the metadata, and the genesis spends
//! its output (`kob_protocol::issue::build_creation`). `resume` sends a creation whose genesis did
//! not go out. `verify-declaration` checks a launch proof.

use std::path::{Path, PathBuf};
use std::time::Duration;

use kaspa_addresses::{Address, Version};
use kaspa_consensus_core::tx::{TransactionId, TransactionOutpoint};
use kob_protocol::issue::{
    build_creation, hex, hex_array, p2pk_script, rpc_transaction_json, CreationPlan, FundingUtxo, Holder, IssueError, IssueProgram,
    IssueSpec, DEFAULT_CARRIER, DEFAULT_FEE_RATE, EXTENSION_FIXED_SUPPLY,
};
use kob_protocol::kcc23;
use kob_protocol::tx::{BuiltTx, KeyUtxo};
use secp256k1::{Keypair, Secp256k1, SecretKey};
use serde_json::{json, Value};

use crate::trade::market::TokenMarket;
use crate::trade::plan::{Clock, Plan, Severity};
use crate::wrpc;

/// Usage text of `kob token create`.
pub const ISSUE_USAGE: &str = "\
kob token create: create a fixed-supply KCC-20 token (by default the published public-mint build of the KCC-20
reference, 3 token inputs / 3 outputs per transfer, the program other indexers such as kascov.io list as a KCC-20
token; `--program 3x3` names the standalone 3 / 3 reference program instead)

A creation is two transactions (KCC-23): the PREPARATION spends the funding into one output of the funding key (the
genesis's carriers and fee) and the change, its payload declaring the token metadata (0x00 + JSON: name, symbol,
decimals, then description, image, image_hash, website, x, telegram when given); the GENESIS spends that output, so the token's
covenant id commits to the declaration. Both are signed at once; the genesis is sent right after the preparation. If it
does not go out, the preparation output stays an ordinary UTXO of the funding key, and `kob token resume <DIR>` sends
the saved genesis.

The genesis's payload names the states of its token outputs (KOBG, docs/spec/token-metadata.md 3): an indexer lists them to
their owners from the genesis on, so the holder's key finds and spends them from any machine and browser (`kob place`,
`kob send`, the web app) without any file of this run.

USAGE
  kob token create --name <NAME> --ticker <TICKER> --decimals <N> [--fund-utxo <TXID:INDEX:AMOUNT:OWNER>...]
                  [--supply <UNITS>] [--holder <OWNER:SCHEME:AMOUNT[:BORROW_SCHEME:GUARD]>...]
                  [--sell price|curve [--keep PERCENT] [sale options]]
                  [--key-file <PATH> | --key-env <NAME>] [--out-dir <DIR>] [--yes | --dry-run] [--node ws://HOST:PORT]

REQUIRED
  --name, --ticker      display name (1..=64 chars) and ticker (2..=12 of A-Z 0-9)
  --decimals            0..=18 (informational; every amount is in base units)

FUNDING
  --fund-utxo           funding UTXO of the preparation, repeatable, all of one key; OWNER = 64-hex x-only pubkey or
                        a kaspa address (P2PK Schnorr). AMOUNT is in sompi.
                        Without it the key's plain KAS UTXOs on the node fund the creation: the fewest that cover it,
                        largest first (the key is then needed, also for --dry-run, which then reads the node).

AFTER CREATION (as the web app's Create token; default: the wallet keeps everything)
  --sell price          sell the funding key's share at one price: a limit sell (GTC) sent right after the genesis,
                        spending the genesis output and the preparation's change
    --price P           KAS per whole token, e.g. 0.01KAS (default: 10000 KAS over the amount for sale)
  --sell curve          sell it on a stepped curve (a KobCurveAsk created in the genesis, no expiry)
    --levels N          price levels, 1 to 1000 (default 100)
    --total KAS         what a sell-out realises (default 10000KAS)
    --multiple M        the last level's price over the first's (default 5)
  --keep PERCENT        the share kept in the wallet as its own output (0 to 100, default 0)
                        One confirmation (or --yes) covers the creation and the sale. A dry run with --sell price
                        reads the node's clock.

SUPPLY
  --holder              initial holder, repeatable, one token output each. OWNER = 64-hex | `self` (the funding
                        key) | kaspa address (scheme 0 only). SCHEME = 0 p2pk-schnorr, 1 p2pkh-schnorr,
                        2 p2pkh-ecdsa, 3 p2sh, 4 covenant id. AMOUNT in base units.
                        Scheme 4 states are always issued with borrow disabled (borrow guard rules cannot be set).
  --supply              total supply in base units; must equal the sum of the holders. Without --holder the
                        whole supply goes to `self` (scheme 0).
  --extension-commitment 64-hex (default: 32 zero bytes)
  --allow-borrow        allow BORROW_SCHEME 1..3 on non-0x04 holders (default: borrow disabled everywhere)
  --program             public-mint (default): the KCC20 actor of upstream's published KCC20PublicMint app, 3 / 3
                        slots (registry kcc20-ref-public-mint), the de-facto standard that kascov.io lists as a
                        KCC-20 token. Its holders carry the app's context field; the genesis holds only KCC20 holders
                        (no PublicMint, no TokenSeed): a fixed supply, nobody can mint more.
                        3x3: the standalone reference program with its 3 / 3 slots (registry kcc20-ref-3x3): a
                        transfer costs less (testnet-10, 2026-11-04: 1 -> 1 0.00678 KAS against 0.00901 KAS) but
                        other indexers (kascov.io) do not recognise it.
                        kasparocket-testnet: KaspaRocket's program (the public-mint actor at 10 / 10 slots,
                        KaspaRocketToken11458), issued on testnet-10 only, to test KOB against that launchpad.

TRANSACTION
  --carrier             sompi on each token output (default 1000000000 = 10 KAS)
  --fee                 explicit GENESIS fee in sompi (default: fee-rate x mass); the preparation pays fee-rate x mass
  --fee-rate            sompi per mass unit (default 100)
  --allow-high-fee      pay a fee above the ceiling (default: a transaction paying more than 4 times the node's fee
                        estimate on its mass is refused: [FEE_ABOVE_CEILING], exit 1; a dry run checks at the relay floor)
  --change-to           address or 64-hex pubkey for the change (default: the funding key)
  --key-file PATH       the signing key: a file holding the 64-hex secret
  --key-env NAME        ... or the environment variable holding it (default $KOB_KEY when the run signs)
                        (`--key` was removed: a key on the command line is visible to other users)

OUTPUT
  --out-dir             write preparation_tx.json, genesis_tx.json, submit_preparation.json, submit_request.json (the
                        genesis), supply.json, metadata.json, metadata.payload.hex, launch_proof.json,
                        registry_entry.json here. A submitting run without it writes them to ./kob-create-<ID16>.
  --description --image --website --x --telegram
                        declared on chain in the preparation payload (at most 1024 bytes with name, symbol and
                        decimals) and copied to the registry entry; --x and --telegram take a handle or the address
  --image <URL>         the token's picture: an https:// or ipfs:// address, declared as `image`.
  --image-file <PATH>   the picture file (PNG, JPEG, WebP or GIF, at most 1500000 bytes, no SVG) whose SHA-256 is
                        declared as `image_hash`, right after `image`: viewers and explorers can then check that the
                        picture at the address is the one you meant. The hash cannot be changed later. Needs --image.
  --image-hash <HEX>    the same hash given directly (64 hex characters, SHA-256 of the file); with --image-file the
                        two must agree. Needs --image.
  --yes, -y             submit without asking. Without it a terminal is shown the plan and asked (type yes); without a
                        terminal nothing is written or submitted. Not needed with --dry-run.
  --dry-run             build and verify everything offline (signs and executes the funding inputs when a key is
                        named (--key-file or --key-env), else the transaction stays unsigned); never submits (excludes --yes)
  --node URL            where to submit the signed transactions (`submitTransaction`): a JSON wRPC endpoint (port 18110
                        mainnet, 18210 testnet-10), a Borsh one (ws://HOST:17110, wss://.../borsh) or `resolver`. Default
                        $KOB_NODE_WRPC (a URL with a password or an API key belongs there: arguments are visible in `ps`),
                        else the public nodes the Kaspa resolvers name (two hosts that must agree on the outputs spent).
                        The node must run the network named: checked before anything is submitted.
  --network NET         mainnet (default; a deployment build is bound to its own) or testnet-10, recorded in metadata/supply;
                        --testnet and --netsuffix=10 are --network testnet-10
";

/// Where the signing key comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeySource {
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

/// Parsed arguments of `kob token create`.
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
    /// `--allow-high-fee`: no fee ceiling.
    pub allow_high_fee: bool,
    /// Image address (declared as `image`; `--image`).
    pub icon: Option<String>,
    /// SHA-256 of the picture file's bytes (64 lowercase hex; declared as `image_hash`).
    pub image_hash: Option<String>,
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
    /// "After creation": the sale flags.
    pub sale: crate::token_sale::SaleArgs,
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

/// The usage error of the removed `--key`: it names what replaces it and never quotes the key.
pub(crate) const KEY_FLAG_REMOVED: &str = "--key was removed: a key on the command line is visible to other users (ps, shell history); use --key-file PATH or --key-env NAME, or set the environment variable KOB_KEY";

/// Whether `flag` is the removed `--key` (also `--key=VALUE`).
pub(crate) fn is_removed_key_flag(flag: &str) -> bool {
    flag == "--key" || flag.starts_with("--key=")
}

/// The signing key of `token create` and `recover`: `--key-file PATH` or `--key-env NAME`.
///
/// A second key option is refused.
pub(crate) fn key_option(flag: &str, value: &str, have: bool) -> Result<KeySource, String> {
    if have {
        return Err("give the key once: --key-file PATH or --key-env NAME".into());
    }
    Ok(match flag {
        "--key-file" => KeySource::File(PathBuf::from(value)),
        _ => KeySource::Env(value.to_string()),
    })
}

/// The key when none was named: the environment variable `KOB_KEY`, when it is set.
pub(crate) fn default_key() -> Option<KeySource> {
    std::env::var(crate::trade::opts::KEY_ENV)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .map(|_| KeySource::Env(crate::trade::opts::KEY_ENV.into()))
}

/// Parse the arguments after `kob token create`.
pub fn parse_issue_args(args: &[String]) -> Result<IssueArgs, String> {
    parse_issue_args_with(args, default_key())
}

/// [`parse_issue_args`] with the key a signing run falls back to when none is named (`$KOB_KEY`).
pub fn parse_issue_args_with(args: &[String], fallback_key: Option<KeySource>) -> Result<IssueArgs, String> {
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
        image_hash: None,
        website: None,
        x: None,
        telegram: None,
        network: String::new(),
        dry_run: false,
        yes: false,
        allow_high_fee: false,
        node: None,
        program: IssueProgram::default(),
        sale: Default::default(),
    };
    let (mut have_name, mut have_ticker, mut have_decimals) = (false, false, false);
    let mut image_hash: Option<String> = None;
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
            k if is_removed_key_flag(k) => return Err(KEY_FLAG_REMOVED.into()),
            "--key-file" | "--key-env" => a.key = Some(key_option(flag, &val(flag)?, a.key.is_some())?),
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
            "--image" => a.icon = Some(val("--image")?),
            "--icon" | "--logo" => return Err(format!("{flag} was removed: use --image")),
            "--image-file" => {
                let p = val("--image-file")?;
                let bytes = std::fs::read(&p).map_err(|e| format!("--image-file {p}: {e}"))?;
                let h = kcc23::image_file_hash(&bytes).map_err(|e| format!("--image-file {p}: {e}"))?;
                if image_hash.as_ref().is_some_and(|x| *x != h) {
                    return Err("--image-file and --image-hash disagree".into());
                }
                image_hash = Some(h);
            }
            "--image-hash" => {
                let v = val("--image-hash")?;
                if v.len() != 64 || !v.bytes().all(|b| b.is_ascii_hexdigit()) {
                    return Err(format!("--image-hash: `{v}` is not 64 hexadecimal characters"));
                }
                let h = v.to_ascii_lowercase();
                if image_hash.as_ref().is_some_and(|x| *x != h) {
                    return Err("--image-file and --image-hash disagree".into());
                }
                image_hash = Some(h);
            }
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
            "--allow-high-fee" => a.allow_high_fee = true,
            "--node" => a.node = Some(val("--node")?),
            "--program" => a.program = IssueProgram::parse(&val("--program")?).map_err(|e| e.to_string())?,
            k if crate::token_sale::SALE_FLAGS.contains(&k) => a.sale.set(k, val(k)?),
            other => return Err(format!("unknown argument `{other}` (see `kob token create --help`)")),
        }
    }
    if !have_name || !have_ticker || !have_decimals {
        return Err("--name, --ticker and --decimals are required".into());
    }
    if image_hash.is_some() && a.icon.is_none() {
        return Err("--image-file / --image-hash need --image (the picture's address)".into());
    }
    a.image_hash = image_hash;
    a.sale.check()?;
    if a.holders.is_empty() && a.supply.is_none() {
        return Err("give --supply and/or at least one --holder".into());
    }
    if a.yes && a.dry_run {
        return Err("--yes and --dry-run exclude each other".into());
    }
    // A dry run signs only when a key is named. A run that submits falls back to $KOB_KEY.
    if !a.dry_run && a.key.is_none() {
        a.key = fallback_key;
    }
    if !a.dry_run && a.key.is_none() {
        return Err("a key is required unless --dry-run (an unsigned transaction cannot be submitted): give --key-file PATH or --key-env NAME, or set the environment variable KOB_KEY".into());
    }
    if a.fund.is_empty() && a.key.is_none() {
        return Err(
            "give --fund-utxo, or the key whose KAS UTXOs on the node fund the creation (--key-file PATH or --key-env NAME)".into()
        );
    }
    a.network = crate::common::resolve_network(&a.network)?;
    a.node = Some(crate::wrpc::node_for(a.node.take(), &a.network));
    Ok(a)
}

/// Read the secret key from its source.
pub(crate) fn read_key(src: &KeySource) -> Result<SecretKey, String> {
    let raw = match src {
        KeySource::Env(v) => std::env::var(v).map_err(|_| format!("environment variable {v} is not set"))?,
        KeySource::File(p) => std::fs::read_to_string(p).map_err(|e| format!("cannot read key file {}: {e}", p.display()))?,
    };
    // A fixed message: the text may be a secret with one stray character, so it is never quoted.
    let bytes: [u8; 32] = hex_array(raw.trim()).map_err(|_| "signing key is not 64 hex characters".to_string())?;
    SecretKey::from_slice(&bytes).map_err(|_| "signing key is not a valid secret key".to_string())
}

/// Build the library spec from parsed arguments (resolving `self` holders against the funding key).
#[cfg(test)]
pub fn to_spec(a: &IssueArgs) -> Result<IssueSpec, String> {
    to_spec_with(a, &a.fund)
}

/// [`to_spec`] funded by `fund` (`--fund-utxo`, or what the node holds for the key).
pub fn to_spec_with(a: &IssueArgs, fund: &[FundArg]) -> Result<IssueSpec, String> {
    let funder = fund.first().ok_or("no funding UTXO")?.owner;
    if fund.iter().any(|f| f.owner != funder) {
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
    let funding = fund.iter().map(|f| FundingUtxo { outpoint: f.outpoint, amount: f.amount, owner_pubkey: f.owner }).collect();
    let mut spec = IssueSpec::new(&a.name, &a.ticker, a.decimals, supply, holders, funding);
    spec.extension_commitment = a.extension_commitment;
    spec.carrier = a.carrier;
    spec.fee = a.fee;
    spec.fee_rate = a.fee_rate;
    spec.allow_borrow = a.allow_borrow;
    spec.change_spk = a.change_to.map(|pk| p2pk_script(&pk));
    spec.description = a.description.clone();
    spec.icon = a.icon.clone();
    spec.image_hash = a.image_hash.clone();
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

/// Run `kob token create`; returns the process exit code.
pub fn run_issue(args: &[String]) -> i32 {
    if let Some(code) = crate::help::help_or_usage(args, &[ISSUE_USAGE], false) {
        return code;
    }
    // A mistake in the arguments is a usage error (2). Anything the run meets after that is 1.
    let a = match parse_issue_args(args) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("error: {e}");
            return crate::help::USAGE_EXIT;
        }
    };
    match issue(&a) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("error: {e}");
            1
        }
    }
}

/// The creation and its sale, planned: the spec, the sale, the creation, the limit sell of a
/// one-price sale, and how many of the funding candidates it spends.
type Planned = (IssueSpec, Option<crate::token_sale::Sale>, CreationPlan, Option<(TokenMarket, Plan)>, usize);

/// Plans the creation with the fewest of `pool` (largest first) that pay for it and, with `--sell
/// price`, for its limit sell. `spare` are the same candidates as UTXOs of the node (automatic
/// funding): those the creation leaves fund the limit sell beyond the preparation's change.
/// `--fund-utxo` is used whole.
fn plan_creation(a: &IssueArgs, pool: &[FundArg], spare: &[KeyUtxo], clock: Option<Clock>) -> Result<Planned, String> {
    let funder = pool.first().ok_or("no funding UTXO")?.owner;
    let first = if spare.is_empty() { pool.len() } else { 1 };
    for n in first..=pool.len() {
        let more = n < pool.len();
        let mut spec = to_spec_with(a, &pool[..n])?;
        let sale = crate::token_sale::apply(&mut spec, &a.sale, funder)?;
        let plan = match build_creation(&spec) {
            Ok(p) => p,
            Err(IssueError::InsufficientFunds { .. }) if more => continue,
            Err(e) => return Err(e.to_string()),
        };
        let ask = match (&sale, clock) {
            (Some(s), Some(c)) if s.price.is_some() => {
                let (m, p) = crate::token_sale::plan_ask(&plan, s, funder, spare.get(n..).unwrap_or_default(), c, a.fee_rate)?;
                if crate::token_sale::short_of_kas(&p) && more {
                    continue;
                }
                Some((m, p))
            }
            _ => None,
        };
        return Ok((spec, sale, plan, ask, n));
    }
    Err("no funding UTXO".into())
}

fn issue(a: &IssueArgs) -> Result<(), String> {
    let sk = a.key.as_ref().map(read_key).transpose()?;
    let key_pk = sk.map(|k| Keypair::from_secret_key(&Secp256k1::new(), &k).x_only_public_key().0.serialize());
    let price_sale = a.sale.mode.as_deref() == Some("price");
    let auto = a.fund.is_empty();
    let node = a.node.as_deref().ok_or("no node")?;
    if auto || price_sale {
        wrpc::check_node_network(node, &a.network, Duration::from_secs(30))?;
    }
    // Automatic funding: the key's plain KAS UTXOs on the node, largest first.
    let spare: Vec<KeyUtxo> = if auto {
        let pk = key_pk.ok_or("give --fund-utxo, or the key whose KAS UTXOs fund the creation")?;
        let mut u = crate::common::maker_funding(node, &a.network, &pk)?;
        u.sort_by(|x, y| y.utxo.amount.cmp(&x.utxo.amount));
        if u.is_empty() {
            return Err(format!("the key {} holds no spendable KAS on the node", hex(&pk)));
        }
        u
    } else {
        vec![]
    };
    let pool: Vec<FundArg> = if auto {
        spare.iter().map(|k| FundArg { outpoint: k.utxo.outpoint(), amount: k.utxo.amount, owner: k.pubkey }).collect()
    } else {
        a.fund.clone()
    };
    let clock = if price_sale { Some(crate::trade::data::clock(node)?) } else { None };
    let (spec, sale, mut plan, ask, used) = plan_creation(a, &pool, &spare, clock)?;
    let signer = match (sk, key_pk) {
        (Some(sk), Some(pk)) => {
            if pk != spec.funding[0].owner_pubkey {
                return Err("the signing key does not own the funding UTXO".into());
            }
            plan.sign(&sk).map_err(|e| e.to_string())?;
            Some(crate::common::Signer { secret: sk.secret_bytes(), pubkey: pk })
        }
        _ => None,
    };
    // The limit sell is signed and run in the script engine now, before the one confirmation.
    let ask_tx = match &ask {
        Some((_, p)) => {
            let b = crate::token_sale::ask_built(p)?;
            if let Some(s) = &signer {
                crate::common::sign_validate_send(b, s, None, "", Value::Null, true, node, &a.network)?;
            }
            Some(b)
        }
        None => None,
    };
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
    if auto {
        let kas: u64 = pool[..used].iter().map(|f| f.amount).sum();
        println!("funding      {used} UTXO(s) of the key on the node, {kas} sompi (picked automatically, largest first)");
    }
    print_sale(&plan, sale.as_ref(), ask.as_ref(), ask_tx);
    println!(
        "verified     KCC-23 binding, covenant group, exact scripts, supply, mass, owner scheme 0x04 self-test{}",
        if report.scripts_executed {
            ", every input executed in the script engine"
        } else {
            " (UNSIGNED: script execution skipped, pass --key-file or --key-env)"
        }
    );
    for w in plan.warnings() {
        println!("warning      {w}");
    }
    if g.spec.program == IssueProgram::Ref3x3 {
        println!("note         --program 3x3: a transfer of this token costs less than the default public-mint build, but other indexers (e.g. kascov.io) do not recognise it as a KCC-20 token");
    }
    // The fee ceiling, as for every sending command: each transaction against 4 times the node's
    // estimate on its mass. A dry run stays offline and reads the relay floor.
    let ceiling = match (a.dry_run, a.node.as_deref()) {
        (false, Some(node)) => crate::feecap::Ceiling::read(node, a.allow_high_fee),
        _ => crate::feecap::Ceiling::offline(a.allow_high_fee),
    };
    // The compute mass is what the relay fee is charged on (the transient mass is at most about twice it).
    let mass_of = |c: u64, _storage: u64| c;
    for (what, fee, mass) in [
        ("preparation", plan.prep.fee, mass_of(plan.prep.compute_mass, plan.prep.storage_mass)),
        ("genesis", g.fee, mass_of(report.compute_mass, report.storage_mass)),
    ] {
        if let Some(m) = ceiling.violation(fee, mass) {
            return Err(format!("the {what}: {m} [{}]", crate::feecap::CODE));
        }
    }
    if let Some(m) = ask_tx.and_then(|b| ceiling.violation_of(b)) {
        return Err(format!("the sell order: {m} [{}]", crate::feecap::CODE));
    }
    // A submitting run asks first, by the rule of every submitting command: `--yes`, or a
    // terminal, else nothing is written or sent. It then always keeps the signed transactions,
    // because `kob token resume` sends them.
    if !a.dry_run {
        crate::trade::opts::confirm_with(
            a.yes,
            &a.network,
            &match ask_tx {
                Some(b) => format!(
                    "the creation of {} (the preparation, the genesis {}, then the sell order {})",
                    g.spec.ticker,
                    g.txid(),
                    hex(&b.tx.id)
                ),
                None => format!("the creation of {} (the preparation, then the genesis {})", g.spec.ticker, g.txid()),
            },
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
    // Record the genesis outputs that the funding key holds (scheme 0), so that `kob send`,
    // `kob place` and `kob merge` can spend them before the indexer has the genesis. The indexer
    // lists them to their owner from the genesis payload (`GET /v1/token-utxos`), so this record
    // only bridges the lag of the indexer.
    let funder = spec.funding[0].owner_pubkey;
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
    // The declared decimals, symbol and name, so that `kob send` names the token with no indexer.
    crate::recent::record_issued(
        &a.network,
        &token,
        &crate::recent::IssuedFacts { decimals: u32::from(g.spec.decimals), symbol: g.spec.ticker.clone(), name: g.spec.name.clone() },
    );
    if let (Some(b), Some(s)) = (ask_tx, &signer) {
        submit_sale(b, s, &dir, node, &a.network, &hex(g.covenant_id.as_bytes().as_slice()))?;
    }
    Ok(())
}

/// The plan lines of the sale.
fn print_sale(
    plan: &CreationPlan,
    sale: Option<&crate::token_sale::Sale>,
    ask: Option<&(TokenMarket, Plan)>,
    ask_tx: Option<&BuiltTx>,
) {
    let Some(s) = sale else { return };
    let Ok(m) = crate::token_sale::market_of(plan) else { return };
    let kept = crate::trade::units::tokens(s.keep as i128, &m.unit());
    match (&s.curve, ask) {
        (Some((t, st)), _) => {
            println!("after        sell on a curve (output {} is its custody), {kept} kept in the wallet", s.at);
            let carrier = plan.genesis.curve.as_ref().and_then(|c| plan.genesis.tx.outputs.get(c.output)).map_or(0, |o| o.value);
            for l in crate::trade::curve::lines(t, st, &m, carrier as i64) {
                println!("             {l}");
            }
            if let Some(c) = &plan.genesis.curve {
                println!("             curve covenant id {}", hex(c.covenant_id.as_bytes().as_slice()));
            }
        }
        (None, Some((m, p))) => {
            println!("after        sell at one price (output {}), {kept} kept in the wallet", s.at);
            for l in crate::trade::place::plan_lines(p, m, None) {
                println!("             {l}");
            }
            for i in &p.issues {
                println!(
                    "             {}: {} [{}]",
                    if i.severity == Severity::Warning { "warning" } else { "note" },
                    i.message,
                    i.code
                );
            }
            if let Some(b) = ask_tx {
                println!("sell order   {}  (sent right after the genesis, fee {} sompi)", hex(&b.tx.id), b.fee.fee);
            }
        }
        _ => {}
    }
}

/// Sends the limit sell of a one-price sale right after the genesis, retried while the node has
/// not yet seen the genesis. It is kept in `dir` as `sell_order.json`.
fn submit_sale(b: &BuiltTx, s: &crate::common::Signer, dir: &Path, node: &str, network: &str, cov: &str) -> Result<(), String> {
    let mut last = String::new();
    for attempt in 1..=GENESIS_ATTEMPTS {
        match crate::common::sign_validate_send(b, s, Some(dir), "sell_order.json", json!({"token": cov}), false, node, network) {
            Ok(sent) => {
                println!("submitted    sell order {}", sent.txid);
                for c in &b.covenants {
                    println!("order covenant id {}", hex(&c.covenant_id));
                }
                return Ok(());
            }
            Err(e) => {
                last = e;
                if attempt < GENESIS_ATTEMPTS {
                    std::thread::sleep(GENESIS_RETRY);
                }
            }
        }
    }
    Err(format!(
        "the token was created, but its sell order was not accepted ({last}): it is saved in {}; or place it with `kob place \
         limit --token {cov} --side sell ...`",
        dir.join("sell_order.json").display()
    ))
}

/// How often the genesis is offered to the node, and the pause between attempts.
///
/// The node normally takes it at once, because its input is the preparation that the node just
/// accepted. A node that has not yet seen the preparation gets a few seconds.
const GENESIS_ATTEMPTS: u32 = 6;
const GENESIS_RETRY: Duration = Duration::from_secs(2);

/// Send the preparation, then the genesis, which is retried a few times.
///
/// With `resuming`, a node error on the preparation is reported and the genesis is still tried,
/// because the preparation may be on chain already. On failure the error says how to resume from
/// `dir`.
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
  kob token resume <DIR> [--node URL] [--network NET]

DIR is the directory `kob token create` wrote (creation.json, and preparation_tx.json and genesis_tx.json, both signed). The preparation is sent
again (a node that already has it refuses it: that is fine) and then the genesis. Nothing is signed: the saved transactions
are sent as they are. If the preparation output was spent elsewhere meanwhile, the genesis cannot be sent; that KAS was
the funding key's own.
";

/// Run `kob token resume`; returns the process exit code.
pub fn run_resume(args: &[String]) -> i32 {
    if let Some(code) = crate::help::help_or_usage(args, &[RESUME_USAGE], true) {
        return code;
    }
    // A mistake in the arguments is a usage error (2). Anything the run meets after that is 1.
    let parse = || -> Result<(PathBuf, String, String), String> {
        let mut dir = None;
        let mut node = None;
        let mut network = String::new();
        let mut it = args.iter();
        while let Some(a) = it.next() {
            match a.as_str() {
                "--node" => node = Some(it.next().cloned().ok_or("--node needs a value")?),
                "--network" => network = it.next().cloned().ok_or("--network needs a value")?,
                other if dir.is_none() && !other.starts_with("--") => dir = Some(PathBuf::from(other)),
                other => return Err(format!("unknown argument `{other}` (see `kob token resume --help`)")),
            }
        }
        let dir = dir.ok_or("give the directory `kob token create` wrote")?;
        let network = crate::common::resolve_network(&network)?;
        let node = wrpc::node_for(node, &network);
        Ok((dir, network, node))
    };
    let (dir, network, node) = match parse() {
        Ok(x) => x,
        Err(e) => {
            eprintln!("error: {e}");
            return crate::help::USAGE_EXIT;
        }
    };
    let run = || -> Result<(), String> {
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
            return Err("the saved transactions are not signed (a dry run without a key): issue again".into());
        }
        let g = &genesis["inputs"][0]["previousOutpoint"];
        if genesis["inputs"].as_array().map(Vec::len) != Some(1) || g["transactionId"].as_str() != Some(prep_id) || g["index"] != 0 {
            return Err("genesis_tx.json does not spend output 0 of preparation_tx.json".into());
        }
        // A signature commits to no network, so the node must run the named one.
        wrpc::check_node_network(&node, &network, Duration::from_secs(30))?;
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
come from anyone: an indexer (GET /v1/tokens/<id>/declaration), a community list, `kob token create`. Both transaction ids and
the covenant id are recomputed; the declaration is printed only when it is bound to that covenant id.
";

/// Run `kob token verify-declaration`; returns the process exit code.
pub fn run_verify_declaration(args: &[String]) -> i32 {
    if let Some(code) = crate::help::help_or_usage(args, &[VERIFY_USAGE], true) {
        return code;
    }
    let [path] = args else {
        return crate::help::print_usage_error(&[VERIFY_USAGE]);
    };
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

    #[test]
    fn a_malformed_key_is_never_echoed() {
        let good = "11".repeat(32);
        let bad = format!("KOB_KEY={good}");
        let dir = std::env::temp_dir().join(format!("kob-readkey-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("k");
        std::fs::write(
            &p,
            format!(
                "{bad}
"
            ),
        )
        .unwrap();
        std::env::set_var("KOB_TEST_BAD_KEY_ENV", &bad);
        for src in [KeySource::Env("KOB_TEST_BAD_KEY_ENV".into()), KeySource::File(p.clone())] {
            let e = read_key(&src).unwrap_err();
            assert_eq!(e, "signing key is not 64 hex characters");
            assert!(!e.contains(&good[..8]));
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    fn v(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_string).collect()
    }

    /// A file holding the key `hex`, for `--key-file`.
    fn key_file(tag: &str, hex: &str) -> String {
        let dir = std::env::temp_dir().join(format!("kob-token-keys-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(tag);
        std::fs::write(
            &p,
            format!(
                "{hex}
"
            ),
        )
        .unwrap();
        p.display().to_string()
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
    fn the_key_rule_of_token_create() {
        let run = format!("--name X --ticker X --decimals 1 --fund-utxo {TX}:0:1:{PK} --supply 1 --network testnet-10");
        let src =
            |extra: &str, fallback: Option<KeySource>| parse_issue_args_with(&v(&format!("{run} {extra}")), fallback).map(|a| a.key);
        assert_eq!(src("--key-file k.txt", None).unwrap(), Some(KeySource::File("k.txt".into())));
        assert_eq!(src("--key-env MY_KEY", None).unwrap(), Some(KeySource::Env("MY_KEY".into())));
        // `--key` was removed in every form: a usage error that names the new options and never the key.
        for old in ["--key file:k.txt", "--key env:MY_KEY", "--key 07", "--key=07"] {
            let e = src(old, None).unwrap_err();
            assert!(e.contains("--key was removed") && e.contains("--key-file") && e.contains("--key-env"), "{old}: {e}");
            assert!(!e.contains("07"), "{e}");
        }
        // One key only.
        assert!(src("--key-file a --key-env B", None).unwrap_err().contains("once"));
        // Without a key: the fallback ($KOB_KEY) when the run signs, nothing in a dry run.
        let kob_key = Some(KeySource::Env("KOB_KEY".into()));
        assert_eq!(src("", kob_key.clone()).unwrap(), kob_key);
        assert_eq!(src("--dry-run", kob_key).unwrap(), None);
    }

    #[test]
    fn requires_the_essentials() {
        assert!(parse_issue_args(&v("--name X --ticker X --decimals 1")).is_err());
        assert!(
            parse_issue_args_with(&v(&format!("--name X --ticker X --decimals 1 --fund-utxo {TX}:0:1:{PK} --supply 1")), None)
                .is_err(),
            "needs a key"
        );
        assert!(
            parse_issue_args_with(
                &v(&format!("--name X --ticker X --decimals 1 --fund-utxo {TX}:0:1:{PK} --supply 1 --node ws://x:1")),
                None
            )
            .is_err(),
            "needs a key"
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
            "--name Test --ticker TEST --decimals 8 --fund-utxo {TX}:0:5000000000:{} --supply 1000 --holder self:0:900 --holder {PK}:4:100 --dry-run --network testnet-10 --key-file {} --out-dir {}",
            hex(&pk),
            key_file("k3", sk),
            dir.display()
        );
        assert_eq!(run_issue(&v(&cmd)), 0);
        let reg: Value = serde_json::from_str(&std::fs::read_to_string(dir.join("registry_entry.json")).unwrap()).unwrap();
        assert_eq!(reg["ticker"], "TEST");
        assert_eq!(reg["template_id"], "kcc20-ref-public-mint", "the default issuance is the published public-mint build");
        // The emitted entry is a valid registry token against the shipped templates.
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
        // The genesis names the states of its two outputs (KOBG), so an indexer finds them for the
        // issuer's key.
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
        // The launch proof verifies from the file, as anyone would check it.
        assert_eq!(run_verify_declaration(&[dir.join("launch_proof.json").display().to_string()]), 0);
        let mut proof: Value = serde_json::from_str(&std::fs::read_to_string(dir.join("launch_proof.json")).unwrap()).unwrap();
        proof["covenantId"] = Value::String("11".repeat(32));
        let forged = dir.join("forged_proof.json");
        std::fs::write(&forged, proof.to_string()).unwrap();
        assert_eq!(run_verify_declaration(&[forged.display().to_string()]), 1);
        let req: Value = serde_json::from_str(&std::fs::read_to_string(dir.join("submit_request.json")).unwrap()).unwrap();
        assert_eq!(req["method"], "submitTransaction");
        // An unsigned dry run needs no key.
        let cmd2 = format!(
            "--name Test --ticker TEST --decimals 8 --fund-utxo {TX}:0:5000000000:{} --supply 5 --dry-run --network testnet-10",
            hex(&pk)
        );
        assert_eq!(run_issue(&v(&cmd2)), 0);
        // The standalone 3/3 program is issued only when it is named.
        let dir3 = std::env::temp_dir().join(format!("kob-issue-test-3x3-{}", std::process::id()));
        assert_eq!(run_issue(&v(&format!("{cmd2} --program 3x3 --out-dir {}", dir3.display()))), 0);
        let reg3: Value = serde_json::from_str(&std::fs::read_to_string(dir3.join("registry_entry.json")).unwrap()).unwrap();
        assert_eq!(reg3["template_id"], "kcc20-ref-3x3");
        let _ = std::fs::remove_dir_all(dir3);
        // A wrong key is refused.
        let cmd3 = format!("{cmd2} --key-file {}", key_file("k5", &"55".repeat(32)));
        assert_eq!(run_issue(&v(&cmd3)), 1);
        // A supply mismatch is refused.
        let cmd4 = format!("{cmd2} --holder self:0:4");
        assert_eq!(run_issue(&v(&cmd4)), 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn x_and_telegram_take_handles() {
        let a = parse_issue_args(&v(&format!(
            "{} --supply 1 --x @kaspa --telegram https://telegram.me/kaspa_chat --image ipfs://cid",
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
            .ends_with(r#""image":"ipfs://cid","x":"https://x.com/kaspa","telegram":"https://t.me/kaspa_chat"}"#));
        // `--logo` and `--icon` were removed: a usage error that names `--image`.
        for old in ["--logo", "--icon"] {
            let e = parse_issue_args(&v(&format!("{} --supply 1 {old} https://e.org/a.png", base()))).unwrap_err();
            assert!(e.contains(&format!("{old} was removed")) && e.contains("--image"), "{e}");
        }
        // The prototype program is not issued any more; the others are.
        for old in ["8x8-prototype", "8x8", "kcc20-ref-8x8"] {
            let e = parse_issue_args(&v(&format!("{} --supply 1 --program {old}", base()))).unwrap_err();
            assert!(e.contains("no longer issued"), "{old}: {e}");
        }
        for ok in ["public-mint", "3x3"] {
            assert!(parse_issue_args(&v(&format!("{} --supply 1 --program {ok}", base()))).is_ok(), "{ok}");
        }
    }

    #[test]
    fn image_file_and_hash_are_declared_right_after_the_image() {
        let dir = std::env::temp_dir().join(format!("kob-image-file-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let png = dir.join("a.png");
        std::fs::write(&png, [&[0x89u8, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a][..], b"pixels"].concat()).unwrap();
        let svg = dir.join("a.svg");
        std::fs::write(&svg, "<svg xmlns='http://www.w3.org/2000/svg'/>").unwrap();
        let want = kcc23::image_file_hash(&std::fs::read(&png).unwrap()).unwrap();
        let args = |extra: &str| parse_issue_args(&v(&format!("{} --supply 1 --image https://e.org/a.png {extra}", base())));
        let a = args(&format!("--image-file {}", png.display())).unwrap();
        assert_eq!(a.image_hash.as_deref(), Some(want.as_str()));
        let json = to_spec(&a).unwrap().token_metadata().json();
        assert!(json.contains(&format!(r#""image":"https://e.org/a.png","image_hash":"{want}""#)), "{json}");
        // The hash given directly (any case, lowercased), alone or agreeing with the file.
        let a = args(&format!("--image-hash {}", want.to_uppercase())).unwrap();
        assert_eq!(a.image_hash.as_deref(), Some(want.as_str()));
        assert!(args(&format!("--image-file {} --image-hash {want}", png.display())).is_ok());
        // Refused: a disagreeing pair, a malformed hash, an SVG, a missing file, and a hash
        // without an address.
        assert!(args(&format!("--image-file {} --image-hash {}", png.display(), "ab".repeat(32))).is_err());
        assert!(args("--image-hash abcd").is_err());
        assert!(args(&format!("--image-file {}", svg.display())).is_err());
        assert!(args(&format!("--image-file {}", dir.join("none.png").display())).is_err());
        assert!(parse_issue_args(&v(&format!("{} --supply 1 --image-hash {want}", base()))).is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// `--yes` and `--dry-run` exclude each other, as in every submitting command.
    #[test]
    fn yes_and_dry_run_exclude_each_other() {
        let base =
            format!("--name Test --ticker TEST --decimals 8 --fund-utxo {TX}:1:5000000000:{PK} --supply 1 --network testnet-10");
        assert!(parse_issue_args(&v(&format!("{base} --dry-run --yes"))).unwrap_err().contains("exclude each other"));
        assert!(parse_issue_args(&v(&format!("{base} --dry-run -y"))).is_err());
        let a = parse_issue_args(&v(&format!("{base} --node ws://x:1 --key-file {} --yes", key_file("kpk", PK)))).unwrap();
        assert!(a.yes && !a.dry_run);
    }

    /// The key 33..33, a file holding it, and its x-only public key.
    fn key33(tag: &str) -> (String, [u8; 32]) {
        let sk = "3333333333333333333333333333333333333333333333333333333333333333";
        let pk = Keypair::from_secret_key(&Secp256k1::new(), &SecretKey::from_slice(&hex_array::<32>(sk).unwrap()).unwrap())
            .x_only_public_key()
            .0
            .serialize();
        (key_file(tag, sk), pk)
    }

    #[test]
    fn sale_flags_parse_and_refuse_what_does_not_apply() {
        let a = parse_issue_args(&v(&format!("{} --supply 100 --sell curve --keep 12.5 --levels 10", base()))).unwrap();
        assert_eq!(
            (a.sale.mode.as_deref(), a.sale.keep.as_deref(), a.sale.levels.as_deref()),
            (Some("curve"), Some("12.5"), Some("10"))
        );
        assert!(parse_issue_args(&v(&format!("{} --supply 100 --keep 10", base()))).unwrap_err().contains("goes with --sell"));
        assert!(parse_issue_args(&v(&format!("{} --supply 100 --sell price --multiple 2", base()))).is_err());
        assert!(parse_issue_args(&v(&format!("{} --supply 100 --sell auction", base()))).is_err());
        // Without --fund-utxo a key is needed, to find its UTXOs on the node.
        let no_fund = "--name X --ticker XX --decimals 0 --supply 1 --dry-run --network testnet-10";
        assert!(parse_issue_args_with(&v(no_fund), None).unwrap_err().contains("--fund-utxo"));
        let (kf, _) = key33("kauto");
        assert!(parse_issue_args_with(&v(&format!("{no_fund} --key-file {kf}")), None).unwrap().fund.is_empty());
    }

    /// `--sell curve`: the genesis holds the curve, its custody and the kept share; a dry run is
    /// offline.
    #[test]
    fn a_curve_sale_is_created_in_the_genesis() {
        let (kf, pk) = key33("kcurve");
        let dir = std::env::temp_dir().join(format!("kob-issue-curve-{}", std::process::id()));
        let cmd = format!(
            "--name Curve --ticker CRV --decimals 2 --fund-utxo {TX}:0:10000000000:{} --supply 100000 --sell curve --keep 10 \
             --levels 20 --dry-run --network testnet-10 --key-file {kf} --out-dir {}",
            hex(&pk),
            dir.display()
        );
        assert_eq!(run_issue(&v(&cmd)), 0);
        let tx: Value = serde_json::from_str(&std::fs::read_to_string(dir.join("genesis_tx.json")).unwrap()).unwrap();
        // The custody (90 %), the kept share (10 %) and the curve.
        assert_eq!(tx["outputs"].as_array().unwrap().len(), 3);
        let sup: Value = serde_json::from_str(&std::fs::read_to_string(dir.join("supply.json")).unwrap()).unwrap();
        assert_eq!(sup["total_supply"], "100000");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Without --fund-utxo the key's UTXOs on the node fund the creation, the fewest, largest
    /// first; `--sell price` sends a limit sell right after the genesis, spending the genesis output
    /// for sale; one --yes covers the three.
    #[test]
    fn automatic_funding_and_a_one_price_sale_send_three_transactions() {
        let (kf, pk) = key33("kprice");
        let spk = kob_protocol::issue::p2pk_script(&pk);
        let utxo = |tag: u8, amount: u64| crate::wrpc::NodeUtxo {
            address: Some(crate::common::address_of(&spk, "testnet-10").unwrap()),
            txid: [tag; 32],
            index: 0,
            amount,
            spk: crate::common::spk_bytes(&spk),
            daa: 990_000,
            coinbase: false,
            covenant_id: None,
        };
        let (url, state) = crate::wrpc::mock::start(vec![utxo(1, 300_000_000), utxo(2, 5_000_000_000), utxo(3, 2_000_000_000)]);
        let dir = std::env::temp_dir().join(format!("kob-create-sale-{}", std::process::id()));
        let cmd = format!(
            "--name Sale --ticker SALE --decimals 0 --supply 1000 --sell price --price 2KAS --keep 20 --network testnet-10 \
             --key-file {kf} --node {url} --out-dir {} --yes",
            dir.display()
        );
        assert_eq!(run_issue(&v(&cmd)), 0);
        let sent = state.lock().unwrap().submitted.clone();
        assert_eq!(sent.len(), 3, "preparation, genesis, sell order");
        // The largest UTXO alone funds the creation.
        assert_eq!(sent[0]["inputs"].as_array().unwrap().len(), 1);
        assert_eq!(sent[0]["inputs"][0]["previousOutpoint"]["transactionId"], hex(&[2; 32]));
        let ids: Value = serde_json::from_str(&std::fs::read_to_string(dir.join("creation.json")).unwrap()).unwrap();
        let spends = |tx: &Value, id: &Value, i: u64| {
            tx["inputs"]
                .as_array()
                .unwrap()
                .iter()
                .any(|x| &x["previousOutpoint"]["transactionId"] == id && x["previousOutpoint"]["index"] == i)
        };
        assert!(spends(&sent[2], &ids["genesis_txid"], 0), "the sell spends the genesis output for sale");
        assert!(spends(&sent[2], &ids["preparation_txid"], 1), "and the preparation's change");
        assert!(dir.join("sell_order.json").exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A submitting run sends the preparation, then the genesis. `resume` sends the pair again.
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
            "--name Test --ticker TEST --decimals 8 --fund-utxo {TX}:0:5000000000:{} --supply 1000 --network testnet-10 --key-file {} --node {url} --out-dir {}",
            hex(&pk),
            key_file("k3b", sk),
            dir.display()
        );
        // Without `--yes` and without a terminal nothing is written and nothing is sent.
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
        // The genesis names the state of its one token output: KOBG, 1, 1, 112, the state span.
        assert!(sent[1]["payload"].as_str().unwrap().starts_with("4b4f4247010170"), "{}", sent[1]["payload"]);
        assert_eq!(run_resume(&v(&format!("{} --node {url} --network testnet-10", dir.display()))), 0);
        let sent = state.lock().unwrap().submitted.clone();
        assert_eq!(sent.len(), 4);
        assert_eq!((&sent[2], &sent[3]), (&sent[0], &sent[1]), "resume sends the saved, signed pair unchanged");
        // A dry run without a key writes unsigned transactions, and resume refuses them.
        let dry = std::env::temp_dir().join(format!("kob-create-dry-{}", std::process::id()));
        let cmd = format!(
            "--name Test --ticker TEST --decimals 8 --fund-utxo {TX}:0:5000000000:{} --supply 5 --dry-run --network testnet-10 --out-dir {}",
            hex(&pk),
            dry.display()
        );
        assert_eq!(run_issue(&v(&cmd)), 0);
        assert_eq!(run_resume(&v(&format!("{} --node {url} --network testnet-10", dry.display()))), 1);
        let _ = std::fs::remove_dir_all(dir);
        let _ = std::fs::remove_dir_all(dry);
    }
}
