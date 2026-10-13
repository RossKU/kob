//! The hex examples of `docs/spec/kob1-payload.md` ("Examples") are the golden vectors' payloads, rebuilt here with the
//! builders and the encoder: a change of the payload format or of the fixtures that moves a byte fails this test until the
//! document is updated with it.

mod common;

use kob_protocol::build::build;
use kob_protocol::payload::{encode, Record};

const DOC: &str = include_str!("../../../docs/spec/kob1-payload.md");

/// The payload of a golden-vector scenario, as the builder encodes it.
fn payload_of(name: &str) -> Vec<u8> {
    let (_, action) = common::scenarios().into_iter().find(|(n, _)| n == name).unwrap_or_else(|| panic!("no scenario {name}"));
    build(&action).unwrap_or_else(|e| panic!("{name}: {e}")).tx.payload
}

/// The bytes of the first code block after `marker` in the Examples section: on each line, the hex groups before the
/// description (which starts after two or more spaces, or after the last hex group).
fn documented(marker: &str) -> Vec<u8> {
    let examples = &DOC[DOC.find("## Examples").expect("Examples section")..];
    let at = examples.find(marker).unwrap_or_else(|| panic!("no `{marker}` in the Examples section"));
    let rest = &examples[at..];
    let open = rest.find("```\n").expect("code block") + 4;
    let close = open + rest[open..].find("```").expect("closed code block");
    let mut hex = String::new();
    for line in rest[open..close].lines() {
        let fields = line.split("  ").next().unwrap_or("");
        for group in fields.split_whitespace() {
            assert!(group.bytes().all(|b| b.is_ascii_hexdigit()), "`{marker}`: `{group}` in `{line}` is not hex");
            hex.push_str(group);
        }
    }
    kob_protocol::json::from_hex(&hex).unwrap_or_else(|e| panic!("`{marker}`: {e}"))
}

#[test]
fn the_documented_payload_examples_are_the_encoders_bytes() {
    for name in ["create.ask", "amend.ask"] {
        let want = payload_of(name);
        let got = documented(&format!("`{name}`"));
        assert_eq!(kob_protocol::json::to_hex(&got), kob_protocol::json::to_hex(&want), "{name}");
        // the byte count the document gives
        assert!(
            DOC.contains(&format!("`{name}` vector ({} bytes)", want.len()))
                || DOC.contains(&format!("`{name}` ({} bytes)", want.len())),
            "{name}: {} bytes",
            want.len()
        );
    }
    let x402 = encode(&[Record::X402 { reference: vec![0xab; 32] }, Record::Note { text: "x402-client/1".into() }]).unwrap();
    assert_eq!(documented("`x402.only`"), x402);
    // the day order: flag bit 0 and the deadline appended
    let day = payload_of("create.ask.day");
    assert!(DOC.contains(&format!("`create.ask.day` ({} bytes)", day.len())));
    assert!(kob_protocol::json::to_hex(&day).ends_with("80a2f1d506") && DOC.contains("`80a2f1d506`"));
}
