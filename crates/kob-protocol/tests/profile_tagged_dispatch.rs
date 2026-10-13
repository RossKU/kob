//! Token programs with KCC-1 entry tags (`silverc` v1 dispatch), profiled read-only by `kob registry add-program`.
//!
//! Fixtures (public data):
//! * `fixtures/kcc20-silverscript-e108d416.in1.redeem.hex`: the redeem script of input 1 of mainnet transaction
//!   `e108d4161c14883e05940c933b616acbc119a0163e2380e16ef9782481d00f99`, a token of the SilverScript KCC20 example
//!   (`kcc20.sil`);
//! * `fixtures/kcc20-silverscript-4x5.artifact.json`: that example compiled by `silverc` v1 with `maxCovIns = 4`,
//!   `maxCovOuts = 5` (its bytecode is the redeem script above, byte for byte).

use kob_protocol::profile::{
    derive_profile, tag_dispatch, DeriveOptions, ProfileFile, ProgramInput, ProgramReview, SourceKind, WitnessScheme,
};
use kob_protocol::registry::Family;

const REDEEM_HEX: &str = include_str!("fixtures/kcc20-silverscript-e108d416.in1.redeem.hex");
const ARTIFACT: &str = include_str!("fixtures/kcc20-silverscript-4x5.artifact.json");
/// The template hash `silverc` records for the artifact (its cut: `OP_TOALTSTACK` prefix, 46-byte state).
const TEMPLATE_HASH: &str = "b5323a04194ba2343adf8fc08da9cfe746aa2a4baef74b1a18ee518b508b45cf";
const LEADER: (&str, &str) = ("25061d6e", "__leader_transfer({byte[32],byte,int,bool}[],sig,byte)");
const DELEGATE: (&str, &str) = ("5123e0e2", "__delegate(sig,byte)");

fn redeem() -> Vec<u8> {
    kob_protocol::json::from_hex(REDEEM_HEX.trim()).unwrap()
}

fn opt() -> DeriveOptions {
    DeriveOptions { name: Some("SilverKcc20".into()), ..Default::default() }
}

#[test]
fn a_mainnet_tagged_program_gets_its_entries_tags_and_layout() {
    let bytes = redeem();
    assert_eq!(bytes.len(), 2662);
    let d = derive_profile(&ProgramInput::Redeem(bytes.clone()), &opt()).unwrap();
    let p = &d.profile;
    assert_eq!(p.template_hash, TEMPLATE_HASH, "{}", d.detection);
    assert_eq!((p.layout.prefix_len, p.layout.state_len, p.layout.suffix_len), (1, 46, 2615));
    assert_eq!((p.codec, p.source.kind), (Family::Kron, SourceKind::RawRedeem));
    assert_eq!(p.witness.scheme, WitnessScheme::Kcc1Tagged);
    assert!(p.witness.trailing_pushes.is_empty());
    let entries: Vec<(&str, Option<&str>)> = p.witness.entries.iter().map(|e| (e.tag.as_str(), e.signature.as_deref())).collect();
    assert_eq!(entries, vec![(LEADER.0, Some(LEADER.1)), (DELEGATE.0, Some(DELEGATE.1))]);
    assert_eq!(p.witness.entries[0].name(), Some("__leader_transfer"));
    // each tag is the KCC-1 tag of its signature
    for (tag, sig) in [LEADER, DELEGATE] {
        assert_eq!(kob_protocol::json::to_hex(&kob_protocol::kcc1::dispatch_tag(sig)), tag);
    }
    assert!(d.detection.contains("KCC-1 entry-tag dispatch") && d.detection.contains("25061d6e __leader_transfer"), "{}", d.detection);
    // the review tasks name the entries; no "unknown dispatch"
    let tasks = d.review_tasks.join("\n");
    assert!(!tasks.contains("not a known dispatch pattern"), "{tasks}");
    for want in [
        "no KOB builder implements kcc1-tagged",
        "entry __leader_transfer (tag 25061d6e",
        "entry __delegate (tag 5123e0e2",
        "owner types",
    ] {
        assert!(tasks.contains(want), "{want}: {tasks}");
    }
    assert_eq!(p.review.status, ProgramReview::PendingReview);
}

#[test]
fn a_tagged_program_is_never_tradable() {
    let d = derive_profile(&ProgramInput::Redeem(redeem()), &opt()).unwrap();
    assert!(d.blocked.as_deref().is_some_and(|b| b.contains("no builder")), "{:?}", d.blocked);
    assert_eq!(d.already_embedded, None);
    // the table refuses it: no builder of its family implements the scheme
    assert!(d.profile.check_shape().unwrap_err().message.contains("not one of the kron builders"));
    let mut table = kob_protocol::profile::table().clone();
    table.programs.push(d.profile.clone());
    assert!(table.check().is_err());
    let json = serde_json::to_string(&table).unwrap();
    assert!(ProfileFile::parse(&json).is_err());
    // and no embedded program or registry template has its hash
    assert!(kob_protocol::profile::by_hash(&d.profile.hash()).is_none());
    let reg = kob_protocol::registry::Registry::default_registry();
    assert!(reg.templates.iter().all(|t| t.template_hash != TEMPLATE_HASH));
}

#[test]
fn the_compiled_artifact_gives_the_same_profile_with_its_abi() {
    let d = derive_profile(&ProgramInput::Artifact(ARTIFACT.into()), &opt()).unwrap();
    let r = derive_profile(&ProgramInput::Redeem(redeem()), &opt()).unwrap();
    assert_eq!(d.profile.source.kind, SourceKind::SilvercArtifact);
    assert_eq!(
        (&d.profile.template_hash, d.profile.layout, d.profile.codec),
        (&r.profile.template_hash, r.profile.layout, r.profile.codec)
    );
    // the ABI's entries, in the order the dispatcher tests them: the same tags and signatures the bytes gave
    assert_eq!(d.profile.witness, r.profile.witness);
    assert!(d.blocked.is_some());
}

#[test]
fn the_dispatch_walk_accepts_only_the_exact_shape() {
    let bytes = redeem();
    let (prefix, suffix) = (&bytes[..1], &bytes[47..]);
    let tags = tag_dispatch(prefix, suffix).unwrap();
    assert_eq!(tags.iter().map(|t| kob_protocol::json::to_hex(t)).collect::<Vec<_>>(), vec![LEADER.0, DELEGATE.0]);
    // no OP_TOALTSTACK prefix; no OP_FROMALTSTACK
    assert_eq!(tag_dispatch(&[], suffix), None);
    assert_eq!(tag_dispatch(prefix, &suffix[1..]), None);
    // the fallthrough is not OP_RETURN, an ENDIF missing or extra, a truncated script
    let mut s = suffix.to_vec();
    let n = s.len();
    s[n - 3] = 0x51;
    assert_eq!(tag_dispatch(prefix, &s), None);
    assert_eq!(tag_dispatch(prefix, &suffix[..n - 1]), None);
    assert_eq!(tag_dispatch(prefix, &[suffix, &[0x68][..]].concat()), None);
    assert_eq!(tag_dispatch(prefix, &suffix[..200]), None);
    // the `== 0` selector of the KRON-style programs is not a tag dispatch
    assert_eq!(tag_dispatch(&[0x6b], &[0x6c, 0x76, 0x00, 0x9c, 0x63, 0x75, 0x51, 0x67, 0x6a, 0x68]), None);
    // a minimal two-entry dispatch with a tag-like run inside a push of the first entry: still two entries
    let fake = [0x08, 0x76, 0x04, 0xaa, 0xbb, 0xcc, 0xdd, 0x87, 0x63, 0x75];
    let mut s = vec![0x6c, 0x76, 0x04, 1, 2, 3, 4, 0x87, 0x63, 0x75];
    s.extend_from_slice(&fake);
    s.extend_from_slice(&[0x75, 0x51, 0x67, 0x76, 0x04, 5, 6, 7, 8, 0x87, 0x63, 0x75, 0x51, 0x67, 0x6a, 0x68, 0x68]);
    assert_eq!(tag_dispatch(&[0x6b], &s), Some(vec![[1, 2, 3, 4], [5, 6, 7, 8]]));
}
