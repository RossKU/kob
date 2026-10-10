//! `kob registry validate`: parse and validate a token registry file (`registry/tokens.json` format).
//! `kob registry verify-genesis`: re-derive every token's genesis check (C1) and live-mint-authority check (C2) from chain
//! evidence (`registry/evidence/<network>-genesis.json`) and compare with the registry.

use kob_protocol::genesis_evidence::{registry_mismatches, verify_evidence, Evidence};
use kob_protocol::registry::{Registry, RegistryError};

const USAGE: &str = "usage: kob registry validate <tokens.json> [--network mainnet|testnet-10|devnet]";

/// Returns the process exit code: 0 valid, 1 invalid, 2 usage or I/O error.
pub fn run_validate(args: &[&str]) -> i32 {
    let mut path = None;
    let mut network = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match *a {
            "--network" => match it.next() {
                Some(n) => network = Some(n.to_string()),
                None => {
                    eprintln!("{USAGE}");
                    return 2;
                }
            },
            p if path.is_none() && !p.starts_with("--") => path = Some(p.to_string()),
            _ => {
                eprintln!("{USAGE}");
                return 2;
            }
        }
    }
    let Some(path) = path else {
        eprintln!("{USAGE}");
        return 2;
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("cannot read {path}: {e}");
            return 2;
        }
    };
    let reg = match Registry::parse_unvalidated(&text) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("{path}: {e}");
            return 1;
        }
    };
    let res = match &network {
        Some(n) => reg.validate_for_network(n),
        None => reg.validate(),
    };
    match res {
        Ok(()) => {
            println!("{path}: valid ({} network, {} templates, {} tokens)", reg.network, reg.templates.len(), reg.tokens.len());
            0
        }
        Err(errs) => {
            eprintln!("{}", RegistryError::Invalid(errs));
            1
        }
    }
}

const VERIFY_USAGE: &str = "\
usage: kob registry verify-genesis --network mainnet|testnet-10|devnet [--evidence FILE] [--registry FILE] [--json]

Re-checks every token of the evidence file offline: the genesis txid is recomputed from the recorded transaction, the
covenant id from the authorising outpoint and the genesis group, every redeem script against its P2SH hash and the token's
pinned program (C1); a KRON genesis without a minter output proves no live minter can exist (C2). Then compares the result
with the registry's genesis_verified / genesis records. Exit 0: the registry says exactly what the evidence proves.
Defaults: --evidence registry/evidence/<network>-genesis.json, --registry registry/tokens.json. --json prints the
genesis records. Collect fresh evidence (read-only node + explorer):
  node web/scripts/registry-genesis-evidence.mjs";

/// `kob registry verify-genesis`. Returns the exit code: 0 the registry matches the evidence, 1 it does not, 2 usage or I/O.
pub fn run_verify_genesis(args: &[&str]) -> i32 {
    let (mut network, mut evidence, mut registry, mut json) = (None, None, None, false);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let slot = match *a {
            "--network" => &mut network,
            "--evidence" => &mut evidence,
            "--registry" => &mut registry,
            "--json" => {
                json = true;
                continue;
            }
            "--help" | "-h" => {
                println!("{VERIFY_USAGE}");
                return 0;
            }
            _ => {
                eprintln!("{VERIFY_USAGE}");
                return 2;
            }
        };
        match it.next() {
            Some(v) => *slot = Some(v.to_string()),
            None => {
                eprintln!("{VERIFY_USAGE}");
                return 2;
            }
        }
    }
    let Some(network) = network else {
        eprintln!("{VERIFY_USAGE}");
        return 2;
    };
    let evidence = evidence.unwrap_or_else(|| format!("registry/evidence/{network}-genesis.json"));
    let registry = registry.unwrap_or_else(|| "registry/tokens.json".into());
    let read = |p: &str| std::fs::read_to_string(p).map_err(|e| eprintln!("cannot read {p}: {e}"));
    let (Ok(rtext), Ok(etext)) = (read(&registry), read(&evidence)) else {
        return 2;
    };
    let reg = match Registry::parse(&rtext) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("{registry}: {e}");
            return 1;
        }
    };
    if let Err(e) = reg.validate_for_network(&network) {
        eprintln!("{registry}: {}", RegistryError::Invalid(e));
        return 1;
    }
    let ev: Evidence = match serde_json::from_str(&etext) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("{evidence}: {e}");
            return 1;
        }
    };
    let verdicts = match verify_evidence(&reg, &ev) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{evidence}: {e}");
            return 1;
        }
    };
    println!(
        "evidence {evidence}: {} token(s), node {} (v{}, virtual DAA {}), explorer {}",
        ev.tokens.len(),
        ev.sources.node.url,
        ev.sources.node.server_version,
        ev.sources.node.virtual_daa_score,
        ev.sources.explorer
    );
    for v in &verdicts {
        match &v.result {
            Ok(g) => println!(
                "  {:<8} {}  C1 genesis {} (DAA {}) outputs {:?} supply {}{}: VERIFIED;  C2 live minter: {}",
                v.ticker,
                v.covenant_id,
                g.txid,
                g.daa_score,
                g.outputs,
                g.supply,
                match g.mint_allowance {
                    Some(a) => format!(" + mint allowance {a} (max supply {})", g.supply.saturating_add(a)),
                    None => String::new(),
                },
                match &g.live_minters {
                    Some(l) if l.is_empty() => format!("none (genesis minter outputs {:?})", g.minter_outputs),
                    Some(l) => format!("YES {l:?}"),
                    None => format!("undetermined (genesis minter outputs {:?}, lineage not traced)", g.minter_outputs),
                }
            ),
            Err(e) => println!("  {:<8} {}  C1 NOT VERIFIED: {e}", v.ticker, v.covenant_id),
        }
    }
    if json {
        let records: Vec<serde_json::Value> = verdicts
            .iter()
            .map(|v| match &v.result {
                Ok(g) => {
                    serde_json::json!({ "ticker": v.ticker, "covenant_id": v.covenant_id, "genesis_verified": true, "genesis": g })
                }
                Err(e) => {
                    serde_json::json!({ "ticker": v.ticker, "covenant_id": v.covenant_id, "genesis_verified": false, "error": e })
                }
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&records).expect("json"));
    }
    let mism = registry_mismatches(&reg, &verdicts);
    if mism.is_empty() {
        println!("{registry}: matches the evidence");
        0
    } else {
        for m in &mism {
            eprintln!("  MISMATCH {m}");
        }
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo_file(rel: &str) -> String {
        format!("{}/../../{rel}", env!("CARGO_MANIFEST_DIR"))
    }

    #[test]
    fn shipped_registries_validate_and_bad_input_fails() {
        assert_eq!(run_validate(&[&repo_file("registry/tokens.json")]), 0);
        assert_eq!(run_validate(&[&repo_file("registry/tokens.json"), "--network", "mainnet"]), 0);
        assert_eq!(run_validate(&[&repo_file("registry/tokens.json"), "--network", "testnet-10"]), 1);
        assert_eq!(run_validate(&[&repo_file("registry/tokens.example.json")]), 0);
        assert_eq!(run_validate(&[&repo_file("registry/does-not-exist.json")]), 2);
        assert_eq!(run_validate(&[]), 2);
        let bad = std::env::temp_dir().join(format!("kob-bad-registry-{}.json", std::process::id()));
        std::fs::write(&bad, "{\"schema_version\": 1}").unwrap();
        assert_eq!(run_validate(&[bad.to_str().unwrap()]), 1);
        let _ = std::fs::remove_file(bad);
    }

    #[test]
    fn verify_genesis_on_the_committed_mainnet_evidence() {
        let reg = repo_file("registry/tokens.json");
        let ev = repo_file("registry/evidence/mainnet-genesis.json");
        assert_eq!(run_verify_genesis(&["--network", "mainnet", "--registry", &reg, "--evidence", &ev]), 0);
        assert_eq!(run_verify_genesis(&["--network", "mainnet", "--registry", &reg, "--evidence", &ev, "--json"]), 0);
        // wrong network, missing file, usage
        assert_eq!(run_verify_genesis(&["--network", "testnet-10", "--registry", &reg, "--evidence", &ev]), 1);
        assert_eq!(run_verify_genesis(&["--network", "mainnet", "--registry", &reg, "--evidence", "no-such-file.json"]), 2);
        assert_eq!(run_verify_genesis(&["--registry", &reg]), 2);
        // a registry that claims something the evidence does not show
        let text = std::fs::read_to_string(&reg).unwrap();
        let mut v: serde_json::Value = serde_json::from_str(&text).unwrap();
        v["tokens"][0]["genesis"]["supply"] = serde_json::json!(1);
        let tmp = std::env::temp_dir().join(format!("kob-genesis-registry-{}.json", std::process::id()));
        std::fs::write(&tmp, v.to_string()).unwrap();
        assert_eq!(run_verify_genesis(&["--network", "mainnet", "--registry", tmp.to_str().unwrap(), "--evidence", &ev]), 1);
        let _ = std::fs::remove_file(tmp);
    }
}
