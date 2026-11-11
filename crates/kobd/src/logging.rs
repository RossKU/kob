//! The executor's log: the console, and optionally a size-rotated file (`--log-file`, `log_file` in the configuration).
//!
//! * One line per event, as kaspad prints them: the UTC time to the millisecond, the level, the message and its fields
//!   (`2026-11-11 08:44:28.529Z  INFO status: ...`). The module path of the event is shown only while `RUST_LOG` is set
//!   (a debugging session).
//! * No ANSI colour codes unless the console is a terminal (and `NO_COLOR` is unset); the file never has any.
//! * With a log file the console keeps showing the log (a terminal, a pipe, `journalctl`, `docker logs`): the file is a copy
//!   that is rotated, not a replacement. The start banner names the file.
//! * The file is rotated by size: when the next event would take it past `max_bytes` it becomes `<file>.1`, the older ones shift
//!   to `.2`, `.3`, ... and the one past `keep` is deleted, so the log takes at most `(keep + 1) * max_bytes`. A file that was
//!   there before the start is appended to.
//!
//! The subscriber is installed once, at the start ([`init`]); the file is attached when the configuration is known
//! ([`attach_file`]), which is after the first start's setup dialogue.

use std::fs::{File, OpenOptions};
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer};

/// Where the log goes and how it rotates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogOptions {
    pub file: PathBuf,
    /// Rotate at this size in bytes (0: never).
    pub max_bytes: u64,
    /// Rotated files kept.
    pub keep: usize,
}

impl LogOptions {
    /// `max_mb` and `keep` as the configuration and the command line give them.
    pub fn new(file: PathBuf, max_mb: u64, keep: usize) -> Self {
        LogOptions { file, max_bytes: max_mb.saturating_mul(1 << 20), keep }
    }
}

/// The log file, once attached.
static FILE: Mutex<Option<Rotating>> = Mutex::new(None);
/// Whether the console shows the log.
static CONSOLE: AtomicBool = AtomicBool::new(true);
/// The stream the log uses on the console.
static TO_STDERR: AtomicBool = AtomicBool::new(false);

/// A file that rotates by size.
struct Rotating {
    opts: LogOptions,
    file: Option<File>,
    size: u64,
}

impl Rotating {
    fn open(opts: LogOptions) -> std::io::Result<Self> {
        if let Some(parent) = opts.file.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new().create(true).append(true).open(&opts.file)?;
        let size = file.metadata().map(|m| m.len()).unwrap_or(0);
        Ok(Rotating { opts, file: Some(file), size })
    }

    /// `<file>.<n>`.
    fn numbered(&self, n: usize) -> PathBuf {
        let mut name = self.opts.file.clone().into_os_string();
        name.push(format!(".{n}"));
        PathBuf::from(name)
    }

    /// The full file becomes `.1`, the older ones shift up, the one past `keep` goes. Nothing is open while files move
    /// (Windows cannot rename an open file).
    fn rotate(&mut self) -> std::io::Result<()> {
        self.file = None;
        if self.opts.keep == 0 {
            let _ = std::fs::remove_file(&self.opts.file);
        } else {
            let _ = std::fs::remove_file(self.numbered(self.opts.keep));
            for n in (1..self.opts.keep).rev() {
                let from = self.numbered(n);
                if from.exists() {
                    let _ = std::fs::rename(&from, self.numbered(n + 1));
                }
            }
            let _ = std::fs::rename(&self.opts.file, self.numbered(1));
        }
        self.file = Some(OpenOptions::new().create(true).append(true).open(&self.opts.file)?);
        self.size = 0;
        Ok(())
    }

    fn write(&mut self, buf: &[u8]) -> std::io::Result<()> {
        if self.opts.max_bytes > 0 && self.size > 0 && self.size + buf.len() as u64 > self.opts.max_bytes {
            self.rotate()?;
        }
        match self.file.as_mut() {
            Some(f) => {
                f.write_all(buf)?;
                self.size += buf.len() as u64;
                Ok(())
            }
            None => Err(std::io::Error::other("the log file is closed")),
        }
    }
}

/// Opens `opts.file` for the log from now on (as a copy: the console keeps showing the log).
pub fn attach_file(opts: LogOptions) -> std::io::Result<()> {
    let f = Rotating::open(opts)?;
    *FILE.lock().unwrap_or_else(|e| e.into_inner()) = Some(f);
    Ok(())
}

/// Whether the console half of the log goes to stderr.
pub fn on_stderr() -> bool {
    TO_STDERR.load(Ordering::Relaxed)
}

/// Shows or hides the log on the console (the file, when attached, keeps it either way). The setup dialogue hides it: raw log
/// lines would interrupt the questions.
pub fn set_console(on: bool) {
    CONSOLE.store(on, Ordering::Relaxed);
}

/// Whether the console shows the log (false: only the file has it).
pub fn console_shows_log() -> bool {
    CONSOLE.load(Ordering::Relaxed)
}

fn console_is_terminal() -> bool {
    if TO_STDERR.load(Ordering::Relaxed) {
        std::io::stderr().is_terminal()
    } else {
        std::io::stdout().is_terminal()
    }
}

/// The console half of the log.
struct ConsoleSink;

impl Write for ConsoleSink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if !CONSOLE.load(Ordering::Relaxed) {
            return Ok(buf.len());
        }
        if TO_STDERR.load(Ordering::Relaxed) {
            std::io::stderr().write(buf)
        } else {
            std::io::stdout().write(buf)
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for ConsoleSink {
    type Writer = ConsoleSink;
    fn make_writer(&'a self) -> ConsoleSink {
        ConsoleSink
    }
}

/// The file half of the log: nothing until a file is attached.
struct FileSink;

impl Write for FileSink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if let Some(f) = FILE.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
            // a full disk or a vanished directory must not take the executor down: the event is lost to the file
            let _ = f.write(buf);
        }
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for FileSink {
    type Writer = FileSink;
    fn make_writer(&'a self) -> FileSink {
        FileSink
    }
}

/// The time of a log line: UTC to the millisecond (`2026-11-11 08:44:28.529Z`).
struct UtcMillis;

impl tracing_subscriber::fmt::time::FormatTime for UtcMillis {
    fn format_time(&self, w: &mut tracing_subscriber::fmt::format::Writer<'_>) -> std::fmt::Result {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
        w.write_str(&utc_millis_text(now.as_millis() as u64))
    }
}

/// `ms` (milliseconds since the Unix epoch) as `YYYY-MM-DD hh:mm:ss.mmmZ`.
pub fn utc_millis_text(ms: u64) -> String {
    let (secs, milli) = (ms / 1000, ms % 1000);
    let (days, rem) = (secs / 86_400, secs % 86_400);
    // the civil date of a day count (Howard Hinnant's days_from_civil, inverted)
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}.{milli:03}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

/// Installs the subscriber: `RUST_LOG` (default `info`), the console on stdout (`to_stderr`: stderr, for the commands that
/// keep stdout for their output), ANSI colour only on a terminal, kaspad's line shape (the module path only under `RUST_LOG`).
pub fn init(to_stderr: bool) {
    TO_STDERR.store(to_stderr, Ordering::Relaxed);
    let debugging = std::env::var_os("RUST_LOG").is_some();
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let ansi = console_is_terminal() && std::env::var_os("NO_COLOR").is_none();
    let console =
        tracing_subscriber::fmt::layer().with_writer(ConsoleSink).with_ansi(ansi).with_timer(UtcMillis).with_target(debugging);
    let file = tracing_subscriber::fmt::layer().with_writer(FileSink).with_ansi(false).with_timer(UtcMillis).with_target(debugging);
    let _ = tracing_subscriber::registry().with(console.and_then(file)).with(filter).try_init();
}

/// `path` against `base` when it is relative.
pub fn resolve(base: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(dir: &Path, max: u64, keep: usize) -> LogOptions {
        LogOptions { file: dir.join("x.log"), max_bytes: max, keep }
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> =
            std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        v.sort();
        v
    }

    #[test]
    fn the_file_rotates_by_size_and_keeps_a_few() {
        let d = tempfile::tempdir().unwrap();
        let sub = d.path().join("sub");
        let mut r = Rotating::open(opts(&sub, 100, 2)).unwrap();
        for i in 0..10u8 {
            r.write(&[b'a' + i; 40]).unwrap();
        }
        // two events per file: the newest in x.log, then x.log.1 and x.log.2; the older ones are gone
        assert_eq!(names(&sub), ["x.log", "x.log.1", "x.log.2"]);
        let read = |n: &str| std::fs::read(sub.join(n)).unwrap();
        assert_eq!(read("x.log"), [[b'i'; 40], [b'j'; 40]].concat());
        assert_eq!(read("x.log.1"), [[b'g'; 40], [b'h'; 40]].concat());
        assert_eq!(read("x.log.2"), [[b'e'; 40], [b'f'; 40]].concat());
    }

    #[test]
    fn keep_zero_starts_over_and_zero_size_never_rotates() {
        let d = tempfile::tempdir().unwrap();
        let mut r = Rotating::open(opts(d.path(), 50, 0)).unwrap();
        for _ in 0..5 {
            r.write(&[b'z'; 30]).unwrap();
        }
        assert_eq!(names(d.path()), ["x.log"]);
        assert_eq!(std::fs::metadata(d.path().join("x.log")).unwrap().len(), 30);
        let e = tempfile::tempdir().unwrap();
        let mut r = Rotating::open(opts(e.path(), 0, 3)).unwrap();
        for _ in 0..5 {
            r.write(&[b'z'; 1000]).unwrap();
        }
        assert_eq!(names(e.path()), ["x.log"]);
        assert_eq!(std::fs::metadata(e.path().join("x.log")).unwrap().len(), 5000);
    }

    #[test]
    fn the_time_of_a_line_is_utc_to_the_millisecond() {
        assert_eq!(utc_millis_text(0), "1970-01-01 00:00:00.000Z");
        // 2026-11-11 08:44:28.529 UTC
        assert_eq!(utc_millis_text(1_794_386_668_529), "2026-11-11 08:44:28.529Z");
        // a leap day
        assert_eq!(utc_millis_text(951_782_400_000), "2000-02-29 00:00:00.000Z");
    }

    #[test]
    fn an_existing_file_is_appended_to_and_counted() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("x.log"), [b'o'; 80]).unwrap();
        let mut r = Rotating::open(opts(d.path(), 100, 1)).unwrap();
        r.write(&[b'n'; 30]).unwrap();
        assert_eq!(names(d.path()), ["x.log", "x.log.1"]);
        assert_eq!(std::fs::read(d.path().join("x.log.1")).unwrap(), [b'o'; 80]);
        assert_eq!(std::fs::read(d.path().join("x.log")).unwrap(), [b'n'; 30]);
    }
}
