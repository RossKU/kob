//! `kob registry programs`: the token-program profiles this build embeds (`crates/kob-protocol/data/programs.json`).
//! `kob registry add-program`: derive the profile of a new token program from its bytes (a `silverc` artifact or a raw
//! redeem script observed on chain), print what the reviewer must still establish, and with `--write` add the program to
//! the tree as `pending-review` (its bytes, its profile, a conformance test stub). See `docs/spec/token-programs.md`.

use std::path::{Path, PathBuf};

use kob_protocol::profile::{
    derive_profile, profiles, registry_template, DeriveOptions, ProgramInput, ProgramProfile, ProgramReview, Slots,
};

const USAGE_ADD: &str = "\
usage: kob registry add-program <ARTIFACT.json | REDEEM.bin | REDEEM.hex | HEX> [OPTIONS]

Derives the token-program profile of a new program from its bytes: layout (detected against the known codec families),
template hash, codec, witness scheme. Prints the profile, the registry strict-list entry (pending-review) and the review
tasks the bytes cannot settle.

OPTIONS
  --name NAME          program name ([A-Za-z0-9_]+; the TemplateId name and budget-table key)
  --registry-id ID     registry template id ([a-z0-9-]+; default: the name in kebab case)
  --slots I/O          token inputs / outputs per transfer (measured; default: a review task)
  --holder N           holder owner value (measured; KRON: 3 address presence, the default, or 0 a token-level
                       signature, served by the *KronD0 order builds, testnet only)
  --network NET        a network the program's tokens exist on (repeatable; default: not restricted). A program not on
                       mainnet is not on the shipped (mainnet) registry
  --summary TEXT       one-line description
  --upstream TEXT      provenance for the registry entry
  --source-path PATH   repo-relative path of the pinned bytes (default: contracts/third-party/<name>/...)
  --write              add it to the tree as pending-review: the bytes under contracts/third-party/<name>/, the profile in
                       crates/kob-protocol/data/programs.json, a conformance test stub in crates/kob-tests/tests/
  --repo DIR           repository root for --write (default: found from the current directory)
  --json               print one JSON document";

fn parse_slots(s: &str) -> Option<Slots> {
    let (i, o) = s.split_once('/')?;
    Some(Slots { inputs: i.parse().ok()?, outputs: o.parse().ok()? })
}

fn read_input(arg: &str) -> Result<(ProgramInput, Vec<u8>), String> {
    let hex_ok = |s: &str| s.len().is_multiple_of(2) && !s.is_empty() && s.bytes().all(|b| b.is_ascii_hexdigit());
    let path = Path::new(arg);
    if path.is_file() {
        let bytes = std::fs::read(path).map_err(|e| format!("cannot read {arg}: {e}"))?;
        let text = String::from_utf8(bytes.clone()).ok();
        if let Some(t) = text.as_deref().map(str::trim) {
            if t.starts_with('{') {
                return Ok((ProgramInput::Artifact(t.to_string()), t.as_bytes().to_vec()));
            }
            if hex_ok(t) {
                let raw = kob_protocol::json::from_hex(&t.to_ascii_lowercase())?;
                return Ok((ProgramInput::Redeem(raw.clone()), raw));
            }
        }
        return Ok((ProgramInput::Redeem(bytes.clone()), bytes));
    }
    if hex_ok(arg) {
        let raw = kob_protocol::json::from_hex(&arg.to_ascii_lowercase())?;
        return Ok((ProgramInput::Redeem(raw.clone()), raw));
    }
    Err(format!("{arg} is neither a file nor a hex redeem script"))
}

/// Repository root: the first ancestor of the current directory holding the profile table.
fn find_repo() -> Option<PathBuf> {
    let mut d = std::env::current_dir().ok()?;
    loop {
        if d.join("crates/kob-protocol/data/programs.json").is_file() {
            return Some(d);
        }
        if !d.pop() {
            return None;
        }
    }
}

/// `kob registry programs [--json]`.
pub fn run_programs(args: &[&str]) -> i32 {
    let json = match args {
        [] => false,
        ["--json"] => true,
        _ => {
            eprintln!("usage: kob registry programs [--json]");
            return 2;
        }
    };
    if json {
        println!("{}", serde_json::to_string_pretty(profiles()).expect("profiles serialise"));
        return 0;
    }
    println!(
        "{:<22} {:<24} {:<6} {:<10} {:>6} {:>5} {:>10} {:<14} review",
        "name", "registry id", "codec", "hash", "cut", "slots", "floor", "witness"
    );
    for p in profiles() {
        let witness = match p.witness.scheme {
            kob_protocol::profile::WitnessScheme::Kcc20Transfer => "kcc20-transfer".to_string(),
            kob_protocol::profile::WitnessScheme::KronColumns if p.witness.trailing_pushes.is_empty() => "kron-columns".into(),
            kob_protocol::profile::WitnessScheme::KronColumns => format!("kron-columns+{}", p.witness.trailing_pushes.len()),
            kob_protocol::profile::WitnessScheme::Kcc1Tagged => format!("kcc1-tagged/{}", p.witness.entries.len()),
        };
        println!(
            "{:<22} {:<24} {:<6} {:<10} {:>6} {:>5} {:>10} {:<14} {}",
            p.name,
            p.registry_id,
            p.codec.as_str(),
            &p.template_hash[..8],
            format!("{}+{}", p.layout.prefix_len, p.layout.state_len),
            format!("{}/{}", p.slots.inputs, p.slots.outputs),
            p.carrier_floor,
            witness,
            serde_json::to_value(p.review.status).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default()
        );
    }
    0
}

/// `kob registry add-program`.
pub fn run_add_program(args: &[&str]) -> i32 {
    let mut input = None;
    let mut opt = DeriveOptions::default();
    let (mut write, mut json, mut repo, mut upstream) = (false, false, None, None);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut val = |what: &str| -> Option<String> {
            let v = it.next().map(|s| s.to_string());
            if v.is_none() {
                eprintln!("{what} needs a value\n\n{USAGE_ADD}");
            }
            v
        };
        match *a {
            "--name" => {
                opt.name = Some(match val("--name") {
                    Some(v) => v,
                    None => return 2,
                })
            }
            "--registry-id" => {
                opt.registry_id = Some(match val("--registry-id") {
                    Some(v) => v,
                    None => return 2,
                })
            }
            "--summary" => {
                opt.summary = Some(match val("--summary") {
                    Some(v) => v,
                    None => return 2,
                })
            }
            "--upstream" => {
                upstream = Some(match val("--upstream") {
                    Some(v) => v,
                    None => return 2,
                })
            }
            "--repo" => {
                repo = Some(PathBuf::from(match val("--repo") {
                    Some(v) => v,
                    None => return 2,
                }))
            }
            "--source-path" => {
                opt.source_path = Some(match val("--source-path") {
                    Some(v) => v,
                    None => return 2,
                })
            }
            "--slots" => match val("--slots").as_deref().and_then(parse_slots) {
                Some(s) => opt.slots = Some(s),
                None => {
                    eprintln!("--slots takes INPUTS/OUTPUTS\n\n{USAGE_ADD}");
                    return 2;
                }
            },
            "--holder" => match val("--holder").as_deref().and_then(|v| v.parse::<u8>().ok()) {
                Some(h) => opt.holder = Some(h),
                None => {
                    eprintln!("--holder takes an owner value (0..=255)\n\n{USAGE_ADD}");
                    return 2;
                }
            },
            "--network" => match val("--network") {
                Some(v) => opt.networks.push(v),
                None => return 2,
            },
            "--write" => write = true,
            "--json" => json = true,
            "--help" | "-h" => {
                println!("{USAGE_ADD}");
                return 0;
            }
            p if input.is_none() && !p.starts_with("--") => input = Some(p.to_string()),
            other => {
                eprintln!("unexpected argument {other}\n\n{USAGE_ADD}");
                return 2;
            }
        }
    }
    let Some(arg) = input else {
        eprintln!("{USAGE_ADD}");
        return 2;
    };
    let (program, bytes) = match read_input(&arg) {
        Ok(x) => x,
        Err(e) => {
            eprintln!("{e}");
            return 2;
        }
    };
    let derived = match derive_profile(&program, &opt) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    // a program no builder serves gets no registry entry: it is described, never listed
    let entry = derived.blocked.is_none().then(|| registry_template(&derived.profile, upstream.as_deref()));
    if json {
        let doc = serde_json::json!({
            "profile": derived.profile,
            "detection": derived.detection,
            "already_embedded": derived.already_embedded,
            "blocked": derived.blocked,
            "review_tasks": derived.review_tasks,
            "registry_template": entry,
        });
        println!("{}", serde_json::to_string_pretty(&doc).expect("json"));
    } else {
        println!("detection: {}", derived.detection);
        if let Some(n) = &derived.already_embedded {
            println!("already embedded as {n}: nothing to add");
        }
        match (&derived.blocked, &entry) {
            (Some(why), _) => {
                println!("\nNOT TRADABLE: {why}");
                println!("\nprofile (a description of the bytes, not a table entry):\n{}", profile_entry_text(&derived.profile));
            }
            (None, Some(entry)) => {
                println!("\nprofile (crates/kob-protocol/data/programs.json):\n{}", profile_entry_text(&derived.profile));
                println!(
                    "\nregistry strict-list entry (registry/tokens.json `templates`, after review):\n{}",
                    serde_json::to_string_pretty(entry).unwrap()
                );
            }
            (None, None) => unreachable!("an unblocked program has a registry entry"),
        }
        println!("\nreview tasks:");
        for t in &derived.review_tasks {
            println!("  - {t}");
        }
    }
    if derived.already_embedded.is_some() {
        return 0;
    }
    if let Some(why) = &derived.blocked {
        if write {
            eprintln!("--write: refused, {why}");
            return 1;
        }
        return 0;
    }
    if !write {
        if !json {
            println!("\n(dry run: pass --write to add the program to the tree as pending-review)");
        }
        return 0;
    }
    let Some(root) = repo.or_else(find_repo) else {
        eprintln!("--write: no repository root found (pass --repo DIR)");
        return 2;
    };
    match write_program(&root, &derived.profile, &bytes) {
        Ok(written) => {
            eprintln!("\nwritten (pending-review, not tradable):");
            for w in written {
                eprintln!("  {w}");
            }
            eprintln!("{}", NEXT_STEPS.replace("{stub}", &derived.profile.name.to_ascii_lowercase()));
            0
        }
        Err(e) => {
            eprintln!("--write: {e}");
            1
        }
    }
}

const NEXT_STEPS: &str = "
next steps (docs/spec/token-programs.md, \"Onboarding a token program\"):
  1. cargo test -p kob-protocol profile                       # the bytes match the profile
  2. measure and fill: owners, slots, carrier_floor (crates/kob-tests/tests/program_{stub}.rs, the ignored review tests)
     and add the registry entry printed above (pending-review) to registry/tokens.json
  3. KOB_REGEN=1 cargo test -p kob-protocol --test budget_table --test keeper_tips   # budgets and keeper tips
  4. run the conformance stub and the order suites on the program, then the reviewer checklist
  5. only after review: review.status reviewed in the profile and the registry entry, scripts/build-deploy.sh mainnet";

/// The profile as `data/programs.json` formats it (two-space indent inside the `programs` array).
fn profile_entry_text(p: &ProgramProfile) -> String {
    let v = serde_json::to_string_pretty(p).expect("profile serialises");
    v.lines().map(|l| format!("    {l}")).collect::<Vec<_>>().join("\n")
}

fn write_program(root: &Path, p: &ProgramProfile, bytes: &[u8]) -> Result<Vec<String>, String> {
    // the table refuses it too (`check_shape`); refused here before anything is written
    p.check_shape().map_err(|e| e.to_string())?;
    let mut written = vec![];
    // 1. the profile, appended to the table without reformatting the existing entries; every check of the table runs before
    // anything is written (a refused program leaves no file behind)
    let table = root.join("crates/kob-protocol/data/programs.json");
    let text = std::fs::read_to_string(&table).map_err(|e| e.to_string())?;
    let mut file = kob_protocol::profile::ProfileFile::parse(&text).map_err(|e| e.to_string())?;
    if file.programs.iter().any(|q| q.name == p.name || q.template_hash == p.template_hash) {
        return Err(format!("{} (or its template hash) is already in data/programs.json", p.name));
    }
    debug_assert_eq!(p.review.status, ProgramReview::PendingReview);
    file.programs.push(p.clone());
    file.check().map_err(|e| e.to_string())?;
    let close = text.rfind("\n  ]").ok_or("data/programs.json: no closing `]` of `programs`")?;
    let new_text = format!("{},\n{}{}", &text[..close], profile_entry_text(p), &text[close..]);
    kob_protocol::profile::ProfileFile::parse(&new_text).map_err(|e| format!("the edited table does not parse: {e}"))?;
    // 2. the bytes
    let src = root.join(&p.source.path);
    if src.exists() {
        if std::fs::read(&src).map_err(|e| e.to_string())? != bytes {
            return Err(format!("{} exists with other content", p.source.path));
        }
    } else {
        std::fs::create_dir_all(src.parent().expect("source path has a directory")).map_err(|e| e.to_string())?;
        std::fs::write(&src, bytes).map_err(|e| e.to_string())?;
        written.push(p.source.path.clone());
    }
    std::fs::write(&table, new_text).map_err(|e| e.to_string())?;
    written.push("crates/kob-protocol/data/programs.json".into());
    // 3. the conformance test stub
    let stub_rel = format!("crates/kob-tests/tests/program_{}.rs", p.name.to_ascii_lowercase());
    let stub = root.join(&stub_rel);
    if !stub.exists() {
        std::fs::write(&stub, test_stub(p)).map_err(|e| e.to_string())?;
        written.push(stub_rel);
    }
    Ok(written)
}

/// The conformance test stub of a new program: the live profile check plus one ignored test per review task, each naming
/// what to measure or prove (docs/spec/token-programs.md, the reviewer checklist).
pub fn test_stub(p: &ProgramProfile) -> String {
    TEST_STUB
        .replace("{name}", &p.name)
        .replace("{variant}", &p.variant)
        .replace("{codec}", p.codec.as_str())
        .replace("{hash}", &p.template_hash)
}

const TEST_STUB: &str = r#"//! Conformance stub of the token program `{name}` (template {hash}), written by `kob registry add-program`.
//!
//! The program is `pending-review`: no token on it is listed and no budget covers it until the tasks below are done
//! (docs/spec/token-programs.md, "Reviewer checklist"). Replace each `#[ignore]` test with the engine test it names; the
//! KRON-codec harness (`common/kron.rs`) and the KCC-20 suites (`kcc20_standard_slots_tests.rs`) show the patterns.

use kob_protocol::artifacts::{token_template, TemplateId};

fn program() -> TemplateId {
    TemplateId::{variant}
}

/// The embedded bytes match the profile (layout, template hash, codec {codec}).
#[test]
fn profile_matches_the_embedded_bytes() {
    let p = program().profile().expect("profile");
    let t = token_template(program());
    assert_eq!(t.hash, p.hash());
    assert_eq!((t.prefix.len(), t.state_len, t.suffix.len()), (p.layout.prefix_len, p.layout.state_len, p.layout.suffix_len));
}

#[test]
#[ignore = "review: a transfer built with the profile's witness scheme is accepted, a tampered one rejected (engine)"]
fn transfer_with_the_profile_witness_scheme() {}

#[test]
#[ignore = "review: custody owner (covenant id) and holder owner of the codec family are accepted and authorised as the order templates assume"]
fn custody_and_holder_owners() {}

#[test]
#[ignore = "review: measure slots (largest accepted token input / output counts) and carrier_floor (smallest token output value)"]
fn slots_and_carrier_floor_are_measured() {}

#[test]
#[ignore = "review: no transaction creates a minter or supply without spending a minter (genesis.minters); every non-transfer entry is a declared capability"]
fn supply_and_authority() {}

#[test]
#[ignore = "review: the KAS-book and pair order suites pass with this program, and their ablation runs still confirm every mutation on it"]
fn order_suites_and_ablation_on_the_program() {}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    /// `--write` on a scratch tree: the bytes, the table entry appended without touching the existing entries, the stub.
    #[test]
    fn add_program_writes_a_pending_review_entry_and_a_stub() {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let root = std::env::temp_dir().join(format!("kob-add-program-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("crates/kob-protocol/data")).unwrap();
        std::fs::create_dir_all(root.join("crates/kob-tests/tests")).unwrap();
        let table = std::fs::read_to_string(repo.join("crates/kob-protocol/data/programs.json")).unwrap();
        // the scratch table is the embedded one without KForge (embedded since): the write adds it again
        let at = table.find(",\n    {\n      \"name\": \"KForgeToken1785\"").expect("the KForge entry");
        let table = format!("{}{}", &table[..at], &table[at + table[at..].find("\n  ]").unwrap()..]);
        std::fs::write(root.join("crates/kob-protocol/data/programs.json"), &table).unwrap();
        let bytes = std::fs::read(repo.join("contracts/third-party/kforge/program.bin")).unwrap();
        // record code 10, as in the embedded table (the derivation alone takes the next free one, 11, now that KForge is embedded)
        let opt = DeriveOptions {
            name: Some("KForgeToken1785".into()),
            slots: Some(Slots { inputs: 1, outputs: 3 }),
            record_code: Some(10),
            ..Default::default()
        };
        let d = derive_profile(&ProgramInput::Redeem(bytes.clone()), &opt).unwrap();
        let written = write_program(&root, &d.profile, &bytes).unwrap();
        assert_eq!(written.len(), 3, "{written:?}");
        let new_table = std::fs::read_to_string(root.join("crates/kob-protocol/data/programs.json")).unwrap();
        let close = table.rfind("\n  ]").unwrap();
        assert!(new_table.starts_with(&table[..close]), "the existing entries are untouched");
        let f = kob_protocol::profile::ProfileFile::parse(&new_table).unwrap();
        let p = f.programs.last().unwrap();
        assert_eq!((p.name.as_str(), p.review.status, p.record_code), ("KForgeToken1785", ProgramReview::PendingReview, 10));
        assert_eq!(std::fs::read(root.join(&p.source.path)).unwrap(), bytes);
        let stub = std::fs::read_to_string(root.join("crates/kob-tests/tests/program_kforgetoken1785.rs")).unwrap();
        assert!(stub.contains("TemplateId::KForgeToken1785") && stub.contains("#[ignore = \"review:"));
        // a second write of the same program is refused
        assert!(write_program(&root, &d.profile, &bytes).unwrap_err().contains("already"));
        // a refused program (here under another source path) leaves no file behind
        let mut dup = d.profile.clone();
        dup.source.path = "contracts/third-party/elsewhere/program.bin".into();
        assert!(write_program(&root, &dup, &bytes).unwrap_err().contains("already"));
        assert!(!root.join(&dup.source.path).exists(), "nothing written for a refused program");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A program with KCC-1 entry tags (the redeem script of a mainnet token, `kob-protocol/tests/fixtures`) is profiled but
    /// never written: no builder implements its witness scheme.
    #[test]
    fn add_program_profiles_a_tagged_program_but_never_writes_it() {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let hex = std::fs::read_to_string(repo.join("crates/kob-protocol/tests/fixtures/kcc20-silverscript-e108d416.in1.redeem.hex"))
            .unwrap();
        let (input, bytes) = read_input(hex.trim()).unwrap();
        let d = derive_profile(&input, &DeriveOptions { name: Some("SilverKcc20".into()), ..Default::default() }).unwrap();
        assert!(d.blocked.as_deref().is_some_and(|b| b.contains("no builder")), "{:?}", d.blocked);
        let root = std::env::temp_dir().join(format!("kob-add-program-tagged-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("crates/kob-protocol/data")).unwrap();
        let table = std::fs::read_to_string(repo.join("crates/kob-protocol/data/programs.json")).unwrap();
        std::fs::write(root.join("crates/kob-protocol/data/programs.json"), &table).unwrap();
        assert!(write_program(&root, &d.profile, &bytes).unwrap_err().contains("not one of the kron builders"));
        assert!(!root.join(&d.profile.source.path).exists(), "nothing written");
        assert_eq!(std::fs::read_to_string(root.join("crates/kob-protocol/data/programs.json")).unwrap(), table);
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(run_add_program(&[hex.trim(), "--write", "--repo", root.to_str().unwrap()]), 1);
        assert_eq!(run_add_program(&[hex.trim(), "--json"]), 0);
    }
}
