//! The update notice: a newer KOB release in use on chain.
//!
//! Every placement a KOB build writes carries the release of that build, in the optional
//! `RELEASE` record of the KOB1 payload (`docs/spec/kob1-payload.md`). The indexer notes the
//! releases newer than its own build that it finds next to the placements it accepts. Once one
//! of them has been seen on [`MIN_PLACEMENTS`] placements (distinct transactions) of at least
//! [`MIN_MAKERS`] makers, the indexer reports it in four places:
//! - `newer_release_seen` of `/v1/health`.
//! - A suffix of the status line.
//! - The `kob_newer_release_seen` metric.
//! - An INFO log line, once per version.
//!
//! The bar exists because anyone can write any release into a payload. One record cannot raise
//! the notice, and raising it still costs real orders. There is no project server. An operator
//! learns of a release from the chain and from announcements (`docs/ops/release.md`).
//!
//! The notice is informational. Nothing the indexer, the matcher or the keepers do depends on it.
//!
//! # Storage
//!
//! The sightings are kept in the database (`meta.release_sightings`, a few hundred bytes of
//! JSON), so a restart keeps the notice. They are written in the transaction of the block that
//! carried them. A reorg does not revert them, because a reorged-away placement may have counted.
//! Only releases newer than the build are stored, and at most [`MAX_VERSIONS`] of them. The
//! lowest are kept, since the real next release is the closest one. Once the operator runs that
//! release, it is no longer newer and the notice is gone.

use super::db::{meta_get, meta_set, DbResult};
use crate::hex::Hash32;
use kob_protocol::payload::Release;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

/// Placements (distinct transactions) a release must be seen on before it is reported.
pub const MIN_PLACEMENTS: usize = 3;
/// Distinct makers of those placements.
pub const MIN_MAKERS: usize = 2;
/// Newer releases tracked at most (the lowest are kept).
pub const MAX_VERSIONS: usize = 8;

const META_KEY: &str = "release_sightings";

/// The newest release seen often enough on chain, as shown in `/v1/health`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NewerRelease {
    /// `0.1.1`.
    pub version: Release,
    /// DAA score of the first placement seen with it.
    pub first_seen_daa: u64,
}

/// What is known of one newer release.
///
/// It holds where the release was first seen and, up to the bar, the placements and makers it
/// was seen with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Seen {
    version: Release,
    first_seen_daa: u64,
    txids: Vec<Hash32>,
    makers: Vec<Hash32>,
}

impl Seen {
    fn confirmed(&self) -> bool {
        self.txids.len() >= MIN_PLACEMENTS && self.makers.len() >= MIN_MAKERS
    }
}

/// The releases newer than a build that were seen next to accepted placements.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sightings {
    versions: Vec<Seen>,
}

impl Sightings {
    /// Notes that placement `txid` of `maker`, in a block of DAA score `daa`, announces `version`.
    ///
    /// The build has release `core` (a pre-release build when `prerelease`). The call is
    /// idempotent: the same placement again, as in a replay, changes nothing. Returns whether
    /// anything changed.
    pub fn note(&mut self, core: Release, prerelease: bool, version: Release, maker: Hash32, txid: Hash32, daa: u64) -> bool {
        if !version.is_newer_than(core, prerelease) {
            return false;
        }
        let at = match self.versions.binary_search_by(|s| s.version.cmp(&version)) {
            Ok(i) => i,
            Err(i) => {
                if self.versions.len() >= MAX_VERSIONS {
                    if i >= MAX_VERSIONS {
                        return false; // higher than every release kept: the closest ones matter
                    }
                    self.versions.pop();
                }
                self.versions.insert(i, Seen { version, first_seen_daa: daa, txids: vec![txid], makers: vec![maker] });
                return true;
            }
        };
        let s = &mut self.versions[at];
        let mut changed = false;
        if daa < s.first_seen_daa {
            s.first_seen_daa = daa;
            changed = true;
        }
        if s.txids.len() < MIN_PLACEMENTS && !s.txids.contains(&txid) {
            s.txids.push(txid);
            changed = true;
        }
        if s.makers.len() < MIN_MAKERS && !s.makers.contains(&maker) {
            s.makers.push(maker);
            changed = true;
        }
        changed
    }

    /// The newest release past the bar that is newer than a build of `core`, or `None`.
    pub fn newer(&self, core: Release, prerelease: bool) -> Option<NewerRelease> {
        self.versions
            .iter()
            .rev()
            .find(|s| s.confirmed() && s.version.is_newer_than(core, prerelease))
            .map(|s| NewerRelease { version: s.version, first_seen_daa: s.first_seen_daa })
    }

    /// Drops what is not newer than a build of `core`, as after an update.
    fn retain_newer(&mut self, core: Release, prerelease: bool) {
        self.versions.retain(|s| s.version.is_newer_than(core, prerelease));
    }
}

/// The stored sightings.
///
/// The result is empty when there are none or the row does not parse. The row is informational
/// and is rebuilt from new placements.
pub fn load(conn: &Connection) -> DbResult<Sightings> {
    let mut s: Sightings = meta_get(conn, META_KEY)?.and_then(|j| serde_json::from_str(&j).ok()).unwrap_or_default();
    s.retain_newer(Release::build_core(), Release::build_is_prerelease());
    Ok(s)
}

/// Stores the sighting through [`Sightings::note`] for this build.
///
/// Returns whether the stored sightings changed.
pub fn note(conn: &Connection, version: Release, maker: Hash32, txid: Hash32, daa: u64) -> DbResult<bool> {
    if !version.is_newer_than_build() {
        return Ok(false);
    }
    let mut s = load(conn)?;
    if !s.note(Release::build_core(), Release::build_is_prerelease(), version, maker, txid, daa) {
        return Ok(false);
    }
    meta_set(conn, META_KEY, &serde_json::to_string(&s).expect("sightings serialize"))?;
    Ok(true)
}

/// The newer release this build should tell its operator about, or `None`.
pub fn newer_seen(conn: &Connection) -> DbResult<Option<NewerRelease>> {
    Ok(load(conn)?.newer(Release::build_core(), Release::build_is_prerelease()))
}

/// Shows `n` in the health and logs it at INFO, once per version.
///
/// The health is `/v1/health`, the status line and the metric. The log line is written when `n` is
/// not the release the health already shows. Returns whether `n` was new.
pub fn publish(health: &super::status::HealthState, n: NewerRelease) -> bool {
    let mut new = false;
    health.update(|h| {
        new = h.newer_release_seen.as_ref().map(|o| o.version) != Some(n.version);
        h.newer_release_seen = Some(n.clone());
    });
    if new {
        tracing::info!(
            version = %n.version,
            first_seen_daa = n.first_seen_daa,
            this_build = env!("CARGO_PKG_VERSION"),
            "{}: placements of several makers announce it (docs/ops/release.md); nothing changes until you do",
            notice_text(&n)
        );
    }
    new
}

/// The words the status line uses for the notice.
pub fn notice_text(n: &NewerRelease) -> String {
    format!("a newer KOB release is in use on chain ({}): update", n.version)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CORE: Release = Release::new(0, 1, 0);

    fn h(b: u8) -> Hash32 {
        Hash32([b; 32])
    }

    #[test]
    fn a_release_is_reported_after_three_placements_of_two_makers() {
        let v = Release::new(0, 1, 1);
        let mut s = Sightings::default();
        assert!(s.note(CORE, false, v, h(1), h(10), 500));
        assert_eq!(s.newer(CORE, false), None);
        // The same placement again (a replay) and more placements of one maker do not pass the bar.
        assert!(!s.note(CORE, false, v, h(1), h(10), 500));
        assert!(s.note(CORE, false, v, h(1), h(11), 600));
        assert!(s.note(CORE, false, v, h(1), h(12), 700));
        assert_eq!(s.newer(CORE, false), None, "one maker");
        // A second maker passes the bar. The first sighting's DAA is reported.
        assert!(s.note(CORE, false, v, h(2), h(13), 800));
        assert_eq!(s.newer(CORE, false), Some(NewerRelease { version: v, first_seen_daa: 500 }));
        // Nothing more is noted once past the bar, so the state stays bounded.
        assert!(!s.note(CORE, false, v, h(3), h(14), 900));
        // An earlier block seen later (a replay from further back) moves the first sighting back.
        assert!(s.note(CORE, false, v, h(3), h(15), 400));
        assert_eq!(s.newer(CORE, false).unwrap().first_seen_daa, 400);
    }

    #[test]
    fn two_makers_on_two_placements_are_not_enough() {
        let v = Release::new(0, 2, 0);
        let mut s = Sightings::default();
        s.note(CORE, false, v, h(1), h(10), 1);
        s.note(CORE, false, v, h(2), h(11), 2);
        assert_eq!(s.newer(CORE, false), None);
        s.note(CORE, false, v, h(2), h(12), 3);
        assert_eq!(s.newer(CORE, false).map(|n| n.version), Some(v));
    }

    #[test]
    fn only_strictly_newer_releases_count() {
        let mut s = Sightings::default();
        for (k, v) in [Release::new(0, 1, 0), Release::new(0, 0, 9)].into_iter().enumerate() {
            for m in 0..3u8 {
                assert!(!s.note(CORE, false, v, h(m), h(k as u8 * 10 + m), 1));
            }
        }
        assert_eq!(s, Sightings::default());
        // The same core is newer than a pre-release build of it (0.1.0 after 0.1.0-rc.1).
        assert!(s.note(CORE, true, Release::new(0, 1, 0), h(1), h(1), 1));
    }

    #[test]
    fn the_newest_release_past_the_bar_is_reported() {
        let mut s = Sightings::default();
        let (a, b) = (Release::new(0, 1, 1), Release::new(0, 2, 0));
        for (k, m) in [(1u8, 1u8), (2, 2), (3, 1)] {
            s.note(CORE, false, a, h(m), h(k), 100 + k as u64);
        }
        // A newer release seen once: the older one is still the one past the bar.
        s.note(CORE, false, b, h(9), h(20), 300);
        assert_eq!(s.newer(CORE, false).map(|n| n.version), Some(a));
        for (k, m) in [(21u8, 8u8), (22, 9)] {
            s.note(CORE, false, b, h(m), h(k), 300);
        }
        assert_eq!(s.newer(CORE, false), Some(NewerRelease { version: b, first_seen_daa: 300 }));
        // After an update to 0.1.1 only 0.2.0 is newer. After one to 0.2.0 nothing is.
        assert_eq!(s.newer(a, false).map(|n| n.version), Some(b));
        assert_eq!(s.newer(b, false), None);
    }

    #[test]
    fn at_most_the_lowest_releases_are_kept() {
        let mut s = Sightings::default();
        // A flood of made-up high releases does not crowd out the real next one.
        for k in 0..20u64 {
            s.note(CORE, false, Release::new(9, k, 0), h(1), h(k as u8), 1);
        }
        assert_eq!(s.versions.len(), MAX_VERSIONS);
        let next = Release::new(0, 1, 1);
        assert!(s.note(CORE, false, next, h(1), h(100), 5));
        assert_eq!(s.versions.len(), MAX_VERSIONS);
        assert_eq!(s.versions[0].version, next);
        assert!(!s.note(CORE, false, Release::new(99, 0, 0), h(1), h(101), 5), "above every kept release");
    }

    #[test]
    fn the_sightings_persist_in_the_store() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE meta (k TEXT PRIMARY KEY, v TEXT NOT NULL)").unwrap();
        let core = Release::build_core();
        let next = Release::new(core.major, core.minor, core.patch + 1);
        assert_eq!(newer_seen(&conn).unwrap(), None);
        // This build's own release and older ones are never stored.
        assert!(!note(&conn, core, h(1), h(1), 1).unwrap());
        assert_eq!(meta_get(&conn, META_KEY).unwrap(), None);
        for (k, m) in [(1u8, 1u8), (2, 2), (3, 1)] {
            assert!(note(&conn, next, h(m), h(k), 10 * k as u64).unwrap());
        }
        assert!(!note(&conn, next, h(1), h(1), 10).unwrap(), "a replayed placement");
        assert_eq!(newer_seen(&conn).unwrap(), Some(NewerRelease { version: next, first_seen_daa: 10 }));
        // A row that does not parse is ignored, because the sightings are informational.
        meta_set(&conn, META_KEY, "{not json").unwrap();
        assert_eq!(newer_seen(&conn).unwrap(), None);
    }

    #[test]
    fn the_health_shows_it_and_it_is_new_once_per_version() {
        let health = super::super::status::HealthState::new("testnet-10");
        let n = |v: Release, daa| NewerRelease { version: v, first_seen_daa: daa };
        assert!(publish(&health, n(Release::new(0, 1, 1), 5)));
        assert_eq!(health.snapshot().newer_release_seen, Some(n(Release::new(0, 1, 1), 5)));
        assert!(!publish(&health, n(Release::new(0, 1, 1), 4)), "the same version: no second log line");
        assert_eq!(health.snapshot().newer_release_seen.unwrap().first_seen_daa, 4);
        assert!(publish(&health, n(Release::new(0, 2, 0), 9)));
    }

    #[test]
    fn the_notice_reads_as_the_status_line_says_it() {
        let n = NewerRelease { version: Release::new(0, 1, 1), first_seen_daa: 7 };
        assert_eq!(notice_text(&n), "a newer KOB release is in use on chain (0.1.1): update");
        assert_eq!(serde_json::to_value(&n).unwrap(), serde_json::json!({"version": "0.1.1", "first_seen_daa": 7}));
    }
}
