mod cli;
mod gui;

use clap::Parser as _;

fn main() -> anyhow::Result<()> {
    let args = cli::Args::parse();
    match args.command {
        None | Some(cli::Command::Gui) => {
            gui::run();
            Ok(())
        }
        Some(command) => cli::run(command),
    }
}
