//! `kob watch`: follows the WebSocket feed of the executor from a terminal or a pipe.
//!
//! The feed is described in `docs/api-reference.md`, section 4. The command is read-only: no key
//! is read and nothing is signed. The WebSocket address is the indexer address (`--indexer` or
//! `$KOB_INDEXER_URL`) with `http` turned into `ws` and `/v1/ws` appended.

mod frames;
mod names;
mod out;
mod session;
mod url;

#[cfg(test)]
mod tests;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use kob_protocol::registry::parse_hex32;

use crate::trade::opts::{parse, Common, Parsed, COMMON_HELP};
use names::Names;
use out::Out;
use session::{Kind, Timing, Watch};

/// The help text of `kob watch`.
pub const USAGE: &str = "\
kob watch: follow the executor's live feed (read-only; no key, nothing is signed)

USAGE
  kob watch fills [--token TOKEN]      every fill, or the fills of one token
  kob watch book TOKEN [--quote B] [--depth N]
                                       the token's book (a pair's with --quote), printed again whenever it may have changed
  kob watch order COVENANT_ID...       the state of each order, printed again whenever it changes
  kob watch health                     the indexer's progress (a line per published batch and a health line every 15 s)

  TOKEN is a ticker or a covenant id. Every watch also reports chain reorganisations. One line per event: the time (UTC), the
  kind, the amount and price in the token's decimals, short ids, the transaction id. With --json, one JSON document per line
  (NDJSON): every frame the server sends as it is, `snapshot` lines for what a notice made it read again, and `note` lines for
  reconnects. Notes and errors otherwise go to stderr.

  The feed is a notice service: a `book` or `order` frame carries no content, so the CLI reads the book or the order again
  over REST (at most every 2 s). When the connection drops it reconnects with a growing pause (1 s up to 30 s), subscribes
  again and reads everything again; fills during the gap are not replayed. Ctrl-C ends it.

  The address is derived from --indexer (default $KOB_INDEXER_URL, else the default indexer in the network's deployment record): http://host:port
  becomes ws://host:port/v1/ws, https:// becomes wss://.
";

/// The options of the shared set that `kob watch` refuses.
///
/// It signs nothing and needs no node.
const REFUSED: &[&str] = &["--yes", "--dry-run", "--key-file", "--key-env", "--fee-rate", "--priority-fee", "--out-dir", "--node"];

fn bail(e: &str, code: i32) -> i32 {
    eprintln!("error: {e}");
    code
}

/// Runs `kob watch`.
pub fn run(args: &[String]) -> i32 {
    if let Some(code) = crate::help::help_or_usage(args, &[USAGE, COMMON_HELP], true) {
        return code;
    }
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    // A second handler is not an error worth stopping for. It happens in tests.
    let _ = ctrlc::set_handler(move || flag.store(true, Ordering::Relaxed));
    let (mut watch, json) = match setup(args) {
        Ok(x) => x,
        Err((e, code)) => return bail(&e, code),
    };
    let mut out = Out::stdio(json);
    watch.run(&stop, &mut out)
}

/// The watch that the arguments describe.
///
/// Its lists are read now, so an unknown token is refused before anything connects.
fn setup(args: &[String]) -> Result<(Watch, bool), (String, i32)> {
    let usage = |e: String| (e, 2);
    let (what, rest) = args.split_first().ok_or_else(|| usage("give what to watch: fills, book, order or health".into()))?;
    let (value_flags, bool_flags): (&[&str], &[&str]) = match what.as_str() {
        "fills" => (&["--token"], &[]),
        "book" => (&["--quote", "--depth"], &[]),
        "order" | "health" => (&[], &[]),
        other => return Err(usage(format!("unknown watch `{other}`: fills, book, order or health (`kob watch --help`)"))),
    };
    let p = parse(rest, value_flags, bool_flags).map_err(usage)?;
    for f in REFUSED {
        if p.has(f) || p.vals.contains_key(*f) {
            return Err(usage(format!("{f} does not apply to `kob watch`: it signs nothing and reads no node")));
        }
    }
    let c = Common::from(&p).map_err(usage)?;
    let indexer = c.need_indexer().map_err(usage)?.to_string();
    let target = url::ws_target(&indexer).map_err(usage)?;
    let fail = |e: String| (e, 1);
    let names = Names::load(&indexer, &c.network, c.registry.clone()).map_err(fail)?;
    let json = c.json;
    let kind = match what.as_str() {
        "fills" => {
            if let Some(x) = p.pos.first() {
                return Err(usage(format!("unexpected argument `{x}`")));
            }
            Kind::Fills(p.get("--token").map(|t| names.find(t)).transpose().map_err(fail)?)
        }
        "book" => {
            let [t] = p.pos.as_slice() else { return Err(usage("give exactly one token (ticker or covenant id)".into())) };
            let a = names.find(t).map_err(fail)?;
            let b = p.get("--quote").map(|q| names.find(q)).transpose().map_err(fail)?;
            if b == Some(a) {
                return Err(usage("--quote names the token itself".into()));
            }
            Kind::Book(a, b)
        }
        "order" => {
            if p.pos.is_empty() {
                return Err(usage("give at least one order covenant id".into()));
            }
            let mut ids = vec![];
            for x in &p.pos {
                let id = parse_hex32(x).ok_or_else(|| usage(format!("`{x}` is not an order covenant id (64 hex characters)")))?;
                if !ids.contains(&id) {
                    ids.push(id);
                }
            }
            if ids.len() > MAX_ORDERS {
                return Err(usage(format!("at most {MAX_ORDERS} orders per watch (the server's subscription cap is 64 channels)")));
            }
            Kind::Orders(ids)
        }
        _ => {
            if let Some(x) = p.pos.first() {
                return Err(usage(format!("unexpected argument `{x}`")));
            }
            Kind::Health
        }
    };
    // The arguments that `kob book` reads on each refetch: the token as typed, because the lists
    // resolve it again, and --quote and --depth.
    let parsed = Parsed { pos: p.pos.clone(), vals: p.vals.clone(), flags: Default::default() };
    Ok((Watch { target, indexer, kind, common: c, parsed, names, timing: Timing::default() }, json))
}

/// The most orders that one watch follows, one channel each.
///
/// It stays below the cap of 64 channels of the server, with room for `reorg` and for the
/// margin of a reconnect.
const MAX_ORDERS: usize = 60;
