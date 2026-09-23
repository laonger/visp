use clap::Parser;

fn main() -> anyhow::Result<()> {
    let cli = visp_acp::Cli::parse();
    visp_acp::init_tracing();
    visp_acp::run(cli)
}
