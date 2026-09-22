//! `oxy-agent` — the Rust port of `bin/oxy-agent`, the `do:` keyword's
//! backend. Same subcommands, same socket, same rows: interchangeable with
//! the script, which stays shipped as the fallback.

#![forbid(unsafe_code)]

#[path = "../agent/mod.rs"]
mod agent;

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(agent::main(argv).await);
}
