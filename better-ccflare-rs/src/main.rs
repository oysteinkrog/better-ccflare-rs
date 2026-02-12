use bccf_cli::Cli;
use bccf_core::init_logging;
use clap::Parser;

fn main() {
    let _cli = Cli::parse();
    init_logging();
    tracing::info!("better-ccflare starting");
}
