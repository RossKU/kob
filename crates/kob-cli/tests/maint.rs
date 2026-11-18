//! Tests the maintainer binary `kob-maint`: `add-program` lives here and not in `kob`. Nothing is
//! written: no `--write`.

use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_kob-maint");

fn maint(args: &[&str]) -> (i32, String, String) {
    let out = Command::new(BIN).args(args).stdin(Stdio::null()).output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn help_lists_add_program_and_mistakes_are_exit_2() {
    for args in [&["--help"][..], &["-h"], &["help"]] {
        let (code, out, err) = maint(args);
        assert_eq!(code, 0, "{err}");
        assert!(out.contains("kob-maint add-program") && err.is_empty(), "{out}{err}");
    }
    let (code, out, err) = maint(&["add-program", "--help"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("usage: kob-maint add-program") && out.contains("--write"), "{out}");
    for args in [&[][..], &["nosuch"], &["add-program"], &["add-program", "--bogus"], &["add-program", "not-hex-and-not-a-file"]] {
        let (code, out, err) = maint(args);
        assert_eq!(code, 2, "{args:?}: {out}{err}");
        assert!(out.is_empty() && !err.is_empty(), "{args:?}: {out}{err}");
    }
}

/// A program already embedded in this build is recognised from its redeem script: nothing to add, and nothing is written.
#[test]
fn add_program_derives_the_profile_of_an_embedded_program_from_its_bytes() {
    let hex = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../kob-protocol/tests/fixtures/kcc20-silverscript-e108d416.in1.redeem.hex"
    ))
    .unwrap();
    // A program with entry tags that no builder serves: described, never listed, never written.
    let (code, out, err) = maint(&["add-program", hex.trim(), "--name", "SilverKcc20", "--json"]);
    assert_eq!(code, 0, "{err}");
    let doc: serde_json::Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("{e}: {out}"));
    assert!(doc["blocked"].as_str().is_some_and(|b| b.contains("no builder")), "{doc}");
    assert!(doc["registry_template"].is_null(), "{doc}");
    // --write of a blocked program is refused (exit 1), before anything is written
    let (code, _, err) = maint(&["add-program", hex.trim(), "--name", "SilverKcc20", "--write", "--repo", env!("CARGO_MANIFEST_DIR")]);
    assert_eq!(code, 1, "{err}");
    assert!(err.contains("--write: refused"), "{err}");
}
