//! The `kobd` binary, the KOB daemon with the Executor and Indexer roles. See `kobd::cli`.

fn main() -> std::process::ExitCode {
    kobd::cli::main()
}
