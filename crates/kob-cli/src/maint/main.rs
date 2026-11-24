//! `kob-maint`: commands for the people who maintain this repository. It is not shipped with `kob`.
//!
//! Run it from the source tree: `cargo run -p kob-cli --bin kob-maint -- add-program ...`.

// The prints of `kob`: every line through the one sanitiser (`text::clean_out`). This binary prints
// what it derives from an artifact or redeem file and the errors of reading it (R8FX-3).
macro_rules! println {
    () => { $crate::text::emit(false, true, "") };
    ($($t:tt)*) => { $crate::text::emit(false, true, &::std::format!($($t)*)) };
}
macro_rules! eprintln {
    () => { $crate::text::emit(true, true, "") };
    ($($t:tt)*) => { $crate::text::emit(true, true, &::std::format!($($t)*)) };
}
macro_rules! print {
    ($($t:tt)*) => { $crate::text::emit(false, false, &::std::format!($($t)*)) };
}
macro_rules! eprint {
    ($($t:tt)*) => { $crate::text::emit(true, false, &::std::format!($($t)*)) };
}

#[allow(dead_code)]
#[path = "../text.rs"]
mod text;

mod add_program;

const USAGE: &str = "\
kob-maint: maintainer commands of the KOB repository (not part of the `kob` release)

USAGE
  kob-maint add-program <ARTIFACT|REDEEM> [--name N] [--slots I/O] [--write] [...]
                                 derive a new token program's profile from its bytes (`kob-maint add-program --help`)
";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(dispatch(&args.iter().map(String::as_str).collect::<Vec<_>>()));
}

fn dispatch(args: &[&str]) -> i32 {
    match args {
        ["add-program", rest @ ..] => add_program::run(rest),
        ["--help"] | ["-h"] | ["help"] => {
            print!("{USAGE}");
            0
        }
        [] => {
            eprint!("{USAGE}");
            2
        }
        [other, ..] => {
            eprintln!("error: unknown command `{other}`\n");
            eprint!("{USAGE}");
            2
        }
    }
}
