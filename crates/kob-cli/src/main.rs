//! The `kob` command line.
//!
//! The commands are:
//! - `kob token create`, `resume` and `verify-declaration`: fixed-supply KCC-20 issuance.
//! - `kob registry validate`, `verify-genesis` and `programs` (`add-program` is `kob-maint`, a maintainer binary).
//! - `kob recover`: find the orders of a maker on the node from a backup, a recovery or export
//!   file or an indexer view, and cancel them with `--cancel`.
//! - The trading commands `kob place`, `replace`, `cancel`, `sweep`, `refund`, `take`, `send` and `merge`;
//!   `cancel` and `sweep` take saved indexer order views with `--view FILE`.
//! - The views `kob book`, `orders` and `balances` ([`trade`]), and `kob watch` and `kob tokens`.

mod common;
mod congestion;
mod feecap;
mod help;
mod inputs;
mod nodelink;
mod order;
mod programs;
mod recent;
mod recover;
mod registry;
mod tls;
mod token;
mod token_lock;
mod token_sale;
mod tokens;
mod trade;
mod watch;
mod wrpc;

#[cfg(test)]
mod tests_e2e;

const USAGE: &str = "\
kob: KOB command line

USAGE
  kob <command> [OPTIONS]      `kob help <command>` and `kob <command> --help` print the help of one command

READ (no key)
  kob tokens [SEARCH] [--all]  the tokens with a KOB market (the indexer's), with --all every token kascov.io lists too
  kob book <TOKEN> [--quote B] the order book (KAS or a pair), from the indexer
  kob orders [--token T] [--all]
                               the maker's orders (--maker KEY|ADDRESS, else the signing key's public key)
  kob balances [--maker KEY|ADDRESS]
                               free KAS and tokens, and what the maker's live orders hold (custody, strays, KAS)
  kob watch fills|book|order|health [...]
                               follow the executor's live feed: fills, a book, orders' states, indexer progress

TRADE (a key: --key-file PATH or --key-env NAME, else $KOB_KEY; --key was removed: a key on the command line is visible to others)
  kob place <KIND> --token T [--quote B] --side buy|sell --amount A [...]
                               place an order as the web wallet plans it: limit, ioc, fok, market, streaming, close,
                               twap, dca, dutch, stop-market, stop-limit, trailing-stop, take-profit, oco, ifd, ifo,
                               repeat-ifd, repeat-ifo; every kind on a pair with --quote (`kob place --help`)
  kob replace <COVENANT_ID> [--price P] [--amount A] [--tip T] [--stop P] [--renew | --gtd WHEN] [...]
                               amend a live order in place, or cancel-replace it in one transaction (trailing stops,
                               if-done entries and pair orders are re-planned from their terms)
  kob cancel <COVENANT_ID>...  the maker's cancel by covenant id (indexer view, checked on the node); `--view FILE` instead: saved views
  kob sweep <COVENANT_ID>... | --all | --view FILE...
                               return an order's strays to the maker, the order continues (`kob sweep --help`)
  kob refund <COVENANT_ID>     the permissionless refund of an ended order (the sender keeps the refund tip)
  kob claim <COVENANT_ID> [--merge | --sell --price P]
                               take the tokens a collected buy order bought (alone, merged with yours, or sold)
  kob take <COVENANT_ID> --amount A
                               fill a resting order as the taker
  kob send <TOKEN|KAS> <TO> <AMOUNT>
                               send a token (within its program's slots) or KAS
  kob merge <TOKEN>            merge the key's own UTXOs of a token into one (a chain of transfers to itself)
  The trading commands show the plan and sign only with --yes or a terminal's confirmation; --dry-run stops before
  signing; --json prints one JSON document. A plan whose fee is above 4 times the node's fee estimate is refused
  (FEE_ABOVE_CEILING, exit 1) unless --allow-high-fee is given; this holds for every command that signs.

TOKENS AND REGISTRY
  kob token create [OPTIONS]   create a fixed-supply KCC-20 token: KCC-23 declaration, then the genesis (`kob token create --help`);
                               submits only with --yes or a terminal's confirmation; --dry-run stops before submitting
  kob token resume <DIR>       send a creation whose genesis did not go out (`kob token resume --help`)
  kob token unlock <TOKEN|DIR> release a creator lock's tokens after its time (`kob token unlock --help`)
  kob token verify-declaration <PROOF>
                               check a KCC-23 launch proof from any source and print the declaration
  kob registry validate <tokens.json> [--network NET]   check a token registry file
  kob registry verify-genesis --network NET [--evidence FILE] [--registry FILE] [--json]
                               re-derive each token's genesis check and live-minter check from chain evidence
  kob registry programs [--json]   the token-program profiles this build embeds
  kob recover --from <FILE>... --node ws://HOST:PORT [--amount-left <AMOUNT>] [--cancel --key-file PATH [--yes] ...]
                               find orders on the node from a backup / export / indexer view; cancel them (--cancel signs and
                               submits only with --yes or a terminal's confirmation; --dry-run stops before submitting)
                               (`kob recover --help`; `--amount-left` names the amount a partly filled order has left)

GLOBAL OPTIONS (`kob <command> --help` lists the ones a command takes)
  --testnet            the testnet-10 network, as --network testnet-10 (also --netsuffix=10); every command
  --network NET        mainnet (the default) or testnet-10; a deployment build is bound to its own network
  --indexer URL        the indexer API (default $KOB_INDEXER_URL, else the deployment record's); read and trade commands
  --node URL           the node's wRPC endpoint (default $KOB_NODE_WRPC, else the public nodes); trade, token, recover
  --allow-high-fee     pay a fee above the ceiling (4 times the node's fee estimate); every command that signs
  --key-file PATH      the file holding the signing key (64 hex), or
  --key-env NAME       the environment variable holding it (default $KOB_KEY); every command that signs
  --json               one JSON document (watch: one per line); read and trade commands
  --yes, --dry-run     sign and submit without asking; or stop before signing; every command that signs
  --help, -h           the help of a command; --version, -V prints the version

  The options, the key rule, the exit codes and the configuration: crates/kob-cli/README.md
";

/// Every library loaded from here on comes from System32 (or a full path), never from the folder of
/// kob.exe or the working directory, where another account might have planted one: this process
/// reads signing keys (R3D-4, as kobd). The imports loaded before `main` are restricted by the
/// linker flag of `build.rs`.
#[cfg(windows)]
fn system32_dlls_only() {
    #[link(name = "kernel32")]
    extern "system" {
        fn SetDefaultDllDirectories(flags: u32) -> i32;
    }
    const LOAD_LIBRARY_SEARCH_SYSTEM32: u32 = 0x800;
    // SAFETY: a flags-only call with no pointers; a failure leaves the default search order.
    unsafe {
        SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32);
    }
}

/// Help of `kob token`.
const TOKEN_USAGE: &str = "\
kob token: create a KCC-20 token, send a creation that did not go out, release a creator lock, check a launch proof

USAGE
  kob token create [OPTIONS]            create a fixed-supply KCC-20 token: KCC-23 declaration, then the genesis
  kob token resume <DIR>                send a creation whose genesis did not go out
  kob token unlock <TOKEN|DIR>          release a creator lock's tokens after its time (--lock-until)
  kob token verify-declaration <PROOF>  check a KCC-23 launch proof from any source and print the declaration

  `kob token <subcommand> --help` (or `kob help token <subcommand>`) prints the help of one of them.
";

/// Help of `kob registry`.
const REGISTRY_USAGE: &str = "\
kob registry: check a token registry, re-derive its genesis checks, list the token programs

USAGE
  kob registry validate <tokens.json> [--network NET]
                                        check a token registry file
  kob registry verify-genesis --network NET [--evidence FILE] [--registry FILE] [--json]
                                        re-derive each token's genesis check and live-minter check from chain evidence
  kob registry programs [--json]        the token-program profiles this build embeds

  `kob registry <subcommand> --help` (or `kob help registry <subcommand>`) prints the help of one of them.
";

/// The commands `kob help <command>` knows.
const COMMANDS: &[&str] = &[
    "token", "registry", "recover", "place", "replace", "cancel", "sweep", "refund", "claim", "take", "send", "merge", "book",
    "orders", "balances", "watch", "tokens",
];

/// The `kob --version` line, such as `kob 0.1.0 (testnet-10 build, commit a1b2c3d4e5f6)`.
///
/// A build without a deployment feature says `development build, no network`. The commit is
/// `KOB_GIT_COMMIT` at build time, which the release workflow sets. It is cut to 12 characters,
/// and is `unknown` when absent or blank, as in `kobd --version`.
fn version_line() -> String {
    version_line_of(kob_protocol::artifacts::deployment_network(), option_env!("KOB_GIT_COMMIT"))
}

fn version_line_of(network: Option<&str>, commit: Option<&str>) -> String {
    let build = match network {
        Some(n) => format!("{n} build"),
        None => "development build, no network".to_string(),
    };
    let commit = commit.map(str::trim).filter(|c| !c.is_empty()).map(|c| c.get(..12).unwrap_or(c)).unwrap_or("unknown");
    format!("kob {} ({build}, commit {commit})", kob_protocol::VERSION)
}

/// A subcommand that does not exist under a command family.
///
/// It prints the mistake on stderr, then the usage of the family, and returns exit code 2.
fn unknown_sub(family: &str, sub: &str, usage: &str) -> i32 {
    eprintln!("error: unknown `kob {family} {sub}`\n");
    help::print_usage_error(&[usage])
}

/// A removed spelling: a usage error (exit 2) that names its replacement.
fn removed(old: &str, new: &str) -> i32 {
    eprintln!("error: `{old}` was removed: use `{new}`");
    help::USAGE_EXIT
}

fn main() {
    #[cfg(windows)]
    system32_dlls_only();
    // `--testnet` and `--netsuffix=10`, as in kaspad, name testnet-10 for every command. Mainnet is
    // the default.
    let args = match common::network_switches(&std::env::args().skip(1).collect::<Vec<_>>()) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(2);
        }
    };
    std::process::exit(dispatch(&args));
}

/// Run the command that the arguments name and return the process exit code.
fn dispatch(args: &[String]) -> i32 {
    match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        // `kob help <command> [<sub>]` is `kob <command> [<sub>] --help`.
        ["help", first, ..] if COMMANDS.contains(first) => {
            let mut routed = args[1..].to_vec();
            routed.push("--help".to_string());
            dispatch(&routed)
        }
        ["help", other @ ..] if !other.is_empty() => {
            eprintln!("error: unknown command `{}`\n", other.join(" "));
            help::print_usage_error(&[USAGE])
        }
        ["token", "create", rest @ ..] => token::run_issue(&args[2..2 + rest.len()]),
        // Removed spellings are usage errors (exit 2) that name what replaces them.
        ["token", "issue", ..] => removed("kob token issue", "kob token create"),
        ["token", "resume", rest @ ..] => token::run_resume(&args[2..2 + rest.len()]),
        ["token", "unlock", rest @ ..] => token_lock::run_unlock(&args[2..2 + rest.len()]),
        ["token", "verify-declaration", rest @ ..] => token::run_verify_declaration(&args[2..2 + rest.len()]),
        ["token", rest @ ..] if rest.is_empty() || help::wants_help(&rest[..1]) => {
            help::help_or_usage(rest, &[TOKEN_USAGE], true).unwrap_or(2)
        }
        ["token", sub, ..] => unknown_sub("token", sub, TOKEN_USAGE),
        ["registry", "validate", rest @ ..] => registry::run_validate(rest),
        ["registry", "verify-genesis", rest @ ..] => registry::run_verify_genesis(rest),
        ["registry", "programs", rest @ ..] => programs::run_programs(rest),
        ["registry", "add-program", ..] => removed(
            "kob registry add-program",
            "kob-maint add-program (the maintainers' binary, from the source tree: cargo run -p kob-cli --bin kob-maint -- add-program ...)",
        ),
        ["registry", rest @ ..] if rest.is_empty() || help::wants_help(&rest[..1]) => {
            help::help_or_usage(rest, &[REGISTRY_USAGE], true).unwrap_or(2)
        }
        ["registry", sub, ..] => unknown_sub("registry", sub, REGISTRY_USAGE),
        ["recover", ..] => recover::run(&args[1..]),
        ["order", "sweep", ..] => removed("kob order sweep", "kob sweep --view FILE (or kob sweep ID..., kob sweep --all)"),
        ["order", "cancel", ..] => removed("kob order cancel", "kob cancel --view FILE (or kob cancel ID...)"),
        ["order", ..] => removed("kob order", "kob sweep or kob cancel (--view FILE takes saved indexer views)"),
        ["place", ..] => trade::place::run(&args[1..]),
        ["replace", ..] => trade::actions::run_replace(&args[1..]),
        ["cancel", ..] => trade::actions::run_cancel(&args[1..]),
        ["sweep", ..] => trade::actions::run_sweep(&args[1..]),
        ["refund", ..] => trade::actions::run_refund(&args[1..]),
        ["claim", ..] => trade::actions::run_claim(&args[1..]),
        ["take", ..] => trade::actions::run_take(&args[1..]),
        ["send", ..] => trade::actions::run_send(&args[1..]),
        ["merge", ..] => trade::merge::run(&args[1..]),
        ["book", ..] => trade::views::run_book(&args[1..]),
        ["orders", ..] => trade::views::run_orders(&args[1..]),
        ["balances", ..] => trade::views::run_balances(&args[1..]),
        ["watch", ..] => watch::run(&args[1..]),
        ["tokens", ..] => tokens::run(&args[1..]),
        ["--version"] | ["-V"] => {
            println!("{}", version_line());
            0
        }
        [] | ["--help"] | ["-h"] | ["help"] => help::print_help(&[USAGE]),
        other => {
            eprintln!("error: unknown command `{}`\n", other.join(" "));
            help::print_usage_error(&[USAGE])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_version_line_names_the_build_and_the_commit() {
        let v = kob_protocol::VERSION;
        assert_eq!(
            version_line_of(Some("testnet-10"), Some("a1b2c3d4e5f60718293a4b5c6d7e8f9012345678")),
            format!("kob {v} (testnet-10 build, commit a1b2c3d4e5f6)")
        );
        assert_eq!(version_line_of(None, None), format!("kob {v} (development build, no network, commit unknown)"));
        assert_eq!(version_line_of(Some("mainnet"), Some("  \n")), format!("kob {v} (mainnet build, commit unknown)"));
        assert_eq!(version_line_of(Some("mainnet"), Some(" abc123\n")), format!("kob {v} (mainnet build, commit abc123)"));
    }

    #[test]
    fn every_command_of_the_page_is_known_to_help() {
        for c in COMMANDS {
            assert!(USAGE.contains(&format!("kob {c} ")) || USAGE.contains(&format!("kob {c}\n")), "{c}");
        }
    }
}
