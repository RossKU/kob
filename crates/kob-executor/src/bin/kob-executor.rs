//! The old name of `kobd`, kept as a second binary of the same program while the workflows that build it by that name
//! (`.github/workflows`: the reproducibility check, the Android and release builds) are moved to `kobd`. Remove it then.

fn main() -> std::process::ExitCode {
    kob_executor::cli::main()
}
