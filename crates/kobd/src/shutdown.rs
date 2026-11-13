//! Stopping the process: what asks for it, and how long it may take.
//!
//! [`shutdown_signal`] resolves when the operator asks the process to stop. The requests are:
//!
//! - Ctrl+C,
//! - SIGTERM (a service manager, `kill`, `docker stop`),
//! - on Windows also Ctrl+Break and the console's close, logoff and shutdown events (closing the
//!   window of a double-clicked executor).
//!
//! The listeners are installed when the function is first called. The commands call it before
//! anything slow starts, and a task of its own watches the listeners. So a request is never lost
//! while the command is still starting. An unhandled Ctrl+C would kill the process in the middle of
//! whatever it was doing.
//!
//! The first request starts the clean stop. Every command races its work against the future this
//! returns. It also arms a hard limit. A second request exits at once. A stop that has not finished
//! in [`HARD_STOP`] exits too, with code 2 and a message on stderr.
//!
//! The database is crash safe either way (SQLite in WAL mode, one transaction per batch). The clean
//! stop only leaves it checkpointed.

use std::future::Future;
use std::sync::OnceLock;
use std::time::Duration;
use tokio::sync::watch;

/// The longest a clean stop may take once it was asked for.
///
/// Everything a stop waits for is cut short much sooner. That covers a request to the node, a step
/// of the matcher and the read API's open requests. This limit is the net under whatever is not.
pub const HARD_STOP: Duration = Duration::from_secs(10);

/// What a request to stop is called in the log.
type Kind = &'static str;

#[cfg(unix)]
struct Signals {
    int: tokio::signal::unix::Signal,
    term: tokio::signal::unix::Signal,
}

#[cfg(unix)]
impl Signals {
    fn new() -> std::io::Result<Signals> {
        use tokio::signal::unix::{signal, SignalKind};
        Ok(Signals { int: signal(SignalKind::interrupt())?, term: signal(SignalKind::terminate())? })
    }

    async fn recv(&mut self) -> Kind {
        tokio::select! {
            _ = self.int.recv() => "interrupt (Ctrl+C)",
            _ = self.term.recv() => "terminate (SIGTERM)",
        }
    }
}

#[cfg(windows)]
struct Signals {
    c: tokio::signal::windows::CtrlC,
    brk: tokio::signal::windows::CtrlBreak,
    close: tokio::signal::windows::CtrlClose,
    logoff: tokio::signal::windows::CtrlLogoff,
    shutdown: tokio::signal::windows::CtrlShutdown,
}

#[cfg(windows)]
impl Signals {
    fn new() -> std::io::Result<Signals> {
        use tokio::signal::windows::{ctrl_break, ctrl_c, ctrl_close, ctrl_logoff, ctrl_shutdown};
        Ok(Signals { c: ctrl_c()?, brk: ctrl_break()?, close: ctrl_close()?, logoff: ctrl_logoff()?, shutdown: ctrl_shutdown()? })
    }

    async fn recv(&mut self) -> Kind {
        tokio::select! {
            _ = self.c.recv() => "Ctrl+C",
            _ = self.brk.recv() => "Ctrl+Break",
            _ = self.close.recv() => "console close",
            _ = self.logoff.recv() => "logoff",
            _ = self.shutdown.recv() => "system shutdown",
        }
    }
}

/// The first request to stop, process wide.
fn requested() -> &'static watch::Sender<bool> {
    static REQUESTED: OnceLock<watch::Sender<bool>> = OnceLock::new();
    REQUESTED.get_or_init(|| {
        let (tx, _) = watch::channel(false);
        match Signals::new() {
            Ok(signals) => {
                tokio::spawn(watch_signals(signals, tx.clone(), HARD_STOP));
            }
            // Without listeners the platform's own handling stays in place. Ctrl+C then ends the
            // process at once.
            Err(e) => tracing::error!(error = %e, "cannot listen for Ctrl+C / SIGTERM: the process can only be killed"),
        }
        tx
    })
}

/// Waits for the first request and tells the commands. Then holds the stop to its limit.
///
/// A second request exits at once with code 130.
async fn watch_signals(mut signals: Signals, requested: watch::Sender<bool>, limit: Duration) {
    let kind = signals.recv().await;
    tracing::info!("{kind}: shutting down (press it again to exit at once)");
    let _ = requested.send(true);
    // A plain thread, so it fires even when the runtime is what is stuck.
    std::thread::spawn(move || {
        std::thread::sleep(limit);
        eprintln!("shutdown did not finish in {} s: exiting now", limit.as_secs());
        std::process::exit(2);
    });
    let again = signals.recv().await;
    eprintln!("{again} again: exiting now");
    std::process::exit(130);
}

/// Resolves when the process is asked to stop (see the module documentation).
///
/// It installs the listeners on the first call. A command calls it before it starts anything slow
/// and hands the future to the part that runs.
pub fn shutdown_signal() -> impl Future<Output = ()> + Send + 'static {
    let mut rx = requested().subscribe();
    async move {
        // `wait_for` only fails when the sender is gone. It never is, because it lives in a static.
        let _ = rx.wait_for(|r| *r).await;
    }
}
