//! Command-line interface. Commands run without starting GPUI.

use backdrop_core::paths::AppPaths;
use clap::{Parser, Subcommand};

/// Browse, organize, download, and manage wallpapers.
///
/// Run without a command to open the graphical interface.
#[derive(Debug, Parser)]
#[command(version)]
pub struct Args {
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Open the graphical interface.
    Gui,
    /// Print where Backdrop stores its settings, index, and thumbnails.
    Paths,
}

pub fn run(command: Command) -> anyhow::Result<()> {
    match command {
        Command::Gui => unreachable!("the GUI is started by main"),
        Command::Paths => print_paths(),
    }
}

fn print_paths() -> anyhow::Result<()> {
    let paths = AppPaths::from_env()?;
    println!("settings    {}", paths.settings_file().display());
    println!("database    {}", paths.database_file().display());
    println!("thumbnails  {}", paths.thumbnails_dir().display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory as _;

    use super::*;

    #[test]
    fn cli_definition_is_valid() {
        Args::command().debug_assert();
    }

    #[test]
    fn no_command_opens_the_gui() {
        let args = Args::try_parse_from(["backdrop"]).unwrap();
        assert!(args.command.is_none());
    }
}
