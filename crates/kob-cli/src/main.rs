//! `kob` command line.
//!
//! Implemented: `kob token issue` (fixed-supply KCC-20 issuance), `kob registry validate` / `verify-genesis` / `programs` / `add-program`, `kob recover` (find the maker's
//! orders on the node from a backup, a recovery / export file or an indexer view, and cancel them with `--cancel`), and
//! `kob order sweep|cancel` (the maker's stray sweep in place and the maker's cancel from an indexer order view), the
//! trading commands `kob place`, `kob replace`, `kob cancel`, `kob refund`, `kob take`, `kob send` and `kob merge`, and the
//! views `kob book`, `kob orders` and `kob balances` ([`trade`]).

mod common;
mod inputs;
mod order;
mod programs;
mod recover;
mod registry;
mod token;
mod trade;
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
  kob registry programs [--json]   the token-program profiles this build embeds
  kob registry add-program <ARTIFACT|REDEEM> [--name N] [--slots I/O] [--write] [...]
                               derive a new token program's profile from its bytes (`kob registry add-program --help`)
  kob recover --from <FILE>... --node ws://HOST:PORT [--amount-left <AMOUNT>] [--cancel --key <KEY> ...]
                               find orders on the node from a backup / export / indexer view; cancel them
                               (`kob recover --help`; `--amount-left` names the amount a partly filled order has left)
  kob order sweep  --view <FILE>... --key <KEY> --node ws://HOST:PORT [...]
                               return an order's strays to the maker, the order continues (`kob order --help`)
  kob order cancel --view <FILE>... --key <KEY> --node ws://HOST:PORT [...]
                               the maker's cancel with custody and strays from an indexer view (older contract versions too)
  kob place <KIND> --token T [--quote B] --side buy|sell --amount A [...]
                               place an order as the web wallet plans it: limit, ioc, fok, market, streaming, close,
                               twap, dca, dutch, stop-market, stop-limit, trailing-stop, take-profit, oco, ifd, ifo,
                               repeat-ifd, repeat-ifo; every kind on a pair with --quote (`kob place --help`)
  kob replace <COVENANT_ID> [--price P] [--amount A] [--tip T] [--stop P] [--renew] [...]
                               amend a live order in place, or cancel-replace it in one transaction (trailing stops,
                               if-done entries and pair orders are re-planned from their terms)
  kob cancel <COVENANT_ID>...  the maker's cancel by covenant id (indexer view, checked on the node)
  kob refund <COVENANT_ID>     the permissionless refund of an ended order (the sender keeps the refund tip)
  kob take <COVENANT_ID> --amount A
                               fill a resting order as the taker
  kob send <TOKEN|KAS> <TO> <AMOUNT>
                               send a token (within its program's slots) or KAS
  kob merge <TOKEN>            merge the key's own UTXOs of a token into one (a chain of transfers to itself)
  kob book <TOKEN> [--quote B] the order book (KAS or a pair), from the indexer
  kob orders [--token T] [--all]
                               the maker's orders (--maker KEY|ADDRESS, else the signing key's public key)
  kob balances                 free KAS and tokens, and what the maker's live orders hold (custody, strays, KAS)
                               the trading commands read the key from --key-file or $KOB_KEY (never --key), show the
                               plan and sign only with --yes or a terminal's confirmation; --dry-run stops before signing
  kob --version
";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["token", "issue", rest @ ..] => token::run_issue(&args[2..2 + rest.len()]),
        ["registry", "validate", rest @ ..] => registry::run_validate(rest),
        ["registry", "verify-genesis", rest @ ..] => registry::run_verify_genesis(rest),
        ["registry", "programs", rest @ ..] => programs::run_programs(rest),
        ["registry", "add-program", rest @ ..] => programs::run_add_program(rest),
        ["recover", ..] => recover::run(&args[1..]),
        ["order", ..] => order::run(&args[1..]),
        ["place", ..] => trade::place::run(&args[1..]),
        ["replace", ..] => trade::actions::run_replace(&args[1..]),
        ["cancel", ..] => trade::actions::run_cancel(&args[1..]),
        ["refund", ..] => trade::actions::run_refund(&args[1..]),
        ["take", ..] => trade::actions::run_take(&args[1..]),
        ["send", ..] => trade::actions::run_send(&args[1..]),
        ["merge", ..] => trade::merge::run(&args[1..]),
        ["book", ..] => trade::views::run_book(&args[1..]),
        ["orders", ..] => trade::views::run_orders(&args[1..]),
        ["balances", ..] => trade::views::run_balances(&args[1..]),
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
