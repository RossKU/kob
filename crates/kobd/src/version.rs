//! The build this executor is: its crate version, and the git commit when the build says which one.
//!
//! The commit comes from the environment variable `KOB_GIT_COMMIT` at build time (`option_env!`).
//! The release workflow (`.github/workflows/release.yml`) and `crates/kobd/Dockerfile` set it. A
//! plain `cargo build` has none.
//!
//! A build is the same bytes with or without a git checkout around it. There is no build script,
//! no `git` call and no timestamp.
//!
//! `GET /v1/health` reports both facts (`version`, `commit`). So an operator or a host can check
//! which release an indexer runs (`docs/ops/release.md`, section 7).

use serde_json::{json, Value};

/// The crate version (the workspace's).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The commit the build was made from, when `KOB_GIT_COMMIT` was set and not blank at build time.
pub fn commit() -> Option<&'static str> {
    commit_of(option_env!("KOB_GIT_COMMIT"))
}

fn commit_of(env: Option<&'static str>) -> Option<&'static str> {
    env.map(str::trim).filter(|c| !c.is_empty())
}

/// The template generation of this build.
///
/// It is the first 16 hex digits of the blake3 hash of the template hashes of the order templates
/// the build pins (`kob_protocol::artifacts::PINNED`, in order). Two builds with the same generation
/// fill the same orders. An executor whose generation differs from the release's misses the orders
/// of the other. See `docs/ops/release.md`, section 7, and `docs/spec/template-retirement.md`.
pub fn template_generation() -> &'static str {
    static GEN: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    GEN.get_or_init(|| {
        let mut h = blake3::Hasher::new();
        for (_, hash) in kob_protocol::artifacts::PINNED.iter() {
            h.update(hash.as_bytes());
        }
        h.finalize().to_hex()[..16].to_string()
    })
}

/// The commits that retired the templates this build still serves in their cancel-only role.
///
/// Each commit is listed once, in the order of the table (`kob_protocol::retired`).
pub fn cancel_only_generations() -> Vec<&'static str> {
    let mut v: Vec<&'static str> = vec![];
    for r in kob_protocol::retired::retired() {
        let by = r.retirement.retired_by();
        if !v.contains(&by) {
            v.push(by);
        }
    }
    v
}

/// The network this build is bound to, as `--version` names it.
///
/// It reads `testnet-10 build` or `mainnet build`. The reference build takes its network from the
/// configuration and reads `development build, no network`.
pub fn build_kind() -> String {
    match kob_protocol::artifacts::deployment_network() {
        Some(n) => format!("{n} build"),
        None => "development build, no network".to_string(),
    }
}

/// The version as `kobd --version` prints it after the name.
///
/// An example is `0.1.0 (testnet-10 build, commit 9e877ae7c0de, payload v4, templates
/// 1a2b3c4d5e6f7a8b)`. The commit is cut to 12 characters. The start log and `/v1/health` carry the
/// same facts.
pub fn long() -> &'static str {
    static LONG: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    LONG.get_or_init(|| {
        let commit = commit().map(|c| c.get(..12).unwrap_or(c)).unwrap_or("unknown");
        format!(
            "{VERSION} ({}, commit {commit}, payload v{}, templates {})",
            build_kind(),
            kob_protocol::payload::PAYLOAD_VERSION,
            template_generation()
        )
    })
}

/// One line for the start log: `kobd ` followed by [`long`].
pub fn build_line() -> String {
    format!("kobd {}", long())
}

/// The `kob_build_info` series (Prometheus): 1, with the build in its labels.
pub fn build_info_metric() -> String {
    format!(
        "# HELP kob_build_info The build: crate version, commit, KOB1 payload version and template generation (labels).\n# TYPE kob_build_info gauge\nkob_build_info{{version=\"{VERSION}\",commit=\"{}\",payload_version=\"{}\",template_generation=\"{}\"}} 1\n",
        commit().unwrap_or(""),
        kob_protocol::payload::PAYLOAD_VERSION,
        template_generation()
    )
}

/// The build facts `/v1/health` serves.
///
/// They are `version`, `commit`, the `KOB1` payload version this build writes and reads, its
/// template generation and the retired generations it serves for their makers' cancel.
pub fn health_fields() -> Value {
    json!({
        "version": VERSION,
        "commit": commit(),
        "payload_version": kob_protocol::payload::PAYLOAD_VERSION,
        "template_generation": template_generation(),
        "cancel_only_generations": cancel_only_generations(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_commit_is_there_only_when_the_build_set_it() {
        assert_eq!(commit_of(None), None);
        assert_eq!(commit_of(Some("")), None);
        assert_eq!(commit_of(Some("  \n")), None);
        assert_eq!(commit_of(Some("9e877ae7\n")), Some("9e877ae7"));
        let v = health_fields();
        assert_eq!(v["version"], VERSION);
        assert_eq!(v["payload_version"], kob_protocol::payload::PAYLOAD_VERSION);
        assert_eq!(v["commit"].as_str(), commit());
        // The template generation is 16 hex digits and stable. The build line and the metric name it.
        let g = template_generation();
        assert_eq!(g.len(), 16);
        assert!(g.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(v["template_generation"], g);
        assert!(build_line().contains(g) && build_info_metric().contains(&format!("template_generation=\"{g}\"")));
        // `--version` names the network the build is bound to (or none) and the commit.
        assert!(long().starts_with(VERSION) && long().contains(&build_kind()) && long().contains("commit "), "{}", long());
        match kob_protocol::artifacts::deployment_network() {
            Some(n) => assert_eq!(build_kind(), format!("{n} build")),
            None => assert_eq!(build_kind(), "development build, no network"),
        }
        assert!(!cancel_only_generations().is_empty());
    }
}
