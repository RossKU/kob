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
            out.push_str(&format!("\\u{{{:x}}}", c as u32));
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
    static CAPTURE: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/// Runs `f` and returns what it printed on this thread, stdout and stderr together (tests).
#[cfg(test)]
pub fn capture<T>(f: impl FnOnce() -> T) -> (T, String) {
    CAPTURE.with(|c| *c.borrow_mut() = Some(String::new()));
    let r = f();
    (r, CAPTURE.with(|c| c.borrow_mut().take().unwrap_or_default()))
}

/// The one way the crate prints (the macros of `main.rs`): `s` through [`clean_out`], to stderr
/// with `err`, with a line break with `line`.
pub fn emit(err: bool, line: bool, s: &str) {
    let s = clean_out(s);
    #[cfg(test)]
    CAPTURE.with(|c| {
        if let Some(b) = c.borrow_mut().as_mut() {
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
pub fn clean_out(s: &str) -> String {
    escape(s, true, None)
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
        assert!(c.starts_with("TOK\\u{d}") && c.contains("\\u{1b}[2K") && c.contains("\\u{202e}"), "{c}");
        let o = clean_out(hostile);
        assert!(!o.contains('\r') && !o.contains('\u{1b}') && !o.contains('\u{202e}') && o.contains('\n'), "{o:?}");
        assert_eq!(clean_out(&o), o, "idempotent");
        assert_eq!(clean("ABC"), "ABC");
        assert_eq!(clean("日本語 ok"), "日本語 ok", "printable text of any script stays");
        let long = "x".repeat(MAX_LEN + 10);
        assert_eq!(clean(&long).chars().count(), MAX_LEN + 1);
    }
}
