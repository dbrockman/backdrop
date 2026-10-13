mod cli;
mod gui;

use backdrop_core::paths::AppPaths;
use clap::Parser as _;

fn main() -> anyhow::Result<()> {
    let args = cli::Args::parse();
    match args.command {
        None | Some(cli::Command::Gui) => {
            gui::run(AppPaths::from_env()?);
            Ok(())
        }
        Some(command) => cli::run(command),
    }
}
