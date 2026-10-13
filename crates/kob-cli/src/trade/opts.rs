//! Options every trading command shares, the signing key (from a file or the environment, never from the command line),
//! the network rule (mainnet only when named), and the confirmation step before anything is signed.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, IsTerminal, Write};
use std::path::PathBuf;

use kob_protocol::tx::{pubkey_of, MIN_FEE_RATE};

use crate::common::Signer;

/// The environment variable the signing key is read from by default.
pub const KEY_ENV: &str = "KOB_KEY";

/// Flags every trading command takes with a value.
pub const COMMON_VALUE_FLAGS: &[&str] =
    &["--network", "--node", "--indexer", "--key-file", "--key-env", "--fee-rate", "--out-dir", "--registry", "--key"];
/// Flags every trading command takes without a value.
pub const COMMON_BOOL_FLAGS: &[&str] = &["--yes", "-y", "--dry-run", "--json"];

/// Help text of the shared options.
pub const COMMON_HELP: &str = "\
COMMON OPTIONS
  --network NET     mainnet or testnet-10 (required unless the build is a deployment build; mainnet is refused
                    unless named here explicitly, even by a mainnet build)
  --node URL        node wRPC JSON endpoint (default $KOB_NODE_WRPC; a URL with credentials belongs there)
  --indexer URL     indexer API, http:// (default $KOB_INDEXER_URL)
  --key-file PATH   the signing key: a file holding the 64-hex secret
  --key-env NAME    ... or the environment variable holding it (default KOB_KEY)
                    a key on the command line (--key) is refused: arguments are visible to other users (ps)
  --registry FILE   token registry (default: the one this build embeds, when its network matches)
  --fee-rate N      sompi per gram (default: the relay floor, 100)
  --dry-run         show the plan and stop: nothing is signed
  --yes, -y         sign and submit without asking (otherwise a terminal asks; without a terminal, refused)
  --json            print the plan and the result as JSON
  --out-dir DIR     also write the signed transaction (and its submitRequest) here
";

/// Parsed arguments: positionals, valued options (repeatable) and flags.
#[derive(Debug, Clone, Default)]
pub struct Parsed {
    /// Positional arguments.
    pub pos: Vec<String>,
    /// `--flag value` (all occurrences).
    pub vals: BTreeMap<String, Vec<String>>,
    /// Boolean flags present.
    pub flags: BTreeSet<String>,
}

impl Parsed {
    /// The last value of `--flag`.
    pub fn get(&self, k: &str) -> Option<&str> {
        self.vals.get(k).and_then(|v| v.last()).map(String::as_str)
    }
    /// Whether `--flag` was given.
    pub fn has(&self, k: &str) -> bool {
        self.flags.contains(k)
    }
}

/// Parse `args` against the command's own valued / boolean flags plus the common ones. `--flag=value` is accepted.
pub fn parse(args: &[String], value_flags: &[&str], bool_flags: &[&str]) -> Result<Parsed, String> {
    let mut p = Parsed::default();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let (flag, inline) = match a.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f.to_string(), Some(v.to_string())),
            _ => (a.clone(), None),
        };
        if value_flags.contains(&flag.as_str()) || COMMON_VALUE_FLAGS.contains(&flag.as_str()) {
            let v = match inline {
                Some(v) => v,
                None => it.next().cloned().ok_or_else(|| format!("{flag} needs a value"))?,
            };
            p.vals.entry(flag).or_default().push(v);
        } else if bool_flags.contains(&flag.as_str()) || COMMON_BOOL_FLAGS.contains(&flag.as_str()) {
            if inline.is_some() {
                return Err(format!("{flag} takes no value"));
            }
            p.flags.insert(if flag == "-y" { "--yes".into() } else { flag });
        } else if flag.starts_with("--") {
            return Err(format!("unknown option `{flag}`"));
        } else {
            p.pos.push(a.clone());
        }
    }
    if p.vals.contains_key("--key") {
        return Err(format!(
            "--key is refused here: a key on the command line is visible to other users (ps, shell history); use --key-file PATH or the environment variable {KEY_ENV}"
        ));
    }
    if p.has("--yes") && p.has("--dry-run") {
        return Err("--yes and --dry-run exclude each other".into());
    }
    Ok(p)
}

/// The shared settings of a trading command.
#[derive(Debug, Clone)]
pub struct Common {
    /// Network.
    pub network: String,
    /// Node URL (may be empty when not needed yet).
    pub node: String,
    /// Indexer URL.
    pub indexer: Option<String>,
    /// Fee rate.
    pub fee_rate: Option<u64>,
    /// Dry run.
    pub dry_run: bool,
    /// Sign without asking.
    pub yes: bool,
    /// JSON output.
    pub json: bool,
    /// Where signed transactions go.
    pub out_dir: Option<PathBuf>,
    /// Registry file.
    pub registry: Option<PathBuf>,
    key_file: Option<PathBuf>,
    key_env: String,
}

/// The network rule of the trading commands: [`crate::common::resolve_network`], and mainnet only when named.
pub fn network_of(given: Option<&str>) -> Result<String, String> {
    let g = given.unwrap_or("").trim();
    let n = crate::common::resolve_network(g)?;
    if n.starts_with("mainnet") && g != "mainnet" {
        return Err("this would trade on mainnet: name it explicitly with --network mainnet".into());
    }
    Ok(n)
}

impl Common {
    /// From parsed arguments.
    pub fn from(p: &Parsed) -> Result<Common, String> {
        let network = network_of(p.get("--network"))?;
        let fee_rate = match p.get("--fee-rate") {
            None => None,
            Some(v) => {
                let r: u64 = v.parse().map_err(|_| format!("--fee-rate must be an integer (sompi per gram), got `{v}`"))?;
                if r < MIN_FEE_RATE {
                    return Err(format!("--fee-rate must be at least {MIN_FEE_RATE}"));
                }
                Some(r)
            }
        };
        Ok(Common {
            network,
            node: crate::wrpc::node_arg(p.get("--node").map(str::to_owned)).unwrap_or_default(),
            indexer: super::http::indexer_arg(p.get("--indexer").map(str::to_owned)),
            fee_rate,
            dry_run: p.has("--dry-run"),
            yes: p.has("--yes"),
            json: p.has("--json"),
            out_dir: p.get("--out-dir").map(PathBuf::from),
            registry: p.get("--registry").map(PathBuf::from),
            key_file: p.get("--key-file").map(PathBuf::from),
            key_env: p.get("--key-env").unwrap_or(KEY_ENV).to_string(),
        })
    }

    /// The node, required.
    pub fn need_node(&self) -> Result<&str, String> {
        if self.node.is_empty() {
            return Err(format!("--node (or ${}) is required", crate::wrpc::NODE_ENV));
        }
        Ok(&self.node)
    }

    /// The indexer, required.
    pub fn need_indexer(&self) -> Result<&str, String> {
        self.indexer.as_deref().ok_or_else(|| format!("--indexer (or ${}) is required", super::http::INDEXER_ENV))
    }

    /// The signing key: `--key-file`, else the environment variable. Refused when its hex appears among `argv`.
    pub fn signer(&self, argv: &[String]) -> Result<Signer, String> {
        let raw = match &self.key_file {
            Some(p) => std::fs::read_to_string(p).map_err(|e| format!("cannot read the key file {}: {e}", p.display()))?,
            None => std::env::var(&self.key_env)
                .map_err(|_| format!("no signing key: give --key-file PATH or set the environment variable {}", self.key_env))?,
        };
        let hex = raw.trim().to_ascii_lowercase();
        let bytes = kob_protocol::json::hex32(&hex).map_err(|_| "the signing key is not 64 hex characters".to_string())?;
        let pubkey = pubkey_of(&bytes).map_err(|_| "the signing key is not a valid secret key".to_string())?;
        if argv.iter().any(|a| a.to_ascii_lowercase().contains(&hex)) {
            return Err("the signing key appears on the command line: refused (it is visible to other users); keep it in a file or the environment".into());
        }
        Ok(Signer { secret: bytes, pubkey })
    }
}

/// Ask before signing: `--yes` proceeds, a terminal is asked, anything else is refused.
pub fn confirm(c: &Common, what: &str) -> Result<(), String> {
    if c.yes {
        return Ok(());
    }
    let stdin = std::io::stdin();
    if !stdin.is_terminal() {
        return Err(format!("not signed: pass --yes to sign and submit {what} (or --dry-run to only see the plan)"));
    }
    eprint!("Sign and submit {what} on {}? Type yes to continue: ", c.network);
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    stdin.lock().read_line(&mut line).map_err(|e| e.to_string())?;
    match line.trim().to_ascii_lowercase().as_str() {
        "yes" | "y" => Ok(()),
        _ => Err("not signed: cancelled".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_string).collect()
    }

    #[test]
    fn parsing_and_the_key_rules() {
        let p = parse(&v("limit --side sell --amount=5TUSD --yes --network testnet-10"), &["--side", "--amount"], &[]).unwrap();
        assert_eq!(p.pos, vec!["limit"]);
        assert_eq!((p.get("--side"), p.get("--amount")), (Some("sell"), Some("5TUSD")));
        assert!(p.has("--yes"));
        assert!(parse(&v("--bogus 1"), &[], &[]).unwrap_err().contains("unknown option"));
        assert!(parse(&v("--side"), &["--side"], &[]).unwrap_err().contains("needs a value"));
        let e = parse(&v("--key 3333"), &[], &[]).unwrap_err();
        assert!(e.contains("--key-file") && e.contains("KOB_KEY"), "{e}");
        assert!(parse(&v("--yes --dry-run"), &[], &[]).is_err());
        // the key from a file; refused when its hex is on the command line too
        let dir = std::env::temp_dir().join(format!("kob-cli-key-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("k");
        std::fs::write(&f, format!("{}\n", "33".repeat(32))).unwrap();
        let args = v(&format!("--key-file {} --network testnet-10", f.display()));
        let c = Common::from(&parse(&args, &[], &[]).unwrap()).unwrap();
        assert_eq!(c.signer(&args).unwrap().pubkey, pubkey_of(&[0x33; 32]).unwrap());
        let mut leak = args.clone();
        leak.push("33".repeat(32));
        assert!(c.signer(&leak).unwrap_err().contains("command line"));
        // the environment variable
        let c = Common::from(&parse(&v("--key-env KOB_TEST_KEY_UNSET_X --network testnet-10"), &[], &[]).unwrap()).unwrap();
        assert!(c.signer(&[]).unwrap_err().contains("KOB_TEST_KEY_UNSET_X"));
    }

    #[test]
    fn mainnet_must_be_named() {
        assert_eq!(network_of(Some("testnet-10")).unwrap(), "testnet-10");
        assert_eq!(network_of(Some("mainnet")).unwrap(), "mainnet");
        // only a deployment build implies a network; a mainnet build still needs the name
        if kob_protocol::artifacts::deployment_network() == Some("mainnet") {
            assert!(network_of(None).unwrap_err().contains("--network mainnet"));
        } else if kob_protocol::artifacts::deployment_network().is_none() {
            assert!(network_of(None).is_err());
        }
    }

    #[test]
    fn confirmation_without_a_terminal_is_refused() {
        let c = Common::from(&parse(&v("--network testnet-10"), &[], &[]).unwrap()).unwrap();
        // the test harness's stdin is not a terminal (or `--yes` is absent): refused, never signed
        if !std::io::stdin().is_terminal() {
            assert!(confirm(&c, "this").unwrap_err().contains("--yes"));
        }
        let y = Common::from(&parse(&v("--network testnet-10 --yes"), &[], &[]).unwrap()).unwrap();
        assert!(confirm(&y, "this").is_ok());
    }
}
