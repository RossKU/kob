//! The help rule of every `kob` command.
//!
//! An explicit `--help` or `-h` prints the usage on stdout and exits 0. A usage printed because
//! of a mistake goes to stderr and exits 2 (the README, "Exit codes").

/// The exit code of a usage mistake.
pub const USAGE_EXIT: i32 = 2;

/// Whether `--help` or `-h` stands among the arguments.
pub fn wants_help<S: AsRef<str>>(args: &[S]) -> bool {
    args.iter().any(|a| matches!(a.as_ref(), "--help" | "-h"))
}

/// The usage text of the parts, ending in one newline.
fn text(parts: &[&str]) -> String {
    let mut s = parts.concat();
    if !s.ends_with('\n') {
        s.push('\n');
    }
    s
}

/// Print the usage on stdout, as an explicit `--help` asks, and return the exit code 0.
pub fn print_help(parts: &[&str]) -> i32 {
    print!("{}", text(parts));
    0
}

/// Print the usage on stderr, for a usage mistake, and return the exit code 2.
pub fn print_usage_error(parts: &[&str]) -> i32 {
    eprint!("{}", text(parts));
    USAGE_EXIT
}

/// The shared first step of a command.
///
/// It returns:
/// - `Some(0)` after printing the usage on stdout, when `--help` or `-h` was given.
/// - `Some(2)` after printing it on stderr, when no argument was given and `needs_args` says that
///   the command cannot run without any.
/// - `None` otherwise, and parsing goes on.
///
/// `parts` are the pieces of the usage text, printed one after the other.
pub fn help_or_usage<S: AsRef<str>>(args: &[S], parts: &[&str], needs_args: bool) -> Option<i32> {
    if wants_help(args) {
        Some(print_help(parts))
    } else if needs_args && args.is_empty() {
        Some(print_usage_error(parts))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn help_goes_to_stdout_and_a_missing_argument_is_a_usage_error() {
        let none: [&str; 0] = [];
        assert_eq!(help_or_usage(&["--help"], &["usage"], true), Some(0));
        assert_eq!(help_or_usage(&["x", "-h"], &["usage"], false), Some(0));
        assert_eq!(help_or_usage(&none, &["usage"], true), Some(2));
        assert_eq!(help_or_usage(&none, &["usage"], false), None);
        assert_eq!(help_or_usage(&["x"], &["usage"], true), None);
        assert_eq!(text(&["a\n", "b"]), "a\nb\n");
        assert_eq!(text(&["a\n"]), "a\n");
    }
}
