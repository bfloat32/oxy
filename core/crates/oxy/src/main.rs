//! `oxy` — the core without the frontend.
//!
//!   oxy query [--local] TEXT   ask the engine a question, print the rows
//!   oxy test [EXT]             run each extension's testQuery against it
//!   oxy test --cases [EXT]     run each extension's *.cases.json assertions
//!   oxy extensions             list the registry
//!
//! `--local` runs the engine in this process — how the tests exercise the
//! whole core without a daemon, and how a machine without oxyd running still
//! gets answers.

#![forbid(unsafe_code)]

mod cases;
mod cli;
mod engine_local;

const USAGE: &str = "oxy query [--local] TEXT | oxy test [--cases|--only manifest|cases] [EXT]      | oxy extensions [--coverage] | oxy send";

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match args.first().map(String::as_str) {
        Some("query") => cli::query::run(&args[1..]).await,
        Some("test") => cli::test::run(&args[1..]).await,
        Some("extensions") => cli::extensions::run(&args[1..]).await,
        Some("send") => cli::send::run().await,
        _ => {
            eprintln!("{USAGE}");
            2
        }
    };
    std::process::exit(code);
}
