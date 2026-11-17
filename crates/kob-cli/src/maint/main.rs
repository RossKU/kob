//! `kob-maint`: commands for the people who maintain this repository. It is not shipped with `kob`.
//!
//! Run it from the source tree: `cargo run -p kob-cli --bin kob-maint -- add-program ...`.

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
