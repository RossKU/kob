//! `kob registry programs`: the token-program profiles that this build embeds
//! (`crates/kob-protocol/data/programs.json`).
//!
//! The maintainer command that adds a program is `kob-maint add-program` (`src/maint`).

use kob_protocol::profile::profiles;

const USAGE_PROGRAMS: &str = "usage: kob registry programs [--json]

Prints the token-program profiles this build embeds: name, registry id, codec, template hash, layout, slots, carrier floor,
witness scheme and review status. --json prints them as one JSON document.";

/// Run `kob registry programs [--json]` and return the process exit code.
pub fn run_programs(args: &[&str]) -> i32 {
    if let Some(code) = crate::help::help_or_usage(args, &[USAGE_PROGRAMS], false) {
        return code;
    }
    let json = match args {
        [] => false,
        ["--json"] => true,
        _ => return crate::help::print_usage_error(&[USAGE_PROGRAMS]),
    };
    if json {
        println!("{}", crate::text::json(profiles()));
        return 0;
    }
    println!(
        "{:<22} {:<24} {:<6} {:<64} {:>6} {:>5} {:>10} {:<14} review",
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
            "{:<22} {:<24} {:<6} {:<64} {:>6} {:>5} {:>10} {:<14} {}",
            p.name,
            p.registry_id,
            p.codec.as_str(),
            p.template_hash,
            format!("{}+{}", p.layout.prefix_len, p.layout.state_len),
            format!("{}/{}", p.slots.inputs, p.slots.outputs),
            p.carrier_floor,
            witness,
            serde_json::to_value(p.review.status).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default()
        );
    }
    0
}
