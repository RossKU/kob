//! The build this executor is: its crate version, and the git commit when the build says which one it is.
//!
//! The commit comes from the environment variable `KOB_GIT_COMMIT` at BUILD time (`option_env!`), set by the release
//! workflow (`.github/workflows/release.yml`) and the `Dockerfile`; a plain `cargo build` has none, and a build is the same
//! bytes with or without a git checkout around it: there is no build script, no `git` call, no timestamp. `GET /v1/health`
//! reports both (`version`, `commit`), so an operator or a host can check which release an indexer runs
//! (docs/ops/release.md, 7).

use serde_json::{json, Value};

/// The crate version (the workspace's).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The commit the build was made from: `KOB_GIT_COMMIT` when it was set and not blank at build time.
pub fn commit() -> Option<&'static str> {
    commit_of(option_env!("KOB_GIT_COMMIT"))
}

fn commit_of(env: Option<&'static str>) -> Option<&'static str> {
    env.map(str::trim).filter(|c| !c.is_empty())
}

/// `version` and `commit` as `/v1/health` serves them, and the `KOB1` payload version this build writes and reads.
pub fn health_fields() -> Value {
    json!({
        "version": VERSION,
        "commit": commit(),
        "payload_version": kob_protocol::payload::PAYLOAD_VERSION,
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
    }
}
