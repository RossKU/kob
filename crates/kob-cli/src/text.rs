//! Text from outside on the terminal (R8F3c-2).
//!
//! Tickers, names and token metadata from the indexer or the chain, memo and payload text, error
//! messages a server returns and file contents echoed back can hold carriage returns, line breaks,
//! escape sequences or bidirectional overrides that rewrite what the terminal shows, the signing
//! screen included. [`clean`] is the one sanitiser: such a string passes through it where it comes
//! in. And every `print!`, `println!`, `eprint!` and `eprintln!` of the crate goes through
//! [`clean_out`] (the macros of `main.rs` shadow the standard ones), so no line reaches the terminal
//! with a character that moves the cursor, erases or reorders text.

/// The most characters of one string from outside that are shown.
pub const MAX_LEN: usize = 160;

/// A character shown escaped: the control characters (C0 with CR, LF and ESC, DEL, C1), the line
/// and paragraph separators, and the bidirectional and other invisible format characters.
fn hidden(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{00AD}'
                | '\u{034F}'
                | '\u{061C}'
                | '\u{115F}'
                | '\u{1160}'
                | '\u{17B4}'
                | '\u{17B5}'
                | '\u{180B}'..='\u{180F}'
                | '\u{200B}'..='\u{200F}'
                | '\u{2028}'..='\u{202E}'
                | '\u{2060}'..='\u{206F}'
                | '\u{3164}'
                | '\u{FE00}'..='\u{FE0F}'
                | '\u{FEFF}'
                | '\u{FFA0}'
                | '\u{FFF9}'..='\u{FFFB}'
                | '\u{1D173}'..='\u{1D17A}'
                | '\u{E0000}'..='\u{E0FFF}'
        )
}

fn escape(s: &str, keep_breaks: bool, max: Option<usize>) -> String {
    let mut out = String::with_capacity(s.len());
    for (n, c) in s.chars().enumerate() {
        if max.is_some_and(|m| n >= m) {
            out.push('…');
            break;
        }
        if keep_breaks && (c == '\n' || c == '\t') {
            out.push(c);
        } else if hidden(c) {
            // JSON's own escape (UTF-16, a surrogate pair above U+FFFF): visible on a terminal, and
            // inside a JSON string it decodes back to the very character, so a JSON document printed
            // through this stays valid and its strings exact (R8DF-3)
            let mut units = [0u16; 2];
            for u in c.encode_utf16(&mut units) {
                out.push_str(&format!("\\u{u:04x}"));
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// A string from outside, made safe to show: every hidden character as `\u{..}`, cut at
/// [`MAX_LEN`] characters.
pub fn clean(s: &str) -> String {
    escape(s, false, Some(MAX_LEN))
}

/// The same for a longer text from outside (a server's error message), cut at `max` characters.
pub fn clean_long(s: &str, max: usize) -> String {
    escape(s, false, Some(max))
}

#[cfg(test)]
thread_local! {
    static CAPTURE: std::cell::RefCell<Option<(String, String)>> = const { std::cell::RefCell::new(None) };
}

/// Runs `f` and returns what it printed on this thread, stdout and stderr together (tests).
#[cfg(test)]
pub fn capture<T>(f: impl FnOnce() -> T) -> (T, String) {
    let (r, out, err) = capture_split(f);
    (r, out + &err)
}

/// [`capture`] with stdout and stderr apart (tests).
#[cfg(test)]
pub fn capture_split<T>(f: impl FnOnce() -> T) -> (T, String, String) {
    CAPTURE.with(|c| *c.borrow_mut() = Some((String::new(), String::new())));
    let r = f();
    let (out, err) = CAPTURE.with(|c| c.borrow_mut().take().unwrap_or_default());
    (r, out, err)
}

/// The one way the crate prints (the macros of `main.rs`): `s` through [`clean_out`], to stderr
/// with `err`, with a line break with `line`.
pub fn emit(err: bool, line: bool, s: &str) {
    let s = clean_out(s);
    #[cfg(test)]
    CAPTURE.with(|c| {
        if let Some((o, e)) = c.borrow_mut().as_mut() {
            let b = if err { e } else { o };
            b.push_str(&s);
            if line {
                b.push('\n');
            }
        }
    });
    match (err, line) {
        (false, true) => std::println!("{s}"),
        (false, false) => std::print!("{s}"),
        (true, true) => std::eprintln!("{s}"),
        (true, false) => std::eprint!("{s}"),
    }
}

/// What every print of the crate shows: the CLI's own line breaks and tabs stay, every other
/// hidden character is escaped. It is idempotent, so text already cleaned is unchanged.
///
/// In a JSON document a raw line break or tab can only stand between tokens (the encoder escapes
/// them in strings), and the escape is JSON's `\uXXXX`, so a document passes through unchanged in
/// meaning: its strings parse back to the outside bytes exactly, and no character that moves the
/// terminal is printed. This is the one decision for human and machine output alike.
pub fn clean_out(s: &str) -> String {
    escape(s, true, None)
}

/// A JSON document as `kob` prints it (`--json`): the encoder's text, with every hidden character
/// as a JSON `\uXXXX` escape ([`clean_out`]). Its strings are the values as they came, byte for
/// byte, so tools can compare ids and names exactly.
pub fn json(v: &(impl serde::Serialize + ?Sized)) -> String {
    clean_out(&serde_json::to_string_pretty(v).unwrap_or_default())
}

/// The same on one line (`kob watch --json`, NDJSON).
pub fn json_line(v: &(impl serde::Serialize + ?Sized)) -> String {
    clean_out(&serde_json::to_string(v).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_control_never_passes() {
        let hostile = "TOK\r  you pay 0.5 KAS\n\u{1b}[2K\u{1b}[1A\u{202e}LAM\u{2028}x\u{7f}\u{9b}31m\u{200b}";
        let c = clean(hostile);
        for bad in ['\r', '\n', '\u{1b}', '\u{202e}', '\u{2028}', '\u{7f}', '\u{9b}', '\u{200b}'] {
            assert!(!c.contains(bad), "{c:?}");
        }
        assert!(c.starts_with(r"TOK\u000d") && c.contains(r"\u001b[2K") && c.contains("\\u202e"), "{c}");
        let o = clean_out(hostile);
        assert!(!o.contains('\r') && !o.contains('\u{1b}') && !o.contains('\u{202e}') && o.contains('\n'), "{o:?}");
        assert_eq!(clean_out(&o), o, "idempotent");
        assert_eq!(clean("ABC"), "ABC");
        assert_eq!(clean("日本語 ok"), "日本語 ok", "printable text of any script stays");
        let long = "x".repeat(MAX_LEN + 10);
        assert_eq!(clean(&long).chars().count(), MAX_LEN + 1);
    }

    /// R8DF-3: a JSON document printed through the sanitiser stays valid JSON, its strings parse back to the outside
    /// bytes exactly (ESC, DEL, C1, U+202E, a zero-width joiner, a character above U+FFFF), and no raw control byte is
    /// printed. The print macros pass such a document unchanged.
    #[test]
    fn json_output_stays_exact_and_valid() {
        let outside = "TK\u{1b}[2K\u{7f}\u{9b}\u{202e}a\u{200d}b\u{e0041}\r\n";
        let doc = serde_json::json!({"ticker": outside, "error": {"message": outside}, "list": [outside]});
        for text in [json(&doc), json_line(&doc)] {
            let back: serde_json::Value = serde_json::from_str(&text).unwrap_or_else(|e| panic!("{e}: {text}"));
            assert_eq!(back, doc);
            for bad in ['\u{1b}', '\u{7f}', '\u{9b}', '\u{202e}', '\u{200d}', '\u{e0041}', '\r'] {
                assert!(!text.contains(bad), "{bad:?} in {text}");
            }
            assert_eq!(clean_out(&text), text, "the print macros leave a printed document as it is");
        }
        // what a user copies exactly passes unchanged: hex ids, txids, bech32 addresses, decimal amounts
        for v in [
            "a1".repeat(32),
            "kaspatest:qr0lr4ml9fn3chekrqmjdkergxl93l4wrk3dankcgvjq776s9wn9jhtkdksae".to_string(),
            "kaspa:qypq7lhv5c7qzwfsxjm4m29ed8t7jxvw2tq2qk8nvjcz3tulnvrhwfg7mmt0ql4".to_string(),
            "1.25 KAS, 1250base, 7070:0".to_string(),
        ] {
            assert_eq!(clean_out(&v), v);
            assert_eq!(clean(&v), v);
        }
    }
}
