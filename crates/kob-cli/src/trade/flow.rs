//! The common tail of every trading command: show the plan (findings, what the order is, the decoded transaction), stop on
//! `--dry-run`, ask (or take `--yes`), sign with the wallet key, validate in the script engine, submit, report.

use kob_protocol::json::to_hex;
use kob_protocol::tx::BuiltTx;
use serde_json::{json, Value};

use super::opts::{confirm, Common};
use super::plan::{Issue, Severity};
use super::show::{self, Units};
use crate::common::{sign_validate_send, Signer};

/// A planned action ready to show.
pub struct Ready {
    /// What it is (`place limit sell`, `cancel ...`).
    pub title: String,
    /// Plan lines (already formatted).
    pub lines: Vec<String>,
    /// Findings.
    pub issues: Vec<Issue>,
    /// The transaction.
    pub built: BuiltTx,
    /// File name under `--out-dir`.
    pub file: String,
    /// Extra JSON of the plan.
    pub json: Value,
}

fn sev(s: Severity) -> &'static str {
    match s {
        Severity::Error => "error",
        Severity::Warning => "warning",
        Severity::Info => "info",
    }
}

/// Print findings (stderr) and return whether one is an error.
pub fn print_issues(issues: &[Issue], json: bool) -> bool {
    if !json {
        for i in issues {
            eprintln!("{}: {} [{}]", sev(i.severity), i.message, i.code);
        }
    }
    issues.iter().any(|i| i.severity == Severity::Error)
}

/// Issues as JSON.
pub fn issues_json(issues: &[Issue]) -> Value {
    json!(issues.iter().map(|i| json!({"code": i.code, "severity": sev(i.severity), "message": i.message})).collect::<Vec<_>>())
}

/// Show, confirm, sign, submit. Returns the process exit code.
pub fn finish(c: &Common, signer: &Signer, r: Ready, units: &Units, argv_hint: &str) -> i32 {
    // the plan was read from this node: it must run the network the command names (mainnet only when named), before
    // anything is shown as a plan of that network or signed
    if let Err(e) = c.need_node().and_then(|n| crate::wrpc::check_node_network(n, &c.network, crate::common::NODE_TIMEOUT)) {
        eprintln!("error: {e}");
        return 1;
    }
    let decoded = show::decode(&r.built, &signer.pubkey, units);
    let failed = print_issues(&r.issues, c.json);
    let new_ids: Vec<String> = r.built.covenants.iter().map(|x| to_hex(&x.covenant_id)).collect();
    let plan_json = json!({
        "action": r.title,
        "network": c.network,
        "plan": r.json,
        "issues": issues_json(&r.issues),
        "transaction": show::json_of(&decoded, &r.built),
        "new_covenant_ids": new_ids,
    });
    if c.json {
        println!("{}", serde_json::to_string_pretty(&plan_json).unwrap_or_default());
    } else {
        println!("{}", r.title);
        for l in &r.lines {
            println!("  {l}");
        }
        for l in show::lines(&decoded, units, &r.built.tx.id) {
            println!("{l}");
        }
    }
    if failed {
        eprintln!("error: the plan has errors: nothing signed");
        return 1;
    }
    if decoded.foreign_signatures > 0 {
        eprintln!("error: the transaction needs signatures of other keys: nothing signed");
        return 1;
    }
    if c.dry_run {
        eprintln!("dry run: nothing signed or submitted");
        return 0;
    }
    if let Err(e) = confirm(c, argv_hint) {
        eprintln!("{e}");
        return 1;
    }
    let node = match c.need_node() {
        Ok(n) => n.to_string(),
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    match sign_validate_send(&r.built, signer, c.out_dir.as_deref(), &r.file, plan_json.clone(), false, &node, &c.network) {
        Ok(sent) => {
            let out = json!({
                "status": "submitted",
                "txid": sent.txid,
                "fee": sent.fee.to_string(),
                "covenant_ids": new_ids,
                "file": sent.file.map(|p| p.display().to_string()),
                "engine_validated": true,
            });
            if c.json {
                println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
            } else {
                println!("submitted: txid {}", sent.txid);
                for id in out["covenant_ids"].as_array().into_iter().flatten() {
                    println!("order covenant id: {}", id.as_str().unwrap_or(""));
                }
            }
            0
        }
        Err(e) => {
            eprintln!("error: {e}");
            1
        }
    }
}
