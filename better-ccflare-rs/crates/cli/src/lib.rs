use clap::Parser;

#[derive(Parser, Debug)]
#[command(
    name = "better-ccflare",
    version,
    about = "Load balancer proxy for Claude"
)]
pub struct Cli {
    /// Enable verbose logging
    #[arg(short, long)]
    pub verbose: bool,
}

impl Cli {
    pub fn parse_args() -> Self {
        Self::parse()
    }
}
