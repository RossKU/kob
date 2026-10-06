//! `kob` command line.
//!
//! Implemented: `kob token issue` (fixed-supply KCC-20 issuance), `kob registry validate` / `verify-genesis`, `kob recover` (find the maker's
//! orders on the node from a backup, a recovery / export file or an indexer view, and cancel them with `--cancel`), and
//! `kob order sweep|cancel` (the maker's stray sweep in place and the maker's cancel from an indexer order view). Planned:
//! place, replace, take, refund, send.

mod common;
mod inputs;
mod order;
mod recover;
mod registry;
mod token;
mod wrpc;

#[cfg(test)]
mod tests_e2e;

const USAGE: &str = "\
kob: KOB command line

USAGE
  kob token issue [OPTIONS]    issue a fixed-supply KCC-20 token (`kob token issue --help`)
  kob registry validate <tokens.json> [--network NET]   check a token registry file
  kob registry verify-genesis --network NET [--evidence FILE] [--registry FILE] [--json]
                               re-derive each token's genesis check and live-minter check from chain evidence
  kob recover --from <FILE>... --node ws://HOST:PORT [--amount-left <AMOUNT>] [--cancel --key <KEY> ...]
                               find orders on the node from a backup / export / indexer view; cancel them
                               (`kob recover --help`; `--amount-left` names the amount a partly filled order has left)
  kob order sweep  --view <FILE>... --key <KEY> --node ws://HOST:PORT [...]
                               return an order's strays to the maker, the order continues (`kob order --help`)
  kob order cancel --view <FILE>... --key <KEY> --node ws://HOST:PORT [...]
                               the maker's cancel with custody and strays from an indexer view (older contract versions too)
  kob --version
";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["token", "issue", rest @ ..] => token::run_issue(&args[2..2 + rest.len()]),
        ["registry", "validate", rest @ ..] => registry::run_validate(rest),
        ["registry", "verify-genesis", rest @ ..] => registry::run_verify_genesis(rest),
        ["recover", ..] => recover::run(&args[1..]),
        ["order", ..] => order::run(&args[1..]),
        ["--version"] | ["-V"] => {
            println!("kob {}", kob_protocol::VERSION);
            0
        }
        [] | ["--help"] | ["-h"] | ["help"] => {
            print!("{USAGE}");
            0
        }
        other => {
            eprintln!("unknown command `{}`\n\n{USAGE}", other.join(" "));
            2
        }
    };
    std::process::exit(code);
}
