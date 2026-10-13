//! Command-line interface. Commands run without starting GPUI.

use std::path::{Path, PathBuf};

use anyhow::bail;
use backdrop_core::paths::AppPaths;
use backdrop_core::settings::{LibraryStatus, Settings};
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
    /// Print the library folder, or change it.
    Library {
        #[command(subcommand)]
        command: Option<LibraryCommand>,
    },
    /// Print where Backdrop stores its settings, index, and thumbnails.
    Paths,
}

#[derive(Debug, Subcommand)]
pub enum LibraryCommand {
    /// Use DIR as the library folder.
    Set { dir: PathBuf },
}

pub fn run(command: Command) -> anyhow::Result<()> {
    let paths = AppPaths::from_env()?;
    match command {
        Command::Gui => unreachable!("the GUI is started by main"),
        Command::Library { command: None } => print_library(&paths.settings_file()),
        Command::Library {
            command: Some(LibraryCommand::Set { dir }),
        } => set_library(&paths.settings_file(), &dir),
        Command::Paths => {
            print_paths(&paths);
            Ok(())
        }
    }
}

fn print_library(settings_file: &Path) -> anyhow::Result<()> {
    match Settings::load(settings_file)?.library_status() {
        LibraryStatus::NotChosen => {
            bail!("no library folder is set; choose one with `backdrop library set <DIR>`")
        }
        LibraryStatus::Missing(dir) => bail!("library folder {} not found", dir.display()),
        LibraryStatus::Available(dir) => println!("{}", dir.display()),
    }
    Ok(())
}

fn set_library(settings_file: &Path, dir: &Path) -> anyhow::Result<()> {
    let (_, dir) = Settings::update(settings_file, |settings| settings.set_library_dir(dir))?;
    println!("{}", dir.display());
    Ok(())
}

fn print_paths(paths: &AppPaths) {
    println!("settings    {}", paths.settings_file().display());
    println!("database    {}", paths.database_file().display());
    println!("thumbnails  {}", paths.thumbnails_dir().display());
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

    #[test]
    fn library_set_takes_a_folder() {
        let args = Args::try_parse_from(["backdrop", "library", "set", "Wallpapers"]).unwrap();
        assert!(matches!(
            args.command,
            Some(Command::Library {
                command: Some(LibraryCommand::Set { dir })
            }) if dir == Path::new("Wallpapers")
        ));
    }
}
