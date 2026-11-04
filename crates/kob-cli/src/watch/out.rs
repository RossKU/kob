//! Where `kob watch` writes: events on stdout (one line each; with `--json`, one JSON document per line), notes and errors on
//! stderr (with `--json`, notes are `{"type":"note"}` lines on stdout so a pipe sees the gaps). A write that fails (a closed
//! pipe) marks the output broken and the watch ends.

use std::io::Write;

use serde_json::{json, Value};

use super::frames::clock;

/// The output of a watch.
pub struct Out {
    json: bool,
    sink: Box<dyn Write + Send>,
    err: Box<dyn Write + Send>,
    now: fn() -> u64,
    /// A write failed: the reader is gone.
    pub broken: bool,
}

fn unix_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

impl Out {
    /// Standard output and error.
    pub fn stdio(json: bool) -> Out {
        Out::with(json, Box::new(std::io::stdout()), Box::new(std::io::stderr()), unix_now)
    }

    /// Any sinks and clock (tests).
    pub fn with(json: bool, sink: Box<dyn Write + Send>, err: Box<dyn Write + Send>, now: fn() -> u64) -> Out {
        Out { json, sink, err, now, broken: false }
    }

    fn put(&mut self, line: &str) {
        if writeln!(self.sink, "{line}").and_then(|_| self.sink.flush()).is_err() {
            self.broken = true;
        }
    }

    fn put_err(&mut self, line: &str) {
        // stderr failing (closed) is not a reason to stop
        let _ = writeln!(self.err, "{line}").and_then(|_| self.err.flush());
    }

    /// An event: the human line with the time, or the frame as one JSON line.
    pub fn event(&mut self, human: &str, frame: &Value) {
        if self.json {
            self.put(&frame.to_string());
        } else {
            let t = clock((self.now)());
            self.put(&format!("{t}  {human}"));
        }
    }

    /// An event of several lines (a book): the time heads the first.
    pub fn block(&mut self, human: &[String], frame: &Value) {
        if self.json {
            self.put(&frame.to_string());
        } else {
            let t = clock((self.now)());
            for (i, l) in human.iter().enumerate() {
                if i == 0 {
                    self.put(&format!("{t}  {l}"));
                } else {
                    self.put(l);
                }
            }
        }
    }

    /// A frame that is shown in `--json` only (a `book` or `order` notice: the refetch is what a person reads).
    pub fn json_only(&mut self, frame: &Value) {
        if self.json {
            self.put(&frame.to_string());
        }
    }

    /// Something the reader should know about the watch itself (a reconnect, a resync, a reorg).
    pub fn note(&mut self, msg: &str) {
        if self.json {
            let line = json!({"type": "note", "data": {"message": msg}}).to_string();
            self.put(&line);
        } else {
            let t = clock((self.now)());
            self.put_err(&format!("{t}  note: {msg}"));
        }
    }

    /// A note that is also a server frame: `--json` prints the frame itself.
    pub fn frame_note(&mut self, msg: &str, frame: &Value) {
        if self.json {
            self.put(&frame.to_string());
        } else {
            self.note(msg);
        }
    }

    /// An error: always on stderr.
    pub fn error(&mut self, msg: &str) {
        self.put_err(&format!("error: {msg}"));
    }
}
