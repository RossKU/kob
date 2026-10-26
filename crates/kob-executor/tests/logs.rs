//! The log of the real binary: `--log-file` writes the log to a file, a console that is not a terminal gets no ANSI colour
//! codes (and nothing at all once a log file takes the log), and every start names the funding address.

use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// A node that takes connections and says nothing (the executor then sits in its first step, logging).
fn silent_node() -> (u16, std::thread::JoinHandle<()>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let h = std::thread::spawn(move || {
        let mut held = Vec::new();
        for s in listener.incoming().flatten() {
            held.push(s);
        }
    });
    (port, h)
}

fn run(dir: &std::path::Path, port: u16, extra: &[&str]) -> Child {
    let key = dir.join("operator.key");
    if !key.exists() {
        // a throwaway key, never funded (readable by its owner only: the executor refuses a key file others can read)
        kob_executor::matcher::wallet::HotKey::create_file(&key).unwrap();
    }
    Command::new(env!("CARGO_BIN_EXE_kob-executor"))
        .args(["run", "--no-fast-start", "--no-api", "--network", "testnet-10", "--rpc-url"])
        .arg(format!("ws://127.0.0.1:{port}"))
        .arg("--data-dir")
        .arg(dir.join("data"))
        .arg("--key-file")
        .arg(&key)
        .args(extra)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

fn wait_for(what: &str, secs: u64, mut ok: impl FnMut() -> bool) {
    let t0 = Instant::now();
    while !ok() {
        assert!(t0.elapsed() < Duration::from_secs(secs), "timed out: {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn drain(mut child: Child) -> (String, String) {
    let _ = child.kill();
    let _ = child.wait();
    let (mut out, mut err) = (String::new(), String::new());
    child.stdout.take().unwrap().read_to_string(&mut out).unwrap();
    child.stderr.take().unwrap().read_to_string(&mut err).unwrap();
    (out, err)
}

#[test]
fn a_log_file_takes_the_log_of_a_console_that_is_not_a_terminal_without_colour_codes() {
    let (port, _node) = silent_node();
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("logs").join("executor.log");
    let child = run(dir.path(), port, &["--log-file", log.to_str().unwrap()]);
    wait_for("the start line in the log file", 60, || {
        std::fs::read_to_string(&log).is_ok_and(|t| t.contains("starting the executor"))
    });
    let text = std::fs::read_to_string(&log).unwrap();
    // the funding address is in the first line of every start
    let address =
        kob_executor::matcher::wallet::HotKey::from_file(&dir.path().join("operator.key")).unwrap().address("testnet-10").to_string();
    assert!(text.contains(&format!("funding_address={address}")), "{text}");
    assert!(address.starts_with("kaspatest:"));
    assert!(!text.contains('\u{1b}'), "no ANSI colour codes in the file: {text:?}");
    let (out, err) = drain(child);
    // the console is a pipe: with a log file it shows nothing, and never colour codes
    assert!(!out.contains("starting the executor") && !out.contains('\u{1b}'), "stdout: {out:?}");
    assert!(!err.contains('\u{1b}'), "stderr: {err:?}");
}

#[test]
fn without_a_log_file_the_log_stays_on_the_console_without_colour_codes() {
    let (port, _node) = silent_node();
    let dir = tempfile::tempdir().unwrap();
    let mut child = run(dir.path(), port, &[]);
    let mut out = child.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        while let Ok(n) = out.read(&mut buf) {
            if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                break;
            }
        }
    });
    let mut seen = String::new();
    let t0 = Instant::now();
    while !seen.contains("starting the executor") {
        assert!(t0.elapsed() < Duration::from_secs(60), "no start line: {seen:?}");
        if let Ok(b) = rx.recv_timeout(Duration::from_millis(200)) {
            seen.push_str(&String::from_utf8_lossy(&b));
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    assert!(seen.contains("funding_address=kaspatest:"), "{seen}");
    assert!(!seen.contains('\u{1b}'), "no ANSI colour codes on a pipe: {seen:?}");
}

#[test]
fn a_log_file_that_cannot_be_opened_stops_the_start_with_its_name() {
    let (port, _node) = silent_node();
    let dir = tempfile::tempdir().unwrap();
    // a directory in the place of the file
    let bad = dir.path().join("taken");
    std::fs::create_dir(&bad).unwrap();
    let child = run(dir.path(), port, &["--log-file", bad.to_str().unwrap()]);
    let out = child.wait_with_output().unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("cannot open the log file"), "{err}");
}

#[test]
fn withdraw_without_a_configuration_or_with_a_bad_amount_sends_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let to = kob_executor::matcher::wallet::address_of(&[5; 32], "testnet-10").to_string();
    let bin = env!("CARGO_BIN_EXE_kob-executor");
    let o = Command::new(bin)
        .args(["withdraw", "--to", &to, "--amount", "all", "--yes", "--dir"])
        .arg(dir.path())
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("kob-executor.toml"), "{}", String::from_utf8_lossy(&o.stderr));
    let o = Command::new(bin)
        .args(["withdraw", "--to", &to, "--amount", "lots", "--yes", "--dir"])
        .arg(dir.path())
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!o.status.success());
    assert!(String::from_utf8_lossy(&o.stderr).contains("--amount"), "{}", String::from_utf8_lossy(&o.stderr));
}
