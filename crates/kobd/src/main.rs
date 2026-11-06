//! `kobd`: the KOB daemon (the Executor and the Indexer roles; see `kobd::cli`).

fn main() -> std::process::ExitCode {
    kobd::cli::main()
}
