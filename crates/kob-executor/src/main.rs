//! `kobd`: the KOB daemon (the Executor and the Indexer roles; see `kob_executor::cli`).

fn main() -> std::process::ExitCode {
    kob_executor::cli::main()
}
